//! `WhatsApp` Cloud API adapter.

use serde::Deserialize;

use crate::{Adapter, Channel, Error, NormalizedMessage, Result};

#[derive(Deserialize)]
struct Payload {
    #[serde(default)]
    entry: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    changes: Vec<Change>,
}

#[derive(Deserialize)]
struct Change {
    #[serde(default)]
    value: Value,
}

#[derive(Deserialize, Default)]
struct Value {
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: String,
    from: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    text: Option<Text>,
}

#[derive(Deserialize)]
struct Text {
    body: String,
}

/// Returns the `WhatsApp` adapter.
#[must_use]
pub fn adapter() -> WhatsAppAdapter {
    WhatsAppAdapter
}

/// Maps `WhatsApp` webhook payloads to [`NormalizedMessage`].
#[derive(Debug, Clone, Copy, Default)]
pub struct WhatsAppAdapter;

impl Adapter for WhatsAppAdapter {
    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
        let parsed: Payload =
            serde_json::from_slice(payload).map_err(|e| Error::Payload(e.to_string()))?;

        Ok(parsed
            .entry
            .into_iter()
            .flat_map(|e| e.changes)
            .flat_map(|c| c.value.messages)
            .map(|m| NormalizedMessage {
                channel: Channel::WhatsApp,
                conversation_id: m.from.clone(),
                sender_id: m.from,
                external_id: m.id,
                text: m.text.map(|t| t.body),
                timestamp: m.timestamp.and_then(|t| t.parse().ok()),
            })
            .collect())
    }
}
