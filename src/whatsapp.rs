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
///
/// ```
/// use messenger_webhook::Adapter;
///
/// let payload = br#"{"object":"whatsapp_business_account","entry":[{"id":"1","changes":[{"field":"messages",
///   "value":{"metadata":{"display_phone_number":"1","phone_number_id":"2"},
///            "messages":[{"id":"wamid.1","from":"4915112345678","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;
///
/// let messages = messenger_webhook::whatsapp::adapter().adapt(payload)?;
/// assert_eq!(messages[0].text.as_deref(), Some("Hallo"));
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
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
