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
//! | [`MatrixClient::send_media_with_txn`] | same, caller-chosen send transaction id | upload: none, send: transaction id |
//! | [`MatrixClient::send_uploaded_media_with_txn`] | send only, for resuming after an upload | transaction id |
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
//!     "syt_access_token_123",
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

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde_json::{json, Value};
use url::Url;

pub use error::{MediaSendError, OutboundError, OutboundResult};
pub use http::{ReqwestTransport, DEFAULT_REQUEST_TIMEOUT, MAX_RESPONSE_BODY_BYTES};
pub use ordering::{OrderingSaturation, GATE_CAPACITY, GATE_WAITERS_PER_ROOM, GATE_WAIT_TIMEOUT};
pub use retry::{
    Operation, RetryPolicy, RetryPolicyError, Verdict, DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_ATTEMPTS,
    DEFAULT_MAX_DELAY_MS, DEFAULT_MAX_RATE_LIMIT_WAIT_MS,
};
pub use testing::{RecordedRequest, RecordingSleeper, StubCounters, StubTransport};
pub use transport::Transport;
pub use types::{
    MediaKind, MediaSendOutcome, MediaUpload, MxcUri, SendOutcome, TxnId, DEFAULT_MAX_UPLOAD_BYTES,
    DEFAULT_TYPING_TIMEOUT_MS, MAX_CONTENT_BYTES, MAX_EVENT_TYPE_BYTES, MAX_ID_BYTES,
    MAX_MXC_URI_BYTES, MAX_TXN_ID_BYTES,
};
// `MAX_TXN_ID_BYTES` and friends are declared in `types` and re-exported above.

