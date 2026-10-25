//! A typed Matrix client for the **outbound** legs of the Client-Server API.
//!
//! Everything above (and in the sibling modules) is payload-in/payload-out: no
//! sockets, no clocks, no retries. This module is the other half — the part that
//! actually talks to a homeserver, kept honest by a [`Transport`] seam and the
//! three properties a bot needs:
//!
//! * **A retried send never posts twice.** Every event send is keyed by a
//!   [`TxnId`]; Matrix de-duplicates `PUT /rooms/{roomId}/send/{eventType}/{txnId}`
//!   on that key, so an internal retry — or a caller repeating the send with the
//!   id from [`OutboundError::txn_id`] — produces one event, not two.
//! * **Uncertain writes are reported as uncertain.** [`OutboundError::Ambiguous`]
//!   means the event may be in the room. A non-idempotent request (media upload)
//!   is never retried after a `5xx` or a read timeout; see [`retry`] for the full
//!   table.
//! * **Rate limits are obeyed.** A `429` with `retry_after_ms` is honoured for
//!   exactly that long (up to [`RetryPolicy::max_rate_limit_wait`]), not for a
//!   backoff the client invented.
//!
//! # What is covered
//!
//! | leg | endpoint | idempotency |
//! |-----|----------|-------------|
//! | [`MatrixClient::send_text`] | `PUT /_matrix/client/v3/rooms/{roomId}/send/m.room.message/{txnId}` | transaction id |
//! | [`MatrixClient::send_event`] | same, any event type | transaction id |
//! | [`MatrixClient::send_reaction`] | `m.reaction` (`m.annotation`) | transaction id |
//! | [`MatrixClient::redact`] | `PUT /rooms/{roomId}/redact/{eventId}/{txnId}` | transaction id |
//! | [`MatrixClient::send_typing`] | `PUT /rooms/{roomId}/typing/{userId}` | state-setting |
//! | [`MatrixClient::send_read_receipt`] | `POST /rooms/{roomId}/receipt/m.read/{eventId}` | state-setting |
//! | [`MatrixClient::upload_media`] | `POST /_matrix/media/v3/upload` | **none** |
//! | [`MatrixClient::send_media`] | upload, then `m.image`/`m.video`/`m.audio`/`m.file` | upload: none, send: transaction id |
//! | [`MatrixClient::whoami`] | `GET /_matrix/client/v3/account/whoami` | read |
//!
//! # What is deliberately not covered
//!
//! Room creation, joining, invites, state events, sync/long-polling, end-to-end
//! encryption, threaded sends and edits. Each is a different consumer: a bot that
//! only writes into rooms it already joined does not need them, and a client that
//! claims them without the tests to match would be the stub this crate exists to
//! avoid.
//!
//! # Quick start
//!
//! ```
//! use std::sync::Arc;
//! use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // The transport is the only place HTTP happens; swap `StubTransport` for
//! // `ReqwestTransport` in production.
//! let transport = Arc::new(StubTransport::new());
//! transport.push_json(200, r#"{"event_id":"$abc"}"#);
//!
//! let client = MatrixClient::new(
//!     transport.clone(),
//!     ClientConfig::new("https://matrix.example.org")?,
//!     "syt_access_token",
//! )?;
//!
//! let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
//! let outcome = runtime.block_on(client.send_text("!room:example.org", "Hallo"))?;
//! assert_eq!(outcome.event_id, "$abc");
//! assert_eq!(outcome.attempts, 1);
//!
//! let request = transport.last_request().expect("one request");
//! assert_eq!(request.method, messenger_webhook::matrix_out::transport::Method::Put);
//! assert!(request.url.ends_with(
//!     &format!("/rooms/%21room%3Aexample.org/send/m.room.message/{}", outcome.txn_id)
//! ));
//! # Ok(())
//! # }
//! ```

pub mod error;
mod http;
mod ordering;
pub mod retry;
pub mod testing;
pub mod transport;
pub mod types;

use std::fmt::{self, Write as _};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

pub use error::{OutboundError, OutboundResult};
pub use http::{ReqwestTransport, DEFAULT_REQUEST_TIMEOUT};
pub use ordering::GATE_CAPACITY;
pub use retry::{
    Operation, RetryPolicy, Verdict, DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_ATTEMPTS,
    DEFAULT_MAX_DELAY_MS, DEFAULT_MAX_RATE_LIMIT_WAIT_MS,
};
pub use testing::{RecordedRequest, RecordingSleeper, StubTransport};
pub use transport::Transport;
pub use types::{
    MediaKind, MediaUpload, MxcUri, SendOutcome, TxnId, DEFAULT_MAX_UPLOAD_BYTES,
    DEFAULT_TYPING_TIMEOUT_MS, MAX_CONTENT_BYTES, MAX_EVENT_TYPE_BYTES, MAX_ID_BYTES,
    MAX_MXC_URI_BYTES, MAX_TXN_ID_BYTES,
};
// `MAX_TXN_ID_BYTES` and friends are declared in `types` and re-exported above.

use crate::matrix_out::ordering::RoomGates;
use crate::matrix_out::transport::{Body, BoxFuture, Method, Request, Response, TransportError};
use crate::Secret;

/// How the client waits between attempts.
///
/// The default ([`TokioSleeper`]) uses `tokio::time::sleep`, which needs a Tokio
/// runtime with the timer enabled. Implement this trait to drive the client on
/// another executor, or to record the schedule in a test
/// ([`RecordingSleeper`]).
///
/// ```
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::Sleeper;
/// use messenger_webhook::matrix_out::transport::BoxFuture;
///
/// /// Never waits.
/// #[derive(Debug)]
/// struct Immediate;
///
/// impl Sleeper for Immediate {
///     fn sleep(&self, _delay: Duration) -> BoxFuture<'_, ()> {
///         Box::pin(async {})
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// runtime.block_on(Immediate.sleep(Duration::from_secs(1)));
/// # Ok(())
/// # }
/// ```
pub trait Sleeper: Send + Sync + fmt::Debug {
    /// Waits for `delay`, or returns immediately in a test double.
    fn sleep(&self, delay: Duration) -> BoxFuture<'_, ()>;
}

