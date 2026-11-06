//! Typed errors.
//!
//! Most failures reported by the shipped adapters are **terminal**: the caller
//! has to fix the payload, the headers or the configuration before retrying.
//! Signature and size failures fail closed, so a request that cannot be proven
//! authentic never reaches a parser.
//!
//! Third-party [`crate::Adapter`] implementations can also report transport and
//! delivery outcomes through [`Error::Transport`] and [`Error::Ambiguous`].
//! Those variants keep retry safety explicit: a transport failure before a
//! write is retryable, while an ambiguous write must be repeated only with the
//! same idempotency key, if one exists.

use thiserror::Error;

use crate::model::{Channel, IdempotencyKey};

/// Unit a provider message limit is expressed in.
///
/// ```
/// use messenger_webhook::LimitUnit;
///
/// assert_eq!(LimitUnit::Bytes.to_string(), "bytes");
/// assert_eq!(LimitUnit::Utf16Units.to_string(), "UTF-16 units");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LimitUnit {
    /// A byte count.
    Bytes,
    /// A count of UTF-16 code units.
    Utf16Units,
}

impl std::fmt::Display for LimitUnit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Bytes => "bytes",
            Self::Utf16Units => "UTF-16 units",
        })
    }
}

/// Everything that can go wrong while verifying, normalising or encoding.
///
/// ```
/// use messenger_webhook::{Channel, Error};
///
/// let error = Error::UnknownChannel(Channel::other("signal")?);
/// assert_eq!(error.to_string(), "no adapter registered for channel signal");
/// assert!(!error.is_retryable());
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The body is valid JSON, but not the JSON shape the provider sends.
    #[error("invalid payload: {0}")]
    Payload(String),

    /// The body is not valid JSON.
    ///
    /// The underlying [`serde_json::Error`] is preserved as the source, so a
    /// caller can inspect its category, line and column instead of having to
    /// parse the error message.
    ///
    /// ```
    /// use messenger_webhook::Error;
    ///
    /// let error = serde_json::from_slice::<serde_json::Value>(b"not json").unwrap_err();
    /// let error = Error::from(error);
    /// assert!(matches!(error, Error::Json { .. }));
    /// assert!(std::error::Error::source(&error).is_some());
    /// ```
    #[error("invalid JSON: {source}")]
    Json {
        /// The underlying parser or serializer failure.
        #[from]
        source: serde_json::Error,
    },

    /// Reading the request body failed before it could be verified.
    #[error("could not read request body: {0}")]
    Io(#[from] std::io::Error),

    /// The body is larger than the adapter's ceiling; it is rejected before
    /// any hashing or deserialisation happens. When the body is read through
    /// [`crate::read_body`], `actual` is at least the limit and is capped at
    /// [`crate::MAX_BODY_READ_BYTES`].
    ///
    /// ```
    /// use messenger_webhook::{
    ///     Adapter, Channel, Error, Headers, NormalizedMessage, OutboundMessage, ProviderMethod,
    ///     ProviderPayload, Registry, Result, MAX_BODY_BYTES,
    /// };
    ///
    /// #[derive(Debug)]
    /// struct AcceptAll;
    ///
    /// impl Adapter for AcceptAll {
    ///     fn channel(&self) -> Channel { Channel::WhatsApp }
    ///     fn verify(&self, _: &Headers, _: &[u8]) -> Result<()> { Ok(()) }
    ///     fn adapt(&self, _: &[u8]) -> Result<Vec<NormalizedMessage>> { Ok(Vec::new()) }
    ///     fn encode(&self, _: &OutboundMessage) -> Result<ProviderPayload> {
    ///         ProviderPayload::new(
    ///             Channel::WhatsApp,
    ///             ProviderMethod::Post,
    ///             "application/json",
    ///             b"{}".to_vec(),
    ///         )
    ///     }
    /// }
    ///
    /// let mut registry = Registry::new();
    /// registry.register(AcceptAll);
    ///
    /// let at_limit = vec![b' '; MAX_BODY_BYTES];
    /// assert!(registry.verify(&Channel::WhatsApp, &Headers::new(), &at_limit).is_ok());
    /// assert!(registry.adapt(&Channel::WhatsApp, &at_limit).is_ok());
    ///
    /// let over_limit = vec![b' '; MAX_BODY_BYTES + 1];
    /// assert!(matches!(
    ///     registry.verify(&Channel::WhatsApp, &Headers::new(), &over_limit),
    ///     Err(Error::BodyTooLarge {
    ///         limit: MAX_BODY_BYTES,
    ///         actual,
    ///     }) if actual == MAX_BODY_BYTES + 1
    /// ));
    /// assert!(matches!(
    ///     registry.adapt(&Channel::WhatsApp, &over_limit),
    ///     Err(Error::BodyTooLarge {
    ///         limit: MAX_BODY_BYTES,
    ///         actual,
    ///     }) if actual == MAX_BODY_BYTES + 1
    /// ));
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[error("payload is {actual} bytes, over the {limit} byte limit")]
    BodyTooLarge {
        /// Configured ceiling in bytes.
        limit: usize,
        /// Actual body length in bytes.
        actual: usize,
    },

    /// The adapter has no verification secret and therefore refuses to accept
    /// anything (fail closed).
    ///
    /// This is server misconfiguration, not a rejection of the request: an HTTP
    /// handler answering a provider subscription handshake should return `500`
    /// so the provider keeps retrying while the operator fixes the deployment.
    #[error("{channel} adapter has no verification secret configured (failing closed)")]
    MissingSecret {
        /// Channel whose adapter is misconfigured.
        channel: Channel,
    },

    /// The configured secret is shorter than the crate's documented minimum.
    ///
    /// ```
    /// use messenger_webhook::Error;
    ///
    /// let error = Error::WeakSecret {
    ///     actual: 15,
    ///     minimum: 16,
    /// };
    /// assert_eq!(
    ///     error.to_string(),
    ///     "secret length is 15 bytes, below the 16 byte minimum"
    /// );
    /// ```
    #[error("secret length is {actual} bytes, below the {minimum} byte minimum")]
    WeakSecret {
        /// Actual secret length in bytes.
        actual: usize,
        /// Minimum accepted secret length in bytes.
        minimum: usize,
    },

    /// The verification header is absent.
    #[error("{channel} request carries no {header:?} header")]
    MissingVerificationHeader {
        /// Channel whose header is missing.
        channel: Channel,
        /// Header name that was looked for, lower-case.
        header: &'static str,
    },

    /// The verification header appears more than once.
    ///
    /// HTTP allows a field to repeat, so a proxy or attacker can forward two
    /// copies of a security header. Picking one would make the authentication
    /// result depend on insertion order; this crate fails closed instead.
    #[error("{channel} request carries {count} {header:?} headers; expected exactly one")]
    DuplicateVerificationHeader {
        /// Channel whose header is duplicated.
        channel: Channel,
        /// Header name that was looked for, lower-case.
        header: &'static str,
        /// Number of matching header fields.
        count: usize,
    },

    /// The verification header is present but not shaped as the provider
    /// documents it.
    #[error("{channel} verification header is malformed: {reason}")]
    MalformedVerificationHeader {
        /// Channel whose header is malformed.
        channel: Channel,
        /// What exactly did not match the expected shape. Never the received
        /// value: that is attacker-controlled and would end up in a log.
        reason: String,
    },

    /// A provider's webhook-subscription request is missing a parameter or has
    /// one in a shape the provider does not document.
    ///
    /// This is the `GET` side of a webhook (Meta calls it a verification
    /// request), so the defect is in the query string rather than in a header.
    #[error("{channel} subscription request is malformed: {reason}")]
    MalformedVerificationRequest {
        /// Channel whose subscription request is malformed.
        channel: Channel,
        /// Which parameter is missing or wrong, and how. Never the received
        /// value: that is attacker-controlled and would end up in a log.
        reason: String,
    },

    /// The signature did not match the body.
    #[error("{channel} signature verification failed")]
    BadSignature {
        /// Channel whose signature did not verify.
        channel: Channel,
    },

    /// HMAC-SHA256 could not be constructed.
    ///
    /// This is an internal configuration or dependency fault, not a forged
    /// request: verification failures caused by a signature mismatch remain
    /// [`Error::BadSignature`].
    ///
    /// ```
    /// use messenger_webhook::Error;
    ///
    /// let error = Error::Hmac {
    ///     source: hmac::digest::InvalidLength,
    /// };
    /// assert_eq!(error.to_string(), "could not construct HMAC-SHA256");
    /// assert!(std::error::Error::source(&error).is_some());
    /// ```
    #[error("could not construct HMAC-SHA256")]
    Hmac {
        /// The underlying HMAC construction failure.
        #[source]
        source: hmac::digest::InvalidLength,
    },

    /// The message cannot be expressed in the provider's outbound schema.
    #[error("{channel} cannot encode this message: {reason}")]
    InvalidMessage {
        /// Target channel.
        channel: Channel,
        /// What the provider would reject.
        reason: String,
    },

    /// A transport failed before the provider could apply the request.
    ///
    /// This is retryable: nothing is known to have been applied, so an adapter
    /// can report a connection, DNS or TLS failure here and let the caller
    /// retry the same request. If the provider might have processed the write,
    /// use [`Error::Ambiguous`] instead.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Error};
    ///
    /// let error = Error::Transport {
    ///     channel: Channel::Telegram,
    ///     reason: "connection refused".to_owned(),
    /// };
    /// assert!(error.is_retryable());
    /// assert!(!error.is_ambiguous());
    /// ```
    #[error("transport failed before the request was applied for {channel}: {reason}")]
    Transport {
        /// Channel whose adapter saw the transport failure.
        channel: Channel,
        /// Transport-level reason. Never include credentials or request bodies.
        reason: String,
    },

    /// The provider may have applied the request, and the adapter cannot tell.
    ///
    /// A read timeout or a `5xx` after the provider might have accepted the
    /// write is ambiguous. Do **not** retry blindly with a fresh idempotency
    /// key: repeat the same key, when one exists, or reconcile the outcome with
    /// the provider before deciding what to do next.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Error, IdempotencyKey};
    ///
    /// let error = Error::Ambiguous {
    ///     channel: Channel::Matrix,
    ///     idempotency_key: Some(IdempotencyKey::new("txn-1")?),
    ///     reason: "read timeout".to_owned(),
    /// };
    /// assert!(!error.is_retryable());
    /// assert!(error.is_ambiguous());
    /// assert_eq!(error.idempotency_key().map(IdempotencyKey::as_str), Some("txn-1"));
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[error(
        "the request may have been applied for {channel}: {reason}{suffix}",
        suffix = idempotency_key_suffix(.idempotency_key.as_ref())
    )]
    Ambiguous {
        /// Channel whose adapter saw the uncertain outcome.
        channel: Channel,
        /// Idempotency key that must be reused if the caller repeats the
        /// request, when the adapter had one.
        idempotency_key: Option<IdempotencyKey>,
        /// Why the outcome is unknown. Never include credentials or request
        /// bodies.
        reason: String,
    },

    /// The message is longer than the provider accepts.
    ///
    /// ```
    /// # #[cfg(feature = "telegram")]
    /// # fn demo() -> Result<(), messenger_webhook::Error> {
    /// use messenger_webhook::{
    ///     telegram::MAX_TEXT_UTF16, Adapter, Channel, Error, LimitUnit, OutboundMessage,
    ///     TelegramAdapter,
    /// };
    ///
    /// let adapter = TelegramAdapter::new("bot-secret-bot-secret")?;
    /// let at_limit = OutboundMessage::text(
    ///     Channel::Telegram,
    ///     "-1001",
    ///     "a".repeat(MAX_TEXT_UTF16),
    /// );
    /// assert!(adapter.encode(&at_limit).is_ok());
    ///
    /// let over_limit = OutboundMessage::text(
    ///     Channel::Telegram,
    ///     "-1001",
    ///     "a".repeat(MAX_TEXT_UTF16 + 1),
    /// );
    /// assert!(matches!(
    ///     adapter.encode(&over_limit),
    ///     Err(Error::MessageTooLong {
    ///         channel: Channel::Telegram,
    ///         limit: MAX_TEXT_UTF16,
    ///         actual,
    ///         unit: LimitUnit::Utf16Units,
    ///     }) if actual == MAX_TEXT_UTF16 + 1
    /// ));
    /// # Ok(())
    /// # }
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[error("{channel} message is {actual} {unit} long, the limit is {limit}")]
    MessageTooLong {
        /// Target channel.
        channel: Channel,
        /// Provider limit, in `unit`.
        limit: usize,
        /// Measured length of the message.
        actual: usize,
        /// Unit the limit is expressed in.
        unit: LimitUnit,
    },

    /// An idempotency key is empty or longer than the crate carries.
    #[error("invalid idempotency key: {reason}")]
    InvalidIdempotencyKey {
        /// Why the key was rejected.
        reason: String,
    },

    /// A header name or value cannot be carried as an HTTP header field.
    ///
    /// ```
    /// use messenger_webhook::{Error, Headers};
    ///
    /// let mut headers = Headers::new();
    /// let error = headers.insert("", "value").unwrap_err();
    /// assert!(matches!(error, Error::InvalidHeader { .. }));
    /// assert_eq!(error.to_string(), "invalid header: header name is empty");
    /// ```
    #[error("invalid header: {reason}")]
    InvalidHeader {
        /// Why the header was rejected.
        reason: String,
    },

    /// A custom channel name is invalid.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Error};
    ///
    /// let error = Channel::other("").unwrap_err();
    /// assert!(matches!(error, Error::InvalidChannelName { .. }));
    /// assert_eq!(error.to_string(), "invalid channel name: channel name is empty");
    /// ```
    #[error("invalid channel name: {reason}")]
    InvalidChannelName {
        /// Why the name was rejected.
        reason: String,
    },

    /// A reaction key is missing, empty, or shaped such that it cannot identify
    /// a reaction.
    ///
    /// ```
    /// use messenger_webhook::{Error, Reaction};
    ///
    /// let error = Reaction::add("wamid.1", "").unwrap_err();
    /// assert!(matches!(error, Error::InvalidReactionKey { .. }));
    /// assert_eq!(
    ///     error.to_string(),
    ///     "invalid reaction key: an added reaction key must contain a visible character"
    /// );
    /// ```
    #[error("invalid reaction key: {reason}")]
    InvalidReactionKey {
        /// Why the key was rejected.
        reason: String,
    },

    /// An interactive reply option is missing an id or shaped such that a tap
    /// could not be answered.
    ///
    /// ```
    /// use messenger_webhook::{Error, InteractiveReply};
    ///
    /// let error = InteractiveReply::new("", "Yes").unwrap_err();
    /// assert!(matches!(error, Error::InvalidInteractiveReply { .. }));
    /// assert_eq!(
    ///     error.to_string(),
    ///     "invalid interactive reply: option id must contain a visible character"
    /// );
    /// ```
    #[error("invalid interactive reply: {reason}")]
    InvalidInteractiveReply {
        /// Why the option was rejected.
        reason: String,
    },

    /// No adapter is registered for the requested channel.
    #[error("no adapter registered for channel {0}")]
    UnknownChannel(Channel),

    /// A channel already has an adapter in a [`crate::Registry`].
    #[error("adapter already registered for channel {0}")]
    ChannelAlreadyRegistered(Channel),
}

