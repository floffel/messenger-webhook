//! Typed errors.
//!
//! Every failure this crate reports is **terminal**: the caller has to fix the
//! payload, the headers or the configuration before retrying. Signature and
//! size failures fail closed, so a request that cannot be proven authentic
//! never reaches a parser.

use thiserror::Error;

use crate::model::Channel;

/// Everything that can go wrong while verifying, normalising or encoding.
///
/// ```
/// use messenger_webhook::{Channel, Error};
///
/// let error = Error::UnknownChannel(Channel::Other("signal".into()));
/// assert_eq!(error.to_string(), "no adapter registered for channel signal");
/// assert!(!error.is_retryable());
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
    #[error("{channel} message is {actual} {unit} long, the limit is {limit}")]
    MessageTooLong {
        /// Target channel.
        channel: Channel,
        /// Provider limit, in `unit`.
        limit: usize,
        /// Measured length of the message.
        actual: usize,
        /// Unit the limit is expressed in: `UTF-16 units` or `bytes`.
        unit: &'static str,
    },

    /// An idempotency key is empty or longer than the crate carries.
    #[error("invalid idempotency key: {reason}")]
    InvalidIdempotencyKey {
        /// Why the key was rejected.
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