/// The default [`Sleeper`], backed by `tokio::time::sleep`.
///
/// ```
/// use messenger_webhook::matrix_out::TokioSleeper;
///
/// assert_eq!(format!("{:?}", TokioSleeper), "TokioSleeper");
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, delay: Duration) -> BoxFuture<'_, ()> {
        Box::pin(async move { tokio::time::sleep(delay).await })
    }
}

/// Everything the client needs that is not the access token.
///
/// Built with [`ClientConfig::new`], which validates the homeserver URL, then
/// refined with the `with_*` methods. The access token is deliberately not part
/// of it: a config is the sort of thing that gets logged, a credential is not.
///
/// ```
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::{ClientConfig, RetryPolicy};
///
/// let config = ClientConfig::new("https://matrix.example.org/")?
///     .with_user_id("@bot:example.org")
///     .with_max_upload_bytes(10_000_000)
///     .with_per_room_ordering(false)
///     .with_retry(RetryPolicy {
///         max_attempts: 2,
///         ..RetryPolicy::default()
///     });
///
/// // The trailing slash is normalised away, so paths never double up.
/// assert_eq!(config.homeserver(), "https://matrix.example.org");
/// assert_eq!(config.user_id(), Some("@bot:example.org"));
/// assert_eq!(config.max_upload_bytes(), 10_000_000);
/// assert!(!config.per_room_ordering());
/// assert_eq!(config.retry().max_attempts, 2);
/// assert_eq!(Duration::from_millis(500), RetryPolicy::default().base_delay);
/// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
/// ```
#[derive(Clone)]
pub struct ClientConfig {
    homeserver: String,
    user_id: Option<String>,
    retry: RetryPolicy,
    max_upload_bytes: u64,
    per_room_ordering: bool,
    sleeper: Arc<dyn Sleeper>,
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("homeserver", &self.homeserver)
            .field("user_id", &self.user_id)
            .field("retry", &self.retry)
            .field("max_upload_bytes", &self.max_upload_bytes)
            .field("per_room_ordering", &self.per_room_ordering)
            .field("sleeper", &self.sleeper)
            .finish()
    }
}

impl ClientConfig {
    /// A configuration for `homeserver`, with the default retry policy, the
    /// default media ceiling and per-room ordering on.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidConfig`] when `homeserver` does not start with
    /// `http://` or `https://`, or carries whitespace or control characters.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// assert!(ClientConfig::new("https://matrix.example.org").is_ok());
    /// assert!(ClientConfig::new("matrix.example.org").is_err());
    /// assert!(ClientConfig::new("ftp://matrix.example.org").is_err());
    /// ```
    pub fn new(homeserver: impl Into<String>) -> OutboundResult<Self> {
        let raw = homeserver.into();
        let trimmed = raw.trim();
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err(OutboundError::InvalidConfig {
                reason: format!(
                    "the homeserver URL {trimmed:?} needs an http:// or https:// scheme"
                ),
            });
        }
        if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(OutboundError::InvalidConfig {
                reason: "the homeserver URL contains whitespace or a control character".to_owned(),
            });
        }
        let base = trimmed.trim_end_matches('/');
        if base.is_empty() || base.ends_with(':') {
            return Err(OutboundError::InvalidConfig {
                reason: "the homeserver URL has no host".to_owned(),
            });
        }
        Ok(Self {
            homeserver: base.to_owned(),
            user_id: None,
            retry: RetryPolicy::default(),
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            per_room_ordering: true,
            sleeper: Arc::new(TokioSleeper),
        })
    }

    /// Sets the account's user id — required by the typing leg, which addresses
    /// the marker with it.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// let config = ClientConfig::new("https://m.example")?.with_user_id("@bot:m.example");
    /// assert_eq!(config.user_id(), Some("@bot:m.example"));
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Replaces the retry policy.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{ClientConfig, RetryPolicy};
    ///
    /// let config = ClientConfig::new("https://m.example")?
    ///     .with_retry(RetryPolicy { max_attempts: 1, ..RetryPolicy::default() });
    /// assert_eq!(config.retry().max_attempts, 1);
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Sets the pre-flight media ceiling. Read it from the homeserver's
    /// `m.upload.size` capability.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// let config = ClientConfig::new("https://m.example")?.with_max_upload_bytes(1_000);
    /// assert_eq!(config.max_upload_bytes(), 1_000);
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn with_max_upload_bytes(mut self, bytes: u64) -> Self {
        self.max_upload_bytes = bytes;
        self
    }

    /// Turns per-room serialisation on or off.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// let config = ClientConfig::new("https://m.example")?.with_per_room_ordering(false);
    /// assert!(!config.per_room_ordering());
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn with_per_room_ordering(mut self, ordered: bool) -> Self {
        self.per_room_ordering = ordered;
        self
    }

    /// Replaces the sleeper — the seam for a non-Tokio runtime or a fake clock.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, RecordingSleeper};
    ///
    /// let config = ClientConfig::new("https://m.example")?
    ///     .with_sleeper(Arc::new(RecordingSleeper::new()));
    /// assert!(format!("{config:?}").contains("RecordingSleeper"));
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// The homeserver base URL, without a trailing slash.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// assert_eq!(ClientConfig::new("https://m.example/")?.homeserver(), "https://m.example");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn homeserver(&self) -> &str {
        &self.homeserver
    }

    /// The configured user id, if any.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// assert_eq!(ClientConfig::new("https://m.example")?.user_id(), None);
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }

    /// The retry policy in force.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// assert_eq!(ClientConfig::new("https://m.example")?.retry().max_attempts, 3);
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub const fn retry(&self) -> RetryPolicy {
        self.retry
    }

    /// The media ceiling in bytes.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{ClientConfig, DEFAULT_MAX_UPLOAD_BYTES};
    ///
    /// assert_eq!(ClientConfig::new("https://m.example")?.max_upload_bytes(), DEFAULT_MAX_UPLOAD_BYTES);
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub const fn max_upload_bytes(&self) -> u64 {
        self.max_upload_bytes
    }

    /// Whether sends into one room are serialised.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// assert!(ClientConfig::new("https://m.example")?.per_room_ordering());
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub const fn per_room_ordering(&self) -> bool {
        self.per_room_ordering
    }
}

