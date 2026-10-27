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

    /// The body is larger than the adapter's ceiling; it is rejected before
    /// any hashing or deserialisation happens.
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

    /// The verification header is present but not shaped as the provider
    /// documents it.
    #[error("{channel} verification header is malformed: {reason}")]
    MalformedVerificationHeader {
        /// Channel whose header is malformed.
        channel: Channel,
        /// What exactly did not match the expected shape.
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

    /// No adapter is registered for the requested channel.
    #[error("no adapter registered for channel {0}")]
    UnknownChannel(Channel),
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

/// Convenience alias for the crate's [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;
