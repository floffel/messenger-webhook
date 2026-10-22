//! Telegram Bot API adapter.

use serde::Deserialize;

use crate::{Adapter, Channel, Error, NormalizedMessage, Result};

#[derive(Deserialize)]
struct Update {
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    message_id: i64,
    chat: Chat,
    #[serde(default)]
    from: Option<User>,
    #[serde(default)]
    date: i64,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct Chat {
    id: i64,
}

#[derive(Deserialize)]
struct User {
    id: i64,
}

/// Returns the Telegram adapter.
#[must_use]
pub fn adapter() -> TelegramAdapter {
    TelegramAdapter
}

/// Maps Telegram updates to [`NormalizedMessage`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TelegramAdapter;

impl Adapter for TelegramAdapter {
    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
        let update: Update =
            serde_json::from_slice(payload).map_err(|e| Error::Payload(e.to_string()))?;

        let Some(message) = update.message else {
            return Ok(Vec::new());
        };
        let sender = message
            .from
            .as_ref()
            .map_or_else(|| message.chat.id.to_string(), |u| u.id.to_string());

        Ok(vec![NormalizedMessage {
            channel: Channel::Telegram,
            external_id: message.message_id.to_string(),
            conversation_id: message.chat.id.to_string(),
            sender_id: sender,
            text: message.text,
            timestamp: Some(message.date),
        }])
    }
}
