//! Normalise inbound messenger webhooks into one message type.
//!
//! Every provider reports the same facts in a different shape. This crate maps
//! them onto [`NormalizedMessage`] so a consumer has one code path for routing,
//! persistence and threading — and one place where provider quirks are handled.
//!
//! ```
//! use messenger_webhook::{Adapter, NormalizedMessage};
//!
//! let payload = br#"{"object":"whatsapp_business_account","entry":[{"id":"1","changes":[{"field":"messages",
//!   "value":{"metadata":{"display_phone_number":"1","phone_number_id":"2"},
//!            "messages":[{"id":"wamid.1","from":"4915112345678","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;
//!
//! let messages = messenger_webhook::whatsapp::adapter().adapt(payload)?;
//! assert_eq!(messages[0].text.as_deref(), Some("Hallo"));
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod error;
#[cfg(feature = "telegram")]
pub mod telegram;
#[cfg(feature = "whatsapp")]
pub mod whatsapp;

pub use error::{Error, Result};

/// Which messenger a message came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    /// `WhatsApp` Cloud API.
    WhatsApp,
    /// `Telegram` Bot API.
    Telegram,
    /// Matrix.
    Matrix,
    /// Anything else, verbatim.
    Other(String),
}

/// One inbound message, provider independent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedMessage {
    /// Origin channel.
    pub channel: Channel,
    /// Provider message id — use it as the idempotency key.
    pub external_id: String,
    /// Conversation the message belongs to (chat id, thread, room).
    pub conversation_id: String,
    /// Who sent it, in the provider's own id space.
    pub sender_id: String,
    /// Plain text, when the message has any.
    pub text: Option<String>,
    /// Unix timestamp in seconds, when the provider sent one.
    pub timestamp: Option<i64>,
}

/// Turns a provider payload into normalised messages.
pub trait Adapter {
    /// Parses `payload` and returns every inbound message it contains.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Payload`] when the body is not the provider's JSON shape.
    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>>;
}