/// A Matrix account's outbound client: one access token, one homeserver, one
/// [`Transport`].
///
/// Stateless per call: the only shared state is the per-room gate map, so a
/// client can be cloned into an `Arc` and used from many tasks at once.
///
/// ```
/// use std::sync::Arc;
/// use messenger_webhook::matrix_out::{
///     ClientConfig, MatrixClient, OutboundError, StubTransport,
/// };
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let transport = Arc::new(StubTransport::new());
/// // An empty token is a misconfiguration, not a request that fails later.
/// let refused = MatrixClient::new(
///     transport.clone(),
///     ClientConfig::new("https://m.example")?,
///     "",
/// );
/// assert!(matches!(refused, Err(OutboundError::InvalidConfig { .. })));
///
/// // The access token is a credential: it never appears in a log line.
/// let client = MatrixClient::new(
///     transport,
///     ClientConfig::new("https://m.example")?,
///     "syt_secret_token",
/// )?;
/// assert!(!format!("{client:?}").contains("syt_secret_token"));
/// assert_eq!(client.config().homeserver(), "https://m.example");
/// # Ok(())
/// # }
/// ```
pub struct MatrixClient {
    transport: Arc<dyn Transport>,
    config: ClientConfig,
    token: Secret,
    gates: RoomGates,
}

impl fmt::Debug for MatrixClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MatrixClient")
            .field("transport", &self.transport)
            .field("config", &self.config)
            .field("token", &self.token)
            .field("gated_rooms", &self.gates.len())
            .finish()
    }
}

/// What the client does per attempt: the request to build, and how the outcome
/// is allowed to be retried.
struct Call<'a> {
    /// Which leg this is; decides the request and the idempotency class.
    kind: CallKind<'a>,
}

/// The leg, with everything the request builder needs.
enum CallKind<'a> {
    /// A transaction-keyed event send.
    Event {
        room_id: &'a str,
        event_type: &'a str,
        txn: &'a TxnId,
        content: Vec<u8>,
    },
    /// A transaction-keyed redaction.
    Redact {
        room_id: &'a str,
        event_id: &'a str,
        txn: &'a TxnId,
        reason: Option<&'a str>,
    },
    /// The typing marker.
    Typing {
        room_id: &'a str,
        user_id: &'a str,
        typing: bool,
        timeout_ms: u32,
    },
    /// A read receipt.
    Receipt { room_id: &'a str, event_id: &'a str },
    /// The connection probe.
    Whoami,
    /// A media upload.
    Upload { media: &'a MediaUpload<'a> },
}

impl Call<'_> {
    /// The room this call belongs to, which decides which gate it takes.
    fn room_id(&self) -> Option<&str> {
        match &self.kind {
            CallKind::Event { room_id, .. }
            | CallKind::Redact { room_id, .. }
            | CallKind::Typing { room_id, .. }
            | CallKind::Receipt { room_id, .. } => Some(room_id),
            CallKind::Whoami | CallKind::Upload { .. } => None,
        }
    }
}

/// A decoded value plus the number of attempts it took.
struct Attempted<T> {
    value: T,
    attempts: u32,
}

/// A decoded event id, wrapped with the transaction id that produced it.
fn send_outcome(attempted: Attempted<String>, txn_id: TxnId) -> SendOutcome {
    SendOutcome {
        event_id: attempted.value,
        txn_id,
        attempts: attempted.attempts,
    }
}

impl MatrixClient {
    /// Builds a client for one homeserver account.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidConfig`] when the access token is empty, the
    /// retry policy is unusable (`max_attempts == 0`, or a `base_delay` longer
    /// than `max_delay`), the media ceiling is `0`, or a configured user id is
    /// not a Matrix user id.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MatrixClient::new(
    ///     Arc::new(StubTransport::new()),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt_token",
    /// )?;
    /// assert_eq!(client.config().homeserver(), "https://m.example");
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(
        transport: Arc<dyn Transport>,
        config: ClientConfig,
        access_token: impl Into<String>,
    ) -> OutboundResult<Self> {
        let token = Secret::new(access_token);
        if token.is_empty() {
            return Err(OutboundError::InvalidConfig {
                reason: "the access token is empty (Matrix would answer 401 M_MISSING_TOKEN)"
                    .to_owned(),
            });
        }
        if let Err(reason) = config.retry.validate() {
            return Err(OutboundError::InvalidConfig { reason });
        }
        if config.max_upload_bytes == 0 {
            return Err(OutboundError::InvalidConfig {
                reason: "max_upload_bytes is 0, which refuses every upload".to_owned(),
            });
        }
        if let Some(user_id) = config.user_id.as_deref() {
            types::validate_user_id(user_id).map_err(|error| OutboundError::InvalidConfig {
                reason: error.to_string(),
            })?;
        }
        Ok(Self {
            transport,
            config,
            token,
            gates: RoomGates::new(GATE_CAPACITY),
        })
    }

