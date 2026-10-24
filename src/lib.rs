//! Normalise messenger webhooks into one message type — and encode them back.
//!
//! Every provider reports the same facts in a different shape. This crate maps
//! `WhatsApp` Cloud API, Telegram Bot API and Matrix events onto
//! [`NormalizedMessage`], so a consumer has one code path for routing,
//! persistence and threading — and one place where provider quirks live:
//!
//! | provider | inbound | signature | outbound |
//! |----------|---------|-----------|----------|
//! | `WhatsAppAdapter` | `entry[].changes[].value.messages` and `statuses` | HMAC-SHA256 (`X-Hub-Signature-256`) | text, media link |
//! | `TelegramAdapter` | `message`, edits, posts, reactions | `X-Telegram-Bot-Api-Secret-Token` | `sendMessage` / media |
//! | `MatrixAdapter` | `m.room.message`, reactions, receipts, unknown types | `Authorization: Bearer` | `m.room.message` |
//!
//! Each provider adapter lives in its own feature-gated module (`whatsapp`,
//! `telegram`, `matrix`); everything in this module is available regardless of
//! the feature set.
//!
//! Verification is part of the adapter contract, not an afterthought: an
//! adapter cannot be constructed without a secret, and [`Adapter::verify`]
//! fails closed on an empty secret, a missing header or an oversized body.
//!
//! ```
//! use messenger_webhook::{signature::meta_signature, Headers, Secret};
//!
//! // What a provider-side test harness does, or what a mock server returns.
//! let secret = Secret::new("app-secret");
//! let body = br#"{"entry":[]}"#;
//! let mut headers = Headers::new();
//! headers.insert("X-Hub-Signature-256", meta_signature(&secret, body)?);
//!
//! assert_eq!(headers.get("x-hub-signature-256").map(str::len), Some(71));
//! # Ok::<(), messenger_webhook::Error>(())
//! ```
//!
//! The adapters are constructed with their secret and used through
//! [`Adapter`]: `WhatsAppAdapter::new(app_secret)`, `TelegramAdapter::new(
//! secret_token)`, `MatrixAdapter::new(hs_token)`.
//!
//! `external_id` is the provider's message id and therefore the right
//! idempotency key. Nothing in this crate performs I/O: the caller owns the
//! HTTP server, retries and rate limits.
//!
//! ## Deliberate exclusions
//!
//! Documented rather than silently missing — see the README for the full list:
//! no network calls, no media download, no rich-text or entity parsing, no
//! Matrix end-to-end encryption (`m.room.encrypted` events are carried
//! verbatim), and Meta's deprecated `X-Hub-Signature` (SHA-1) header is
//! rejected in favour of `X-Hub-Signature-256`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod error;
pub mod headers;
pub mod secret;
pub mod signature;

#[cfg(feature = "matrix")]
pub mod matrix;
#[cfg(feature = "telegram")]
pub mod telegram;
#[cfg(feature = "whatsapp")]
pub mod whatsapp;

mod json;
mod model;

#[cfg(feature = "matrix")]
pub use matrix::MatrixAdapter;
#[cfg(feature = "telegram")]
pub use telegram::TelegramAdapter;
#[cfg(feature = "whatsapp")]
pub use whatsapp::WhatsAppAdapter;

pub use error::{Error, Result};
pub use headers::Headers;
pub use model::{Attachment, AttachmentKind, Channel, DeliveryStatus, NormalizedMessage, Reaction};
pub use secret::Secret;

/// Hard ceiling on a request body, enforced before any hashing or JSON parsing.
///
/// Providers cap webhook bodies well below this; the point is to bound the work
/// an unauthenticated caller can make the process do.
///
/// ```
/// use messenger_webhook::MAX_BODY_BYTES;
///
/// assert_eq!(MAX_BODY_BYTES, 1024 * 1024);
/// ```
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Turns provider payloads into normalised messages.
///
/// Implementations are `Send + Sync`, hold no interior mutability and are safe
/// to share across tasks by reference.
///
/// ```
/// use messenger_webhook::{Adapter, Channel, Error, Headers, NormalizedMessage, Result};
///
/// /// An adapter for a provider this crate does not ship.
/// struct Echo;
///
/// impl Adapter for Echo {
///     fn channel(&self) -> Channel {
///         Channel::Other("echo".into())
///     }
///
///     fn verify(&self, _headers: &Headers, body: &[u8]) -> Result<()> {
///         if body.is_empty() {
///             return Err(Error::Payload("empty body".into()));
///         }
///         Ok(())
///     }
///
///     fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
///         let text = String::from_utf8_lossy(payload).into_owned();
///         Ok(vec![NormalizedMessage::text(Channel::Other("echo".into()), "self", text)])
///     }
/// }
///
/// let events = Echo.adapt(b"ping")?;
/// assert_eq!(events[0].text.as_deref(), Some("ping"));
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
pub trait Adapter: Send + Sync {
    /// Which channel this adapter speaks.
    fn channel(&self) -> Channel;

    /// Verifies that `body` really came from the provider.
    ///
    /// # Errors
    ///
    /// - [`Error::BodyTooLarge`] if `body` exceeds [`MAX_BODY_BYTES`], checked
    ///   before any hashing.
    /// - [`Error::MissingSecret`] if the adapter was built without a secret.
    /// - [`Error::MissingVerificationHeader`] if the provider header is absent.
    /// - [`Error::MalformedVerificationHeader`] if the header does not have the
    ///   provider's documented shape.
    /// - [`Error::BadSignature`] if the signature does not match. The
    ///   comparison is constant time.
    fn verify(&self, headers: &Headers, body: &[u8]) -> Result<()>;

    /// Parses `body` and returns every event it contains.
    ///
    /// Unknown fields and unknown event types are carried, not rejected;
    /// entries without an id are dropped because they cannot be used as an
    /// idempotency key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Payload`] when the body is not the provider's JSON
    /// shape, and [`Error::BodyTooLarge`] for oversized bodies.
    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>>;
}

/// Rejects bodies above the crate-wide ceiling.
pub(crate) fn check_body(limit: usize, actual: usize) -> Result<()> {
    if actual > limit {
        Err(Error::BodyTooLarge { limit, actual })
    } else {
        Ok(())
    }
}
