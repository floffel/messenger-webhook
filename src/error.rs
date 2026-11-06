//! Typed errors.
//!
//! Every failure this crate reports is **terminal**: the caller has to fix the
//! payload, the headers or the configuration before retrying. Signature and
//! size failures fail closed, so a request that cannot be proven authentic
//! never reaches a parser.

use thiserror::Error;

use crate::model::Channel;

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
    /// The body is not JSON, or not the JSON shape the provider sends.
    #[error("invalid payload: {0}")]
    Payload(String),

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

    /// No adapter is registered for the requested channel.
    #[error("no adapter registered for channel {0}")]
    UnknownChannel(Channel),

    /// A channel already has an adapter in a [`crate::Registry`].
    #[error("adapter already registered for channel {0}")]
    ChannelAlreadyRegistered(Channel),
}

impl Error {
    /// Whether the same request could succeed if it were retried unchanged.
    ///
    /// Always `false`: every error here is terminal and needs a fix in the
    /// payload or the configuration, not a retry.
    ///
    /// ```
    /// use messenger_webhook::Error;
    ///
    /// assert!(!Error::Payload("not json".into()).is_retryable());
    /// ```
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        false
    }
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
        assert!(check_body(MAX_BODY_BYTES - 1).is_ok());
        assert!(check_body(MAX_BODY_BYTES).is_ok());
        assert!(matches!(
            check_body(MAX_BODY_BYTES + 1),
            Err(Error::BodyTooLarge {
                limit: MAX_BODY_BYTES,
                actual,
            }) if actual == MAX_BODY_BYTES + 1
        ));
    }
}