    /// The configuration this client was built with.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MatrixClient::new(
    ///     Arc::new(StubTransport::new()),
    ///     ClientConfig::new("https://m.example")?.with_user_id("@bot:m.example"),
    ///     "syt",
    /// )?;
    /// assert_eq!(client.config().user_id(), Some("@bot:m.example"));
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Sends a text message (`m.room.message` / `m.text`) into a room.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidRequest`] for an empty body or an unusable room
    /// id, [`OutboundError::MessageTooLong`] above the event ceiling, plus every
    /// delivery error [`send_text_with_txn`](Self::send_text_with_txn) reports.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_json(200, r#"{"event_id":"$abc"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let outcome = runtime.block_on(client.send_text("!room:m.example", "Hallo"))?;
    /// assert_eq!(outcome.event_id, "$abc");
    /// assert_eq!(
    ///     transport.last_request().expect("sent").json()?,
    ///     serde_json::json!({"msgtype": "m.text", "body": "Hallo"}),
    /// );
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_text(&self, room_id: &str, body: &str) -> OutboundResult<SendOutcome> {
        self.send_text_with_txn(TxnId::generate(), room_id, body)
            .await
    }

    /// Sends a text message under a caller-chosen transaction id.
    ///
    /// Use this to repeat a send whose outcome was unclear: the homeserver
    /// de-duplicates on the transaction id and answers with the original event
    /// id, so the room never receives two copies.
    ///
    /// # Errors
    ///
    /// As [`send_text`](Self::send_text).
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport, TxnId};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport
    ///     .push_json(200, r#"{"event_id":"$abc"}"#)
    ///     .push_json(200, r#"{"event_id":"$abc"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let txn = TxnId::new("order-42")?;
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let first = runtime.block_on(client.send_text_with_txn(txn.clone(), "!r:m.example", "Hallo"))?;
    /// let again = runtime.block_on(client.send_text_with_txn(txn, "!r:m.example", "Hallo"))?;
    /// // Same transaction id, same event: the second call is a no-op at the homeserver.
    /// assert_eq!(first.event_id, again.event_id);
    /// assert_eq!(transport.requests()[0].url, transport.requests()[1].url);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_text_with_txn(
        &self,
        txn_id: TxnId,
        room_id: &str,
        body: &str,
    ) -> OutboundResult<SendOutcome> {
        if body.is_empty() {
            return Err(OutboundError::InvalidRequest {
                reason: "the message body is empty".to_owned(),
            });
        }
        self.send_event_with_txn(
            txn_id,
            room_id,
            "m.room.message",
            json!({ "msgtype": "m.text", "body": body }),
        )
        .await
    }

    /// Sends an arbitrary event type with client-generated content.
    ///
    /// The escape hatch for event types this crate does not model — a
    /// `com.example.deploy` notice, an `m.notice`, a custom relation. The
    /// content is sent verbatim; only the event ceiling is enforced.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidRequest`] for an unusable room id or event type,
    /// [`OutboundError::MessageTooLong`] above [`MAX_CONTENT_BYTES`], plus every
    /// delivery error [`send_event_with_txn`](Self::send_event_with_txn) reports.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_json(200, r#"{"event_id":"$notice"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let outcome = runtime.block_on(client.send_event(
    ///     "!r:m.example",
    ///     "com.example.deploy",
    ///     serde_json::json!({"status": "green"}),
    /// ))?;
    /// assert_eq!(outcome.event_id, "$notice");
    /// assert!(transport.last_request().expect("sent").url.contains("com.example.deploy"));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_event(
        &self,
        room_id: &str,
        event_type: &str,
        content: Value,
    ) -> OutboundResult<SendOutcome> {
        self.send_event_with_txn(TxnId::generate(), room_id, event_type, content)
            .await
    }

    /// Sends an arbitrary event type under a caller-chosen transaction id.
    ///
    /// # Errors
    ///
    /// As [`send_event`](Self::send_event).
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport, TxnId};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_json(200, r#"{"event_id":"$1"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let outcome = runtime.block_on(client.send_event_with_txn(
    ///     TxnId::new("deploy-7")?,
    ///     "!r:m.example",
    ///     "m.notice",
    ///     serde_json::json!({"msgtype": "m.notice", "body": "done"}),
    /// ))?;
    /// assert_eq!(outcome.txn_id.as_str(), "deploy-7");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_event_with_txn(
        &self,
        txn_id: TxnId,
        room_id: &str,
        event_type: &str,
        content: Value,
    ) -> OutboundResult<SendOutcome> {
        types::validate_room_id(room_id)?;
        types::validate_event_type(event_type)?;
        let content = check_content(&content)?;
        let call = Call {
            kind: CallKind::Event {
                room_id,
                event_type,
                txn: &txn_id,
                content,
            },
        };
        let attempted = self
            .execute(call, Operation::Idempotent, Some(&txn_id), decode_event_id)
            .await?;
        Ok(send_outcome(attempted, txn_id))
    }

    /// Reacts to an event (`m.reaction` carrying an `m.annotation`).
    ///
    /// Removing a reaction is a redaction, not an empty key — see
    /// [`redact`](Self::redact).
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidRequest`] for an empty key, an unusable room or
    /// event id, [`OutboundError::MessageTooLong`] above the event ceiling, plus
    /// every delivery error.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_json(200, r#"{"event_id":"$reaction"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// runtime.block_on(client.send_reaction("!r:m.example", "$abc", "👍"))?;
    /// let body = transport.last_request().expect("sent").json()?;
    /// assert_eq!(body["m.relates_to"]["rel_type"], "m.annotation");
    /// assert_eq!(body["m.relates_to"]["event_id"], "$abc");
    /// assert_eq!(body["m.relates_to"]["key"], "👍");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_reaction(
        &self,
        room_id: &str,
        event_id: &str,
        key: &str,
    ) -> OutboundResult<SendOutcome> {
        types::validate_room_id(room_id)?;
        types::validate_event_id(event_id)?;
        if key.is_empty() {
            return Err(OutboundError::InvalidRequest {
                reason: "the reaction key is empty (remove a reaction with `redact`)".to_owned(),
            });
        }
        if key.chars().any(char::is_control) {
            return Err(OutboundError::InvalidRequest {
                reason: "the reaction key contains a control character".to_owned(),
            });
        }
        let txn_id = TxnId::generate();
        let content = json!({
            "m.relates_to": {
                "rel_type": "m.annotation",
                "event_id": event_id,
                "key": key,
            }
        });
        let content = check_content(&content)?;
        let call = Call {
            kind: CallKind::Event {
                room_id,
                event_type: "m.reaction",
                txn: &txn_id,
                content,
            },
        };
        let attempted = self
            .execute(call, Operation::Idempotent, Some(&txn_id), decode_event_id)
            .await?;
        Ok(send_outcome(attempted, txn_id))
    }

    /// Redacts (deletes) an event, optionally with a reason.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidRequest`] for an unusable room or event id, plus
    /// every delivery error. A reason over the event ceiling is reported as
    /// [`OutboundError::MessageTooLong`].
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_json(200, r#"{"event_id":"$redaction"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let outcome = runtime.block_on(client.redact("!r:m.example", "$abc", Some("typo")))?;
    /// assert_eq!(outcome.event_id, "$redaction");
    /// assert!(transport.last_request().expect("sent").url.contains("/redact/%24abc/"));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn redact(
        &self,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> OutboundResult<SendOutcome> {
        types::validate_room_id(room_id)?;
        types::validate_event_id(event_id)?;
        let txn_id = TxnId::generate();
        let call = Call {
            kind: CallKind::Redact {
                room_id,
                event_id,
                txn: &txn_id,
                reason,
            },
        };
        let attempted = self
            .execute(call, Operation::Idempotent, Some(&txn_id), decode_event_id)
            .await?;
        Ok(send_outcome(attempted, txn_id))
    }

    /// Shows the typing marker for the configured user.
    ///
    /// The marker decays on the homeserver; call this again to keep it up.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidConfig`] when no user id is configured,
    /// [`OutboundError::InvalidRequest`] for a zero timeout or an unusable room
    /// id, plus every delivery error.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport, DEFAULT_TYPING_TIMEOUT_MS};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_status(200);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?.with_user_id("@bot:m.example"),
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// runtime.block_on(client.send_typing("!r:m.example", DEFAULT_TYPING_TIMEOUT_MS))?;
    /// let request = transport.last_request().expect("sent");
    /// assert!(request.url.ends_with("/typing/%40bot%3Am.example"));
    /// assert_eq!(request.json()?["typing"], true);
    /// assert_eq!(request.json()?["timeout"], 4_000);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_typing(&self, room_id: &str, timeout_ms: u32) -> OutboundResult<()> {
        if timeout_ms == 0 {
            return Err(OutboundError::InvalidRequest {
                reason: "a typing timeout of 0 would show nothing (use `stop_typing`)".to_owned(),
            });
        }
        self.typing(room_id, true, timeout_ms).await
    }

    /// Clears the typing marker for the configured user.
    ///
    /// # Errors
    ///
    /// As [`send_typing`](Self::send_typing).
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_status(200);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?.with_user_id("@bot:m.example"),
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// runtime.block_on(client.stop_typing("!r:m.example"))?;
    /// assert_eq!(transport.last_request().expect("sent").json()?, serde_json::json!({"typing": false}));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn stop_typing(&self, room_id: &str) -> OutboundResult<()> {
        self.typing(room_id, false, 0).await
    }

    /// The shared body of [`send_typing`](Self::send_typing) and
    /// [`stop_typing`](Self::stop_typing).
    async fn typing(&self, room_id: &str, typing: bool, timeout_ms: u32) -> OutboundResult<()> {
        types::validate_room_id(room_id)?;
        let Some(user_id) = self.config.user_id() else {
            return Err(OutboundError::InvalidConfig {
                reason: "the typing leg addresses the marker with the account user id; set it with `ClientConfig::with_user_id`".to_owned(),
            });
        };
        types::validate_user_id(user_id)?;
        let call = Call {
            kind: CallKind::Typing {
                room_id,
                user_id,
                typing,
                timeout_ms,
            },
        };
        self.execute(call, Operation::Idempotent, None, decode_unit)
            .await
            .map(|_| ())
    }

    /// Posts a read receipt on an event: the outbound half of Matrix read
    /// receipts.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidRequest`] for an unusable room or event id, plus
    /// every delivery error.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_status(200);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// runtime.block_on(client.send_read_receipt("!r:m.example", "$abc"))?;
    /// let request = transport.last_request().expect("sent");
    /// assert!(request.url.contains("/receipt/m.read/%24abc"));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_read_receipt(&self, room_id: &str, event_id: &str) -> OutboundResult<()> {
        types::validate_room_id(room_id)?;
        types::validate_event_id(event_id)?;
        let call = Call {
            kind: CallKind::Receipt { room_id, event_id },
        };
        self.execute(call, Operation::Idempotent, None, decode_unit)
            .await
            .map(|_| ())
    }

    /// Uploads media and returns the `mxc://` URI that references it.
    ///
    /// The upload carries **no** idempotency key: a repeated upload stores the
    /// bytes twice. It is therefore never retried after a `5xx` or a timeout —
    /// those come back as [`OutboundError::Ambiguous`] — only after a `429` or a
    /// connection that was never established.
    ///
    /// # Errors
    ///
    /// [`OutboundError::MediaTooLarge`] above the configured ceiling,
    /// [`OutboundError::InvalidRequest`] for empty bytes or an unusable content
    /// type or file name, plus every delivery error.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, MediaUpload, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_json(200, r#"{"content_uri":"mxc://m.example/abc"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let upload = MediaUpload::new(b"\x89PNG", "image/png").with_filename("cat.png");
    /// let uri = runtime.block_on(client.upload_media(&upload))?;
    /// assert_eq!(uri.as_str(), "mxc://m.example/abc");
    ///
    /// let request = transport.last_request().expect("sent");
    /// assert!(request.url.contains("/_matrix/media/v3/upload?filename=cat.png"));
    /// assert_eq!(request.header("content-type"), Some("image/png"));
    /// assert_eq!(request.body, b"\x89PNG");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn upload_media(&self, media: &MediaUpload<'_>) -> OutboundResult<MxcUri> {
        types::validate_media(media, self.config.max_upload_bytes)?;
        let call = Call {
            kind: CallKind::Upload { media },
        };
        self.execute(call, Operation::NonIdempotent, None, decode_content_uri)
            .await
            .map(|attempted| attempted.value)
    }

    /// Uploads media and sends it into a room as one message.
    ///
    /// Two legs, two delivery contracts: the upload is non-idempotent (an
    /// ambiguous upload leaves orphaned bytes but no message), the send is keyed
    /// by a transaction id. An upload that fails ambiguously stops the call
    /// before anything is sent.
    ///
    /// # Errors
    ///
    /// As [`upload_media`](Self::upload_media) for the first leg, as
    /// [`send_event_with_txn`](Self::send_event_with_txn) for the second.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{
    ///     ClientConfig, MatrixClient, MediaKind, MediaUpload, StubTransport,
    /// };
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport
    ///     .push_json(200, r#"{"content_uri":"mxc://m.example/abc"}"#)
    ///     .push_json(200, r#"{"event_id":"$img"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let upload = MediaUpload::new(b"\x89PNG", "image/png")
    ///     .with_filename("cat.png")
    ///     .with_kind(MediaKind::Image);
    /// let outcome = runtime.block_on(client.send_media("!r:m.example", &upload, Some("meine Katze")))?;
    /// assert_eq!(outcome.event_id, "$img");
    ///
    /// let message = transport.last_request().expect("send").json()?;
    /// assert_eq!(message["msgtype"], "m.image");
    /// assert_eq!(message["url"], "mxc://m.example/abc");
    /// assert_eq!(message["body"], "meine Katze");
    /// assert_eq!(message["info"]["mimetype"], "image/png");
    /// assert_eq!(message["info"]["size"], 4);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_media(
        &self,
        room_id: &str,
        media: &MediaUpload<'_>,
        caption: Option<&str>,
    ) -> OutboundResult<SendOutcome> {
        types::validate_room_id(room_id)?;
        let uri = self.upload_media(media).await?;
        let body = caption
            .or(media.filename)
            .unwrap_or_else(|| media.kind.msgtype());
        let content = json!({
            "msgtype": media.kind.msgtype(),
            "body": body,
            "url": uri.as_str(),
            "info": {
                "mimetype": media.content_type,
                "size": media.data.len(),
            },
        });
        self.send_event(room_id, "m.room.message", content).await
    }

    /// The connection probe: resolves the authenticated user id.
    ///
    /// # Errors
    ///
    /// [`OutboundError::Decode`] when the body is not the `whoami` shape, plus
    /// every delivery error.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_json(200, r#"{"user_id":"@bot:m.example","is_guest":false}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// assert_eq!(runtime.block_on(client.whoami())?, "@bot:m.example");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn whoami(&self) -> OutboundResult<String> {
        let call = Call {
            kind: CallKind::Whoami,
        };
        self.execute(call, Operation::Read, None, decode_whoami)
            .await
            .map(|attempted| attempted.value)
    }

    /// Runs one call under the retry policy, holding the room gate when the call
    /// belongs to a room.
    async fn execute<T>(
        &self,
        call: Call<'_>,
        operation: Operation,
        txn: Option<&TxnId>,
        decode: fn(&Response) -> OutboundResult<T>,
    ) -> OutboundResult<Attempted<T>> {
        let _gate = match call.room_id() {
            Some(room_id) if self.config.per_room_ordering => self.gates.acquire(room_id).await,
            _ => None,
        };
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let request = self.build(&call);
            match self.transport.send(request).await {
                Ok(response) => match self.config.retry.classify(attempt, &response, operation) {
                    Verdict::Success => {
                        return match decode(&response) {
                            Ok(value) => Ok(Attempted {
                                value,
                                attempts: attempt,
                            }),
                            Err(error) => Err(Self::undecodable(error, operation, txn, attempt)),
                        };
                    }
                    Verdict::Retry { delay } => self.config.sleeper.sleep(delay).await,
                    Verdict::Rejected => return Err(rejection(&response)),
                    Verdict::Exhausted | Verdict::Ambiguous => {
                        return Err(http_failure(&response, operation, txn, attempt));
                    }
                },
                Err(error) => {
                    match self
                        .config
                        .retry
                        .classify_transport(attempt, &error, operation)
                    {
                        Verdict::Retry { delay } => self.config.sleeper.sleep(delay).await,
                        _ => return Err(transport_failure(&error, operation, txn, attempt)),
                    }
                }
            }
        }
    }

    /// Builds the request for one attempt.
    fn build<'a>(&self, call: &'a Call<'a>) -> Request<'a> {
        let base = &self.config.homeserver;
        match &call.kind {
            CallKind::Event {
                room_id,
                event_type,
                txn,
                content,
            } => {
                let url = format!(
                    "{base}/_matrix/client/v3/rooms/{}/send/{}/{}",
                    encode_path_segment(room_id),
                    encode_path_segment(event_type),
                    encode_path_segment(txn.as_str()),
                );
                self.authorized(Method::Put, url)
                    .with_header("content-type", "application/json")
                    .with_body(Body::Json(content.clone()))
            }
            CallKind::Redact {
                room_id,
                event_id,
                txn,
                reason,
            } => {
                let url = format!(
                    "{base}/_matrix/client/v3/rooms/{}/redact/{}/{}",
                    encode_path_segment(room_id),
                    encode_path_segment(event_id),
                    encode_path_segment(txn.as_str()),
                );
                let mut body = serde_json::Map::new();
                if let Some(reason) = reason {
                    body.insert("reason".to_owned(), Value::String((*reason).to_owned()));
                }
                self.authorized(Method::Put, url)
                    .with_header("content-type", "application/json")
                    .with_body(Body::Json(json_bytes(&Value::Object(body))))
            }
            CallKind::Typing {
                room_id,
                user_id,
                typing,
                timeout_ms,
            } => {
                let url = format!(
                    "{base}/_matrix/client/v3/rooms/{}/typing/{}",
                    encode_path_segment(room_id),
                    encode_path_segment(user_id),
                );
                let content = if *typing {
                    json!({ "typing": true, "timeout": timeout_ms })
                } else {
                    json!({ "typing": false })
                };
                self.authorized(Method::Put, url)
                    .with_header("content-type", "application/json")
                    .with_body(Body::Json(json_bytes(&content)))
            }
            CallKind::Receipt { room_id, event_id } => {
                let url = format!(
                    "{base}/_matrix/client/v3/rooms/{}/receipt/m.read/{}",
                    encode_path_segment(room_id),
                    encode_path_segment(event_id),
                );
                self.authorized(Method::Post, url)
                    .with_header("content-type", "application/json")
                    .with_body(Body::Json(b"{}".to_vec()))
            }
            CallKind::Whoami => {
                let url = format!("{base}/_matrix/client/v3/account/whoami");
                self.authorized(Method::Get, url)
            }
            CallKind::Upload { media } => {
                let mut url = format!("{base}/_matrix/media/v3/upload");
                if let Some(filename) = media.filename {
                    url.push_str("?filename=");
                    url.push_str(&encode_path_segment(filename));
                }
                self.authorized(Method::Post, url)
                    .with_header("content-type", media.content_type)
                    .with_body(Body::Bytes(media.data))
            }
        }
    }

    /// The headers every leg carries: the bearer token and the JSON accept.
    ///
    /// Generic over the body lifetime: a JSON leg hands back a `Request<'static>`
    /// (the body is owned), a media leg narrows it to the borrow of the bytes.
    fn authorized<'b>(&self, method: Method, url: String) -> Request<'b> {
        Request::new(method, url)
            .with_header(
                "authorization",
                format!("Bearer {}", self.token.expose_secret()),
            )
            .with_header("accept", "application/json")
    }

    /// Maps a `2xx` whose body did not decode. For a write the request may still
    /// have been applied — the homeserver answered, after all.
    fn undecodable(
        error: OutboundError,
        operation: Operation,
        txn: Option<&TxnId>,
        attempt: u32,
    ) -> OutboundError {
        if operation.is_repeatable() && txn.is_none() {
            return error;
        }
        OutboundError::Ambiguous {
            txn_id: txn.cloned(),
            attempts: attempt,
            reason: format!("the homeserver accepted the request but answered unreadably: {error}"),
        }
    }
}