impl Error {
    /// Whether the failure is a server-side misconfiguration.
    ///
    /// Webhook HTTP handlers usually split verification failures into two
    /// buckets: a request that cannot be authenticated from the provider is a
    /// client rejection, while a missing credential is the endpoint operator's
    /// fault. Return `500` for `true`, so the provider keeps retrying while the
    /// configuration is fixed; return `403` for a subscription request when
    /// this is `false`.
    ///
    /// Currently true only for [`Error::MissingSecret`].
    ///
    /// ```
    /// use messenger_webhook::{Channel, Error};
    ///
    /// let error = Error::MissingSecret {
    ///     channel: Channel::WhatsApp,
    /// };
    /// assert!(error.is_misconfiguration());
    ///
    /// let error = Error::BadSignature {
    ///     channel: Channel::WhatsApp,
    /// };
    /// assert!(!error.is_misconfiguration());
    /// ```
    #[must_use]
    pub const fn is_misconfiguration(&self) -> bool {
        matches!(self, Self::MissingSecret { .. })
    }

    /// Whether the same request could succeed if it were retried unchanged.
    ///
    /// `true` only for [`Error::Transport`], where nothing is known to have
    /// been applied. An [`Error::Ambiguous`] is not retryable through this
    /// method because a blind retry may duplicate work; use
    /// [`Error::is_ambiguous`] and repeat with the same idempotency key.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Error};
    ///
    /// let transport = Error::Transport {
    ///     channel: Channel::WhatsApp,
    ///     reason: "dns failure".to_owned(),
    /// };
    /// assert!(transport.is_retryable());
    ///
    /// let ambiguous = Error::Ambiguous {
    ///     channel: Channel::WhatsApp,
    ///     idempotency_key: None,
    ///     reason: "timeout".to_owned(),
    /// };
    /// assert!(!ambiguous.is_retryable());
    ///
    /// assert!(!Error::Payload("not a JSON object".into()).is_retryable());
    /// ```
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport { .. })
    }

    /// Whether the request may have been applied.
    ///
    /// Only [`Error::Ambiguous`] is ambiguous. A caller must not treat it as a
    /// terminal request error: the write may already be in the provider's
    /// system.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Error};
    ///
    /// let error = Error::Ambiguous {
    ///     channel: Channel::Telegram,
    ///     idempotency_key: None,
    ///     reason: "timeout".to_owned(),
    /// };
    /// assert!(error.is_ambiguous());
    /// ```
    #[must_use]
    pub const fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous { .. })
    }

    /// The idempotency key of an ambiguous delivery outcome, when there is one.
    ///
    /// Repeat the request with this key instead of minting a new one, or use it
    /// to reconcile the provider-side outcome.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Error, IdempotencyKey};
    ///
    /// let key = IdempotencyKey::new("txn-1")?;
    /// let error = Error::Ambiguous {
    ///     channel: Channel::Matrix,
    ///     idempotency_key: Some(key.clone()),
    ///     reason: "timeout".to_owned(),
    /// };
    /// assert_eq!(error.idempotency_key(), Some(&key));
    ///
    /// let transport = Error::Transport {
    ///     channel: Channel::Matrix,
    ///     reason: "dns failure".to_owned(),
    /// };
    /// assert!(transport.idempotency_key().is_none());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn idempotency_key(&self) -> Option<&IdempotencyKey> {
        match self {
            Self::Ambiguous {
                idempotency_key, ..
            } => idempotency_key.as_ref(),
            _ => None,
        }
    }
}

fn idempotency_key_suffix(idempotency_key: Option<&IdempotencyKey>) -> String {
    idempotency_key.map_or_else(String::new, |key| {
        format!(" (repeat with idempotency key `{key}`)")
    })
}

impl From<hmac::digest::InvalidLength> for Error {
    fn from(source: hmac::digest::InvalidLength) -> Self {
        Self::Hmac { source }
    }
}

/// Convenience alias for the crate's [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::Error;
    use crate::{check_body, MAX_BODY_BYTES};

    #[test]
    fn body_too_large_boundary_matches_the_variant_contract() {
        assert!(check_body(MAX_BODY_BYTES - 1, MAX_BODY_BYTES).is_ok());
        assert!(check_body(MAX_BODY_BYTES, MAX_BODY_BYTES).is_ok());
        assert!(matches!(
            check_body(MAX_BODY_BYTES + 1, MAX_BODY_BYTES),
            Err(Error::BodyTooLarge {
                limit: MAX_BODY_BYTES,
                actual,
            }) if actual == MAX_BODY_BYTES + 1
        ));
    }
}