use crate::matrix_out::ordering::{GateOutcome, RoomGates, RoomKey};
use crate::matrix_out::transport::{
    Body, BoxFuture, Method, Request, Response, TransportError, TransportErrorKind,
};
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
///     .with_ordering_saturation(messenger_webhook::matrix_out::OrderingSaturation::Unordered)
///     .with_retry(RetryPolicy::default().with_max_attempts(2).expect("valid"));
///
/// // The trailing slash is normalised away, so paths never double up.
/// assert_eq!(config.homeserver(), "https://matrix.example.org");
/// assert_eq!(config.user_id(), Some("@bot:example.org"));
/// assert_eq!(config.max_upload_bytes(), 10_000_000);
/// assert!(!config.per_room_ordering());
/// assert_eq!(
///     config.ordering_saturation(),
///     messenger_webhook::matrix_out::OrderingSaturation::Unordered
/// );
/// assert_eq!(config.retry().max_attempts(), 2);
/// assert_eq!(Duration::from_millis(500), RetryPolicy::default().base_delay());
/// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
/// ```
#[derive(Clone)]
pub struct ClientConfig {
    homeserver: String,
    user_id: Option<String>,
    retry: RetryPolicy,
    max_upload_bytes: u64,
    per_room_ordering: bool,
    ordering_saturation: OrderingSaturation,
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
            .field("ordering_saturation", &self.ordering_saturation)
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
    /// [`OutboundError::InvalidConfig`] when `homeserver` is not an absolute
    /// `http://` or `https://` URL without userinfo, query or fragment, or
    /// carries whitespace or control characters.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ClientConfig;
    ///
    /// assert!(ClientConfig::new("https://matrix.example.org").is_ok());
    /// assert!(ClientConfig::new("matrix.example.org").is_err());
    /// assert!(ClientConfig::new("ftp://matrix.example.org").is_err());
    /// assert!(ClientConfig::new("https://user:secret@matrix.example.org").is_err());
    /// ```
    pub fn new(homeserver: impl Into<String>) -> OutboundResult<Self> {
        let raw = homeserver.into();
        let base = parse_homeserver(raw.as_str())?;
        Ok(Self {
            homeserver: base,
            user_id: None,
            retry: RetryPolicy::default(),
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            per_room_ordering: true,
            ordering_saturation: OrderingSaturation::Backpressure,
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
    ///     .with_retry(RetryPolicy::default().with_max_attempts(1).expect("valid"));
    /// assert_eq!(config.retry().max_attempts(), 1);
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

    /// Chooses what happens when all per-room ordering gates are in use.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{ClientConfig, OrderingSaturation};
    ///
    /// let config = ClientConfig::new("https://m.example")?
    ///     .with_ordering_saturation(OrderingSaturation::Error);
    /// assert_eq!(config.ordering_saturation(), OrderingSaturation::Error);
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn with_ordering_saturation(mut self, saturation: OrderingSaturation) -> Self {
        self.ordering_saturation = saturation;
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
    /// assert_eq!(ClientConfig::new("https://m.example")?.retry().max_attempts(), 3);
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

    /// What happens when all per-room ordering gates are in use.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{ClientConfig, OrderingSaturation};
    ///
    /// assert_eq!(
    ///     ClientConfig::new("https://m.example")?.ordering_saturation(),
    ///     OrderingSaturation::Backpressure,
    /// );
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub const fn ordering_saturation(&self) -> OrderingSaturation {
        self.ordering_saturation
    }
}

/// Parses and normalises a homeserver base URL.
fn parse_homeserver(homeserver: &str) -> OutboundResult<String> {
    let trimmed = homeserver.trim();
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(OutboundError::InvalidConfig {
            reason: "the homeserver URL contains whitespace or a control character".to_owned(),
        });
    }

    let parsed = Url::parse(trimmed).map_err(|error| OutboundError::InvalidConfig {
        reason: format!("the homeserver URL is not a valid absolute URL: {error}"),
    })?;
    let after_scheme = trimmed
        .get(parsed.scheme().len()..)
        .filter(|rest| rest.starts_with("://"))
        .ok_or_else(|| OutboundError::InvalidConfig {
            reason: "the homeserver URL must be an absolute http:// or https:// URL".to_owned(),
        })?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(OutboundError::InvalidConfig {
            reason: format!(
                "the homeserver URL scheme `{}` is not http:// or https://",
                parsed.scheme()
            ),
        });
    }
    let authority_end = after_scheme[3..]
        .find(['/', '?', '#'])
        .map_or(trimmed.len(), |offset| parsed.scheme().len() + 3 + offset);
    if trimmed[parsed.scheme().len() + 3..authority_end].contains('@') {
        return Err(OutboundError::InvalidConfig {
            reason: "the homeserver URL must not include userinfo credentials".to_owned(),
        });
    }
    if parsed.query().is_some() {
        return Err(OutboundError::InvalidConfig {
            reason: "the homeserver URL must not include a query string".to_owned(),
        });
    }
    if parsed.fragment().is_some() {
        return Err(OutboundError::InvalidConfig {
            reason: "the homeserver URL must not include a fragment".to_owned(),
        });
    }
    if parsed.host_str().is_none() {
        return Err(OutboundError::InvalidConfig {
            reason: "the homeserver URL has no host".to_owned(),
        });
    }

    let mut base = parsed.origin().ascii_serialization();
    let path = parsed.path().trim_end_matches('/');
    if !path.is_empty() {
        base.push_str(path);
    }
    Ok(base)
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
            .field("gate_saturation_waits", &self.gates.saturation_waits())
            .field("unordered_sends", &self.gates.unordered_sends())
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
        content: Vec<u8>,
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
    /// media ceiling is `0`, or a configured user id is not a Matrix user id.
    /// A retry policy is validated when it is built by [`RetryPolicy::new`] or
    /// one of its `with_*` methods.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MatrixClient::new(
    ///     Arc::new(StubTransport::new()),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt_token_123456",
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
        let token = Secret::new(access_token).map_err(|error| OutboundError::InvalidConfig {
            reason: error.to_string(),
        })?;
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
        let saturation = config.ordering_saturation;
        Ok(Self {
            transport,
            config,
            token,
            gates: RoomGates::new(GATE_CAPACITY, saturation)?,
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
    ///     "syt_token_123456",
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
    ///     "syt_token_123456",
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
    ///     "syt_token_123456",
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
    ///     "syt_token_123456",
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
    ///     "syt_token_123456",
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
    ///     "syt_token_123456",
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
    /// [`OutboundError::InvalidRequest`] for an unusable room or event id, or a
    /// reason containing a control character, plus every delivery error. A
    /// reason over the event ceiling is reported as
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
    ///     "syt_token_123456",
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
        if let Some(reason) = reason {
            if reason.chars().any(char::is_control) {
                return Err(OutboundError::InvalidRequest {
                    reason: "the redaction reason contains a control character".to_owned(),
                });
            }
        }
        let content = match reason {
            Some(reason) => json!({ "reason": reason }),
            None => json!({}),
        };
        let content = check_content(&content)?;
        let txn_id = TxnId::generate();
        let call = Call {
            kind: CallKind::Redact {
                room_id,
                event_id,
                txn: &txn_id,
                content,
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
    ///     "syt_token_123456",
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
    ///     "syt_token_123456",
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
        self.execute(call, Operation::StateSetting, None, decode_unit)
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
    ///     "syt_token_123456",
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
        self.execute(call, Operation::StateSetting, None, decode_unit)
            .await
            .map(|_| ())
    }

    /// Uploads media and returns the `mxc://` URI that references it.
    ///
    /// The upload carries **no** idempotency key: a repeated upload stores the
    /// bytes twice. It is therefore never retried after a `5xx`, a timeout, or a
    /// `429` that is not proven by a Matrix `M_LIMIT_EXCEEDED` body — those come
    /// back as [`OutboundError::Ambiguous`] — only after a homeserver rate limit
    /// or a connection that was never established.
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
    ///     "syt_token_123456",
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
    ///     "syt_token_123456",
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
        self.send_media_with_txn(TxnId::generate(), room_id, media, caption)
            .await
            .map(|outcome| outcome.send)
            .map_err(OutboundError::from)
    }

    /// Uploads media and sends it into a room under a caller-chosen transaction id.
    ///
    /// The returned [`MediaSendOutcome`] carries both the upload URI and the event
    /// send result. Keep the URI and transaction id if the send leg later fails:
    /// [`send_uploaded_media_with_txn`](Self::send_uploaded_media_with_txn) can
    /// resume with just those values, without re-uploading the bytes.
    ///
    /// # Errors
    ///
    /// [`MediaSendError::Upload`] when the upload leg fails; [`MediaSendError::Send`]
    /// when the bytes were uploaded but the event send fails. The latter carries
    /// the `mxc://` URI and the transaction id needed to resume at the send leg.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{
    ///     ClientConfig, MatrixClient, MediaUpload, StubTransport, TxnId,
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
    ///     "syt_token_123456",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let outcome = runtime.block_on(client.send_media_with_txn(
    ///     TxnId::new("media-1")?,
    ///     "!r:m.example",
    ///     &MediaUpload::new(b"\x89PNG", "image/png"),
    ///     Some("cat"),
    /// ))?;
    /// assert_eq!(outcome.content_uri.as_str(), "mxc://m.example/abc");
    /// assert_eq!(outcome.send.event_id, "$img");
    /// assert_eq!(outcome.send.txn_id.as_str(), "media-1");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_media_with_txn(
        &self,
        txn_id: TxnId,
        room_id: &str,
        media: &MediaUpload<'_>,
        caption: Option<&str>,
    ) -> Result<types::MediaSendOutcome, error::MediaSendError> {
        types::validate_room_id(room_id).map_err(|source| MediaSendError::Request {
            source: Box::new(source),
        })?;
        let uri = self
            .upload_media(media)
            .await
            .map_err(|source| MediaSendError::Upload {
                source: Box::new(source),
            })?;
        self.send_uploaded_media_with_txn(txn_id, room_id, &uri, media, caption)
            .await
    }

    /// Sends an already-uploaded media item into a room under a caller-chosen
    /// transaction id.
    ///
    /// This is the resumable half of [`send_media_with_txn`](Self::send_media_with_txn):
    /// call it with the `mxc://` URI and transaction id from a
    /// [`MediaSendError::Send`] instead of re-uploading the bytes.
    ///
    /// # Errors
    ///
    /// As [`send_event_with_txn`](Self::send_event_with_txn) for the event leg.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use messenger_webhook::matrix_out::{
    ///     ClientConfig, MatrixClient, MediaUpload, MxcUri, StubTransport, TxnId,
    /// };
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = Arc::new(StubTransport::new());
    /// transport.push_json(200, r#"{"event_id":"$img"}"#);
    /// let client = MatrixClient::new(
    ///     transport.clone(),
    ///     ClientConfig::new("https://m.example")?,
    ///     "syt_token_123456",
    /// )?;
    ///
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// let upload = MediaUpload::new(b"\x89PNG", "image/png");
    /// let outcome = runtime.block_on(client.send_uploaded_media_with_txn(
    ///     TxnId::new("media-1")?,
    ///     "!r:m.example",
    ///     &MxcUri::parse("mxc://m.example/abc")?,
    ///     &upload,
    ///     Some("cat"),
    /// ))?;
    /// assert_eq!(outcome.send.event_id, "$img");
    /// assert_eq!(transport.call_count(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_uploaded_media_with_txn(
        &self,
        txn_id: TxnId,
        room_id: &str,
        content_uri: &MxcUri,
        media: &MediaUpload<'_>,
        caption: Option<&str>,
    ) -> Result<types::MediaSendOutcome, error::MediaSendError> {
        types::validate_room_id(room_id).map_err(|source| MediaSendError::Request {
            source: Box::new(source),
        })?;
        let body = caption
            .or(media.filename)
            .unwrap_or_else(|| media.kind.msgtype());
        let content = json!({
            "msgtype": media.kind.msgtype(),
            "body": body,
            "url": content_uri.as_str(),
            "info": {
                "mimetype": media.content_type,
                "size": media.data.len(),
            },
        });
        let send_txn_id = txn_id.clone();
        let send = self
            .send_event_with_txn(send_txn_id, room_id, "m.room.message", content)
            .await
            .map_err(|source| MediaSendError::Send {
                content_uri: content_uri.clone(),
                txn_id: txn_id.clone(),
                source: Box::new(source),
            })?;
        Ok(types::MediaSendOutcome {
            content_uri: content_uri.clone(),
            send,
        })
    }

    /// The connection probe: resolves the authenticated user id.
    ///
    /// # Errors
    ///
    /// [`OutboundError::DecodeRead`] when the body is not the `whoami` shape,
    /// plus every delivery error.
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
    ///     "syt_token_123456",
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

    /// Runs one call under the retry policy, holding the room gate only while a
    /// request is in flight.
    async fn execute<T>(
        &self,
        call: Call<'_>,
        operation: Operation,
        txn: Option<&TxnId>,
        decode: fn(&Response) -> OutboundResult<T>,
    ) -> OutboundResult<Attempted<T>> {
        let room_key = match call.room_id() {
            Some(room_id) if self.config.per_room_ordering => Some(RoomKey::new(room_id)?),
            _ => None,
        };
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let request = self.build(&call);
            let response = match room_key {
                Some(room_key) => {
                    let _gate = match self.gates.acquire(room_key).await? {
                        GateOutcome::Ordered(guard) => Some(guard),
                        GateOutcome::Unordered => None,
                    };
                    self.transport.send(request).await
                }
                None => self.transport.send(request).await,
            };
            match response {
                Ok(response) => {
                    let verdict = self
                        .config
                        .retry
                        .classify(attempt, &response, operation, txn);
                    match verdict {
                        Verdict::Success => {
                            return match decode(&response) {
                                Ok(value) => Ok(Attempted {
                                    value,
                                    attempts: attempt,
                                }),
                                Err(error) => Err(Self::undecodable(error, operation, txn)),
                            };
                        }
                        Verdict::Retry { delay } => self.config.sleeper.sleep(delay).await,
                        Verdict::Rejected | Verdict::Exhausted | Verdict::Ambiguous => {
                            return Err(http_failure(verdict, &response, operation, txn, attempt));
                        }
                    }
                }
                Err(error) => {
                    match self
                        .config
                        .retry
                        .classify_transport(attempt, &error, operation, txn)
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
                content,
            } => {
                let url = format!(
                    "{base}/_matrix/client/v3/rooms/{}/redact/{}/{}",
                    encode_path_segment(room_id),
                    encode_path_segment(event_id),
                    encode_path_segment(txn.as_str()),
                );
                self.authorized(Method::Put, url)
                    .with_header("content-type", "application/json")
                    .with_body(Body::Json(content.clone()))
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
                    url.push_str(&encode_query_value(filename));
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
    ) -> OutboundError {
        if matches!(operation, Operation::Read | Operation::StateSetting) {
            return error;
        }
        match error {
            OutboundError::DecodeRead { what, reason } => OutboundError::DecodeAfterWrite {
                what,
                txn_id: txn.cloned(),
                reason,
            },
            error => error,
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
    // The specification says "The complete event MUST NOT be larger than 65536
    // bytes". The envelope is never empty, so content of exactly the event
    // ceiling can never fit.
    if bytes.len() >= MAX_CONTENT_BYTES {
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
    utf8_percent_encode(raw, URL_COMPONENT).to_string()
}

/// Percent-encodes one URL query value.
///
/// Query values use the same unreserved-only set as path segments, so query
/// delimiters such as `&` and `=` in a filename cannot reshape the URL.
fn encode_query_value(raw: &str) -> String {
    utf8_percent_encode(raw, URL_COMPONENT).to_string()
}

/// The RFC 3986 unreserved bytes a URL path segment or query value may carry
/// unchanged.
const URL_COMPONENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// The `errcode` and `error` of a Matrix error body, with the raw body as the
/// fallback and a bound on how much of it is echoed.
fn matrix_error(response: &Response) -> (String, String) {
    let fallback = |body: &[u8]| {
        let prefix = &body[..body.len().min(ERROR_BODY_FALLBACK_BYTES)];
        let mut message = String::from_utf8_lossy(prefix).into_owned();
        if body.len() > ERROR_BODY_FALLBACK_BYTES {
            message.push_str("…truncated");
        }
        ("M_UNKNOWN".to_owned(), message)
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

/// How many bytes of an unparseable error body may reach the caller.
const ERROR_BODY_FALLBACK_BYTES: usize = 200;

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
    verdict: Verdict,
    response: &Response,
    operation: Operation,
    txn: Option<&TxnId>,
    attempts: u32,
) -> OutboundError {
    let (errcode, message) = matrix_error(response);
    match verdict {
        Verdict::Exhausted if response.status == 429 => OutboundError::RateLimited {
            retry_after: retry::retry_after_ms(response),
            errcode,
            message,
        },
        Verdict::Exhausted if operation.is_repeatable_with(txn) => OutboundError::Server {
            operation,
            txn_id: txn.cloned(),
            status: response.status,
            errcode,
            message,
            attempts,
        },
        Verdict::Ambiguous | Verdict::Exhausted => OutboundError::Ambiguous {
            txn_id: txn.cloned(),
            attempts,
            reason: format!(
                "the homeserver answered {} for a write that may have been applied: {detail}",
                response.status,
                detail = detail_text(&errcode, &message),
            ),
        },
        Verdict::Success | Verdict::Retry { .. } | Verdict::Rejected => rejection(response),
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
    if error.kind == TransportErrorKind::TestHarness {
        return OutboundError::TestHarness { reason };
    }
    if error.kind == TransportErrorKind::InvalidRequest
        || error.kind == TransportErrorKind::ResponseTooLarge
    {
        return OutboundError::InvalidRequest {
            reason: error.message.clone(),
        };
    }
    if error.kind.is_before_write()
        || matches!(operation, Operation::Read | Operation::StateSetting)
    {
        return OutboundError::Transport {
            kind: error.kind,
            attempts,
            reason,
        };
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
        serde_json::from_slice(&response.body).map_err(|error| OutboundError::DecodeRead {
            what: "event send",
            reason: error.to_string(),
        })?;
    field_str(&body, &["event_id", "eventId"])
        .filter(|event_id| !event_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| OutboundError::DecodeRead {
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
        serde_json::from_slice(&response.body).map_err(|error| OutboundError::DecodeRead {
            what: "media upload",
            reason: error.to_string(),
        })?;
    let uri = field_str(&body, &["content_uri", "contentUri"]).ok_or_else(|| {
        OutboundError::DecodeRead {
            what: "media upload",
            reason: "the body carries no content_uri".to_owned(),
        }
    })?;
    MxcUri::parse(uri).map_err(|error| OutboundError::DecodeRead {
        what: "media upload",
        reason: error.to_string(),
    })
}

/// `{"user_id": "@…"}` from the probe.
fn decode_whoami(response: &Response) -> OutboundResult<String> {
    let body: Value =
        serde_json::from_slice(&response.body).map_err(|error| OutboundError::DecodeRead {
            what: "whoami",
            reason: error.to_string(),
        })?;
    field_str(&body, &["user_id", "userId"])
        .filter(|user_id| !user_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| OutboundError::DecodeRead {
            what: "whoami",
            reason: "the body carries no user_id".to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

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
        assert_eq!(encode_path_segment("a/b?c=d&e+f"), "a%2Fb%3Fc%3Dd%26e%2Bf");
        // Everything printable survives a round trip through the encoder as
        // unreserved bytes only.
        let encoded = encode_path_segment("a b/c?d#e%f");
        assert!(encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric()
                || matches!(byte, b'%' | b'-' | b'_' | b'.' | b'~')));
    }

    #[test]
    fn query_values_escape_query_delimiters_but_keep_the_path_unreserved_set() {
        assert_eq!(
            encode_query_value("Bild München & plan.png"),
            "Bild%20M%C3%BCnchen%20%26%20plan.png"
        );
        assert_eq!(
            encode_query_value("a/b?c=d&e+f#g%h"),
            "a%2Fb%3Fc%3Dd%26e%2Bf%23g%25h"
        );
        assert_eq!(
            encode_query_value("name=green&body=hello"),
            "name%3Dgreen%26body%3Dhello"
        );
        assert_eq!(encode_query_value("100% done"), "100%25%20done");
        assert_eq!(encode_query_value("plain-1_2.3~"), "plain-1_2.3~");
    }

    proptest! {
        #[test]
        fn encoded_url_values_round_trip(raw in "\\PC{0,200}") {
            let encoded = encode_query_value(raw.as_str());
            let decoded = percent_encoding::percent_decode_str(&encoded)
                .decode_utf8()
                .expect("encoded input is UTF-8");
            prop_assert_eq!(decoded.as_ref(), raw.as_str());
        }
    }

    #[test]
    fn content_at_the_event_ceiling_is_refused_at_the_boundary() {
        let envelope = json!({ "body": "" }).to_string().len();

        let just_inside = json!({ "body": "x".repeat(MAX_CONTENT_BYTES - envelope - 1) });
        let bytes = check_content(&just_inside).expect("just inside the ceiling");
        assert_eq!(bytes.len(), MAX_CONTENT_BYTES - 1);

        let at_limit = json!({ "body": "x".repeat(MAX_CONTENT_BYTES - envelope) });
        let error = check_content(&at_limit).expect_err("at the ceiling");
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

        // A hostile body cannot flood a log line; the fallback keeps the
        // boundary exact and marks only the bytes it dropped.
        let just_inside = Response::new(500, vec![b'x'; ERROR_BODY_FALLBACK_BYTES - 1]);
        assert_eq!(
            matrix_error(&just_inside).1,
            "x".repeat(ERROR_BODY_FALLBACK_BYTES - 1)
        );

        let at_limit = Response::new(500, vec![b'x'; ERROR_BODY_FALLBACK_BYTES]);
        assert_eq!(
            matrix_error(&at_limit).1,
            "x".repeat(ERROR_BODY_FALLBACK_BYTES)
        );

        let over_limit = Response::new(500, vec![b'x'; ERROR_BODY_FALLBACK_BYTES + 1]);
        assert_eq!(
            matrix_error(&over_limit).1,
            format!("x{}…truncated", "x".repeat(ERROR_BODY_FALLBACK_BYTES - 1))
        );

        let huge = Response::new(500, vec![b'x'; 10_000]);
        assert!(matrix_error(&huge).1.ends_with("…truncated"));
        assert_eq!(
            matrix_error(&huge).1.len(),
            ERROR_BODY_FALLBACK_BYTES + "…truncated".len()
        );
    }

    #[test]
    fn http_errors_follow_the_classified_verdict_for_every_operation() {
        let policy = RetryPolicy::default().with_max_attempts(1).expect("valid");
        let txn = TxnId::new("txn-73").expect("valid");
        let cases = [
            (
                Response::json(403, r#"{"errcode":"M_FORBIDDEN"}"#),
                "rejected",
            ),
            (
                Response::json(429, r#"{"retry_after_ms":10}"#),
                "rate limit",
            ),
            (Response::json(503, r#"{"errcode":"M_UNKNOWN"}"#), "server"),
            (
                Response::new(429, Vec::new()).with_header("retry-after", "2"),
                "proxy rate limit",
            ),
        ];

        for (response, label) in cases {
            for operation in [
                Operation::Read,
                Operation::StateSetting,
                Operation::Idempotent,
                Operation::NonIdempotent,
            ] {
                let txn = match operation {
                    Operation::Read | Operation::StateSetting => None,
                    Operation::Idempotent | Operation::NonIdempotent => Some(&txn),
                };
                let verdict = policy.classify(1, &response, operation, txn);
                let error = http_failure(verdict, &response, operation, txn, 1);

                match verdict {
                    Verdict::Rejected => assert!(
                        matches!(error, OutboundError::Rejected { .. }),
                        "{label}/{operation:?}: {verdict:?} produced {error:?}"
                    ),
                    Verdict::Exhausted if response.status == 429 => assert!(
                        matches!(error, OutboundError::RateLimited { .. }),
                        "{label}/{operation:?}: {verdict:?} produced {error:?}"
                    ),
                    Verdict::Exhausted => assert!(
                        matches!(error, OutboundError::Server { .. }),
                        "{label}/{operation:?}: {verdict:?} produced {error:?}"
                    ),
                    Verdict::Ambiguous => assert!(
                        matches!(error, OutboundError::Ambiguous { .. }),
                        "{label}/{operation:?}: {verdict:?} produced {error:?}"
                    ),
                    Verdict::Success | Verdict::Retry { .. } => {
                        panic!("{label}/{operation:?}: classification stopped on {verdict:?}")
                    }
                }
            }
        }
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
            "http://m.example"
        );
        for bad in [
            "",
            "   ",
            "m.example",
            "ftp://m.example",
            "https://",
            "https://m example",
            "http:m.example",
            "https://user:secret@m.example",
            "https://@m.example",
            "https://m.example?access_token=secret",
            "https://m.example#fragment",
        ] {
            assert!(ClientConfig::new(bad).is_err(), "{bad:?} should be refused");
        }
        // A URL that is nothing but a scheme and a colon has no host.
        assert!(ClientConfig::new("https://:").is_err());
    }

    #[test]
    fn a_homeserver_path_prefix_is_kept_but_credentials_never_reach_debug() {
        let config = ClientConfig::new("https://m.example/proxy")
            .expect("path prefixes stay part of the base URL");
        assert_eq!(config.homeserver(), "https://m.example/proxy");

        let error = ClientConfig::new("https://user:secret@m.example").expect_err("userinfo");
        assert!(!format!("{error:?}").contains("secret"));
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn client_construction_rejects_the_three_unusable_configurations() {
        let transport = Arc::new(StubTransport::new());
        let config = || ClientConfig::new("https://m.example").expect("valid");

        assert!(MatrixClient::new(transport.clone(), config(), "").is_err());
        assert!(MatrixClient::new(
            transport.clone(),
            config().with_max_upload_bytes(0),
            "matrix-access-token"
        )
        .is_err());
        assert!(MatrixClient::new(
            transport.clone(),
            config().with_user_id("bot"),
            "matrix-access-token"
        )
        .is_err());
        let client = MatrixClient::new(
            transport,
            config().with_user_id("@bot:m.example"),
            "matrix-access-token",
        )
        .expect("valid");
        assert!(!format!("{client:?}").contains("matrix-access-token"));
    }

    #[test]
    fn strict_saturation_is_observable_on_the_send_path() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async {
            let transport = Arc::new(StubTransport::new());
            let config = ClientConfig::new("https://m.example")
                .expect("valid")
                .with_ordering_saturation(OrderingSaturation::Error);
            let client = MatrixClient {
                transport: transport.clone(),
                config,
                token: Secret::new("syt_token_123456").expect("valid token"),
                gates: RoomGates::new(1, OrderingSaturation::Error).expect("valid capacity"),
            };
            let held = client
                .gates
                .acquire(RoomKey::new("!a:x").expect("valid room"))
                .await
                .expect("the first room fits");
            let held = match held {
                GateOutcome::Ordered(guard) => guard,
                GateOutcome::Unordered => {
                    unreachable!("strict mode must not return an unordered outcome")
                }
            };

            let error = client
                .send_text("!b:x", "Hallo")
                .await
                .expect_err("the second room exceeds capacity");
            assert!(matches!(
                error,
                OutboundError::OrderingLost {
                    room_id,
                    capacity: 1,
                } if room_id == "!b:x"
            ));
            assert_eq!(transport.call_count(), 0, "nothing may be sent unordered");
            drop(held);
        });
    }

    #[test]
    fn explicit_unordered_saturation_is_reported_by_debug() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async {
            let transport = Arc::new(StubTransport::new());
            transport.push_json(200, r#"{"event_id":"$ok"}"#);
            let config = ClientConfig::new("https://m.example")
                .expect("valid")
                .with_ordering_saturation(OrderingSaturation::Unordered);
            let client = MatrixClient {
                transport: transport.clone(),
                config,
                token: Secret::new("syt_token_123456").expect("valid token"),
                gates: RoomGates::new(1, OrderingSaturation::Unordered).expect("valid capacity"),
            };
            let held = client
                .gates
                .acquire(RoomKey::new("!a:x").expect("valid room"))
                .await
                .expect("the first room fits");
            let held = match held {
                GateOutcome::Ordered(guard) => guard,
                GateOutcome::Unordered => {
                    unreachable!("the first room must be ordered")
                }
            };

            let outcome = client
                .send_text("!b:x", "Hallo")
                .await
                .expect("unordered mode sends explicitly");
            assert_eq!(outcome.event_id, "$ok");
            assert_eq!(transport.call_count(), 1);
            let debug = format!("{client:?}");
            assert!(debug.contains("unordered_sends: 1"), "{debug}");
            assert!(debug.contains("gate_saturation_waits: 0"), "{debug}");
            drop(held);
        });
    }

    #[test]
    fn the_debug_never_prints_the_token() {
        let client = MatrixClient::new(
            Arc::new(StubTransport::new()),
            ClientConfig::new("https://m.example").expect("valid"),
            "syt_very_secret_token",
        )
        .expect("valid");
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("syt_very_secret_token"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}