/// Serialises a JSON value for a request body.
///
/// `Value`'s own formatter cannot fail, so there is no error path here.
fn json_bytes(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

/// Serialises outbound content and enforces the event ceiling.
fn check_content(content: &Value) -> OutboundResult<Vec<u8>> {
    let bytes = json_bytes(content);
    if bytes.len() > MAX_CONTENT_BYTES {
        return Err(OutboundError::MessageTooLong {
            limit: MAX_CONTENT_BYTES,
            actual: bytes.len(),
        });
    }
    Ok(bytes)
}

/// Percent-encodes one URL path segment.
///
/// Room ids, user ids, event ids and transaction ids all carry characters that
/// would otherwise end the segment (`!`, `:`, `$`, `+`). Everything outside the
/// RFC 3986 unreserved set is escaped, which is what the Matrix URL grammar
/// expects of a path segment.
fn encode_path_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(*byte));
            }
            _ => {
                // Writing into the existing buffer avoids one allocation per
                // escaped byte; the write to a `String` cannot fail.
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The `errcode` and `error` of a Matrix error body, with the raw body as the
/// fallback and a bound on how much of it is echoed.
fn matrix_error(response: &Response) -> (String, String) {
    let fallback = |body: &[u8]| {
        let text = String::from_utf8_lossy(body);
        let trimmed: String = text.chars().take(200).collect();
        ("M_UNKNOWN".to_owned(), trimmed)
    };
    match serde_json::from_slice::<Value>(&response.body) {
        Ok(body) => {
            let errcode = body
                .get("errcode")
                .and_then(Value::as_str)
                .unwrap_or("M_UNKNOWN")
                .to_owned();
            let message = body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if message.is_empty() && errcode == "M_UNKNOWN" {
                return fallback(&response.body);
            }
            (errcode, message)
        }
        Err(_) => fallback(&response.body),
    }
}

/// `M_CODE: message`, or just the code when the homeserver sent no message.
fn detail_text(errcode: &str, message: &str) -> String {
    if message.is_empty() {
        errcode.to_owned()
    } else {
        format!("{errcode}: {message}")
    }
}

/// A `4xx` that is not a rate limit: the request is wrong, retrying is pointless.
fn rejection(response: &Response) -> OutboundError {
    let (errcode, message) = matrix_error(response);
    OutboundError::Rejected {
        status: response.status,
        errcode,
        message,
    }
}

/// A failure the policy stopped on, turned into the variant that tells the caller
/// what may have happened.
fn http_failure(
    response: &Response,
    operation: Operation,
    txn: Option<&TxnId>,
    attempts: u32,
) -> OutboundError {
    let (errcode, message) = matrix_error(response);
    match response.status {
        429 => OutboundError::RateLimited {
            retry_after: retry::retry_after_ms(response),
            errcode,
            message,
        },
        500..=599 if !operation.is_repeatable() || txn.is_some() => OutboundError::Ambiguous {
            txn_id: txn.cloned(),
            attempts,
            reason: format!(
                "the homeserver answered {} for a write that may have been applied: {detail}",
                response.status,
                detail = detail_text(&errcode, &message),
            ),
        },
        500..=599 => OutboundError::Server {
            status: response.status,
            errcode,
            message,
            attempts,
        },
        _ => rejection(response),
    }
}

/// A transport failure the policy stopped on.
fn transport_failure(
    error: &TransportError,
    operation: Operation,
    txn: Option<&TxnId>,
    attempts: u32,
) -> OutboundError {
    let reason = error.to_string();
    if error.kind.is_before_write() || (operation.is_repeatable() && txn.is_none()) {
        return OutboundError::Transport { attempts, reason };
    }
    OutboundError::Ambiguous {
        txn_id: txn.cloned(),
        attempts,
        reason,
    }
}

/// Reads a string field, tolerating the `snake_case` and `camelCase` spellings a
/// homeserver or a proxy may send.
fn field_str<'a>(body: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| body.get(*name).and_then(Value::as_str))
}

/// `{"event_id": "$…"}` from a send or redact leg.
fn decode_event_id(response: &Response) -> OutboundResult<String> {
    let body: Value =
        serde_json::from_slice(&response.body).map_err(|error| OutboundError::Decode {
            what: "event send",
            reason: error.to_string(),
        })?;
    field_str(&body, &["event_id", "eventId"])
        .filter(|event_id| !event_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| OutboundError::Decode {
            what: "event send",
            reason: "the body carries no event_id".to_owned(),
        })
}

/// `{}` from a typing or receipt leg — the body is deliberately ignored, because
/// a homeserver that answers `200` with an empty body has done its job.
///
/// The `Result` looks redundant but is what makes all four decoders one
/// function-pointer type; removing it would split `execute` in two.
#[allow(clippy::unnecessary_wraps)]
fn decode_unit(_response: &Response) -> OutboundResult<()> {
    Ok(())
}

/// `{"content_uri": "mxc://…"}` from an upload.
fn decode_content_uri(response: &Response) -> OutboundResult<MxcUri> {
    let body: Value =
        serde_json::from_slice(&response.body).map_err(|error| OutboundError::Decode {
            what: "media upload",
            reason: error.to_string(),
        })?;
    let uri =
        field_str(&body, &["content_uri", "contentUri"]).ok_or_else(|| OutboundError::Decode {
            what: "media upload",
            reason: "the body carries no content_uri".to_owned(),
        })?;
    MxcUri::parse(uri).map_err(|error| OutboundError::Decode {
        what: "media upload",
        reason: error.to_string(),
    })
}

/// `{"user_id": "@…"}` from the probe.
fn decode_whoami(response: &Response) -> OutboundResult<String> {
    let body: Value =
        serde_json::from_slice(&response.body).map_err(|error| OutboundError::Decode {
            what: "whoami",
            reason: error.to_string(),
        })?;
    field_str(&body, &["user_id", "userId"])
        .filter(|user_id| !user_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| OutboundError::Decode {
            what: "whoami",
            reason: "the body carries no user_id".to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_escape_everything_but_the_unreserved_set() {
        assert_eq!(
            encode_path_segment("!room:example.org"),
            "%21room%3Aexample.org"
        );
        assert_eq!(encode_path_segment("$abc+def"), "%24abc%2Bdef");
        assert_eq!(
            encode_path_segment("@bot:example.org"),
            "%40bot%3Aexample.org"
        );
        assert_eq!(encode_path_segment("plain-1_2.3~"), "plain-1_2.3~");
        assert_eq!(encode_path_segment("ä"), "%C3%A4");
        assert_eq!(encode_path_segment(""), "");
        // Everything printable survives a round trip through the encoder as
        // unreserved bytes only.
        let encoded = encode_path_segment("a b/c?d#e%f");
        assert!(encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric()
                || matches!(byte, b'%' | b'-' | b'_' | b'.' | b'~')));
    }

    #[test]
    fn content_over_the_event_ceiling_is_refused_at_the_boundary() {
        let at_limit = json!({ "body": "x".repeat(MAX_CONTENT_BYTES - 32) });
        let bytes = check_content(&at_limit).expect("below the ceiling");
        assert!(bytes.len() <= MAX_CONTENT_BYTES);

        let over = json!({ "body": "x".repeat(MAX_CONTENT_BYTES) });
        let error = check_content(&over).expect_err("over the ceiling");
        assert!(matches!(error, OutboundError::MessageTooLong { .. }));
        assert!(error.to_string().contains("65536"));
    }

    #[test]
    fn error_bodies_are_read_permissively_and_bounded() {
        let response = Response::json(403, r#"{"errcode":"M_FORBIDDEN","error":"nope","extra":1}"#);
        assert_eq!(
            matrix_error(&response),
            ("M_FORBIDDEN".into(), "nope".into())
        );

        let no_message = Response::json(403, r#"{"errcode":"M_FORBIDDEN"}"#);
        assert_eq!(
            matrix_error(&no_message),
            ("M_FORBIDDEN".into(), String::new())
        );

        let empty = Response::json(403, "{}");
        assert_eq!(matrix_error(&empty), ("M_UNKNOWN".into(), "{}".into()));

        let not_json = Response::new(502, "<html>bad gateway</html>".as_bytes().to_vec());
        let (errcode, message) = matrix_error(&not_json);
        assert_eq!(errcode, "M_UNKNOWN");
        assert!(message.contains("bad gateway"));

        // A hostile body cannot flood a log line.
        let huge = Response::new(500, vec![b'x'; 10_000]);
        assert_eq!(matrix_error(&huge).1.chars().count(), 200);
    }

    #[test]
    fn decoding_tolerates_unknown_fields_and_both_spellings() {
        let class = |body: &str| decode_event_id(&Response::json(200, body));
        assert_eq!(class(r#"{"event_id":"$1"}"#).expect("snake"), "$1");
        assert_eq!(
            class(r#"{"eventId":"$2","room_id":"!r"}"#).expect("camel"),
            "$2"
        );
        assert!(class(r#"{"event_id":""}"#).is_err());
        assert!(class(r#"{"event_id":7}"#).is_err());
        assert!(class("not json").is_err());

        let uri = |body: &str| decode_content_uri(&Response::json(200, body));
        assert_eq!(
            uri(r#"{"content_uri":"mxc://a/b"}"#)
                .expect("snake")
                .as_str(),
            "mxc://a/b"
        );
        assert_eq!(
            uri(r#"{"contentUri":"mxc://a/c"}"#)
                .expect("camel")
                .as_str(),
            "mxc://a/c"
        );
        assert!(uri(r#"{"content_uri":"https://a/b"}"#).is_err());
        assert!(uri("{}").is_err());

        let me = |body: &str| decode_whoami(&Response::json(200, body));
        assert_eq!(
            me(r#"{"user_id":"@a:b","device_id":"X"}"#).expect("snake"),
            "@a:b"
        );
        assert_eq!(
            me(r#"{"userId":"@b:c","is_guest":true}"#).expect("camel"),
            "@b:c"
        );
        assert!(me(r#"{"device_id":"X"}"#).is_err());
        assert!(me("[]").is_err());
    }

    #[test]
    fn every_configured_homeserver_is_normalised_or_refused() {
        assert_eq!(
            ClientConfig::new(" https://m.example/ ")
                .expect("valid")
                .homeserver(),
            "https://m.example"
        );
        assert_eq!(
            ClientConfig::new("HTTP://m.example")
                .expect("valid")
                .homeserver(),
            "HTTP://m.example"
        );
        for bad in [
            "",
            "   ",
            "m.example",
            "ftp://m.example",
            "https://",
            "https://m example",
        ] {
            assert!(ClientConfig::new(bad).is_err(), "{bad:?} should be refused");
        }
        // A URL that is nothing but a scheme and a colon has no host.
        assert!(ClientConfig::new("https://:").is_err());
    }

    #[test]
    fn client_construction_rejects_the_three_unusable_configurations() {
        let transport = Arc::new(StubTransport::new());
        let config = || ClientConfig::new("https://m.example").expect("valid");

        assert!(MatrixClient::new(transport.clone(), config(), "").is_err());
        let no_attempts = config().with_retry(RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::default()
        });
        assert!(MatrixClient::new(transport.clone(), no_attempts, "t").is_err());
        assert!(
            MatrixClient::new(transport.clone(), config().with_max_upload_bytes(0), "t").is_err()
        );
        assert!(MatrixClient::new(transport.clone(), config().with_user_id("bot"), "t").is_err());
        let client = MatrixClient::new(transport, config().with_user_id("@bot:m.example"), "t")
            .expect("valid");
        assert!(!format!("{client:?}").contains("t="));
    }

    #[test]
    fn the_debug_never_prints_the_token() {
        let client = MatrixClient::new(
            Arc::new(StubTransport::new()),
            ClientConfig::new("https://m.example").expect("valid"),
            "syt_very_secret",
        )
        .expect("valid");
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("syt_very_secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}
