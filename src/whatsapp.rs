//! `WhatsApp` Cloud API adapter.
//!
//! Meta posts a batch of events to one endpoint, so messages and delivery
//! receipts arrive in the same `change` objects. Both are normalised here:
//! receipts come back as [`NormalizedMessage`] with
//! [`NormalizedMessage::status`] set and no text.
//!
//! Outbound, the same adapter turns a normalised message into the Cloud API
//! request body and refuses to produce one the provider would reject:
//!
//! ```
//! use messenger_webhook::{Adapter, Channel, NormalizedMessage, WhatsAppAdapter};
//!
//! let message = NormalizedMessage::text(Channel::WhatsApp, "4915112345678", "Hallo");
//! let payload = WhatsAppAdapter::new("app-secret").encode(&message)?;
//! assert_eq!(payload.json()?["text"]["body"], "Hallo");
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

use serde_json::Value;

use crate::error::{Error, Result};
use crate::headers::Headers;
use crate::json;
use crate::model::{
    Attachment, AttachmentKind, Channel, DeliveryStatus, NormalizedMessage, ProviderPayload,
    Reaction,
};
use crate::secret::Secret;
use crate::signature::{self, META_SIGNATURE_HEADER};
use crate::{check_body, json_payload, recipient, Adapter, MAX_BODY_BYTES};

/// Maximum length of a `WhatsApp` text body, in bytes.
///
/// ```
/// assert_eq!(messenger_webhook::whatsapp::MAX_TEXT_BYTES, 4096);
/// ```
pub const MAX_TEXT_BYTES: usize = 4096;

/// Maximum length of a `WhatsApp` media caption, in bytes.
///
/// ```
/// assert_eq!(messenger_webhook::whatsapp::MAX_CAPTION_BYTES, 1024);
/// ```
pub const MAX_CAPTION_BYTES: usize = 1024;

/// Adapter for Meta's `WhatsApp` Cloud API webhooks.
///
/// The constructor takes the app secret; every inbound call has to be verified
/// before it is parsed.
///
/// ```
/// use messenger_webhook::{signature::meta_signature, Adapter, Headers, Secret, WhatsAppAdapter};
///
/// let secret = Secret::new("app-secret");
/// let adapter = WhatsAppAdapter::new("app-secret");
/// let body = br#"{"entry":[{"changes":[{"value":{"messages":[
///     {"id":"wamid.1","from":"4915112345678","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;
///
/// let mut headers = Headers::new();
/// headers.insert("x-hub-signature-256", meta_signature(&secret, body)?);
/// adapter.verify(&headers, body)?;
///
/// let messages = adapter.adapt(body)?;
/// assert_eq!(messages[0].text.as_deref(), Some("Hallo"));
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct WhatsAppAdapter {
    secret: Secret,
}

impl WhatsAppAdapter {
    /// Creates an adapter that verifies with `app_secret` — the app secret from
    /// the Meta app dashboard.
    ///
    /// ```
    /// use messenger_webhook::{Adapter, WhatsAppAdapter};
    ///
    /// assert_eq!(WhatsAppAdapter::new("s").channel().as_str(), "whatsapp");
    /// ```
    #[must_use]
    pub fn new(app_secret: impl Into<String>) -> Self {
        Self {
            secret: Secret::new(app_secret),
        }
    }

    /// The header Meta signs with.
    ///
    /// ```
    /// use messenger_webhook::WhatsAppAdapter;
    ///
    /// assert_eq!(WhatsAppAdapter::SIGNATURE_HEADER, "x-hub-signature-256");
    /// ```
    pub const SIGNATURE_HEADER: &'static str = META_SIGNATURE_HEADER;
}

impl Adapter for WhatsAppAdapter {
    fn channel(&self) -> Channel {
        Channel::WhatsApp
    }

    fn verify(&self, headers: &Headers, body: &[u8]) -> Result<()> {
        // Size before hashing: never spend work on an oversized body.
        check_body(MAX_BODY_BYTES, body.len())?;
        if self.secret.is_empty() {
            return Err(Error::MissingSecret {
                channel: Channel::WhatsApp,
            });
        }
        let header =
            headers
                .get(META_SIGNATURE_HEADER)
                .ok_or(Error::MissingVerificationHeader {
                    channel: Channel::WhatsApp,
                    header: META_SIGNATURE_HEADER,
                })?;
        signature::verify_meta(&self.secret, header, body)
    }

    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
        check_body(MAX_BODY_BYTES, payload.len())?;
        let root: Value =
            serde_json::from_slice(payload).map_err(|error| Error::Payload(error.to_string()))?;
        if !root.is_object() {
            return Err(Error::Payload("expected a JSON object".to_owned()));
        }

        let mut events = Vec::new();
        for entry in json::array(&root, "entry") {
            for change in json::array(entry, "changes") {
                let Some(value) = json::child(change, "value") else {
                    continue;
                };
                for message in json::array(value, "messages") {
                    events.extend(normalize_message(message));
                }
                for status in json::array(value, "statuses") {
                    events.extend(normalize_status(status));
                }
            }
        }
        Ok(events)
    }

    fn encode(&self, message: &NormalizedMessage) -> Result<ProviderPayload> {
        let to = recipient(Channel::WhatsApp, message)?;
        if !to.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::InvalidMessage {
                channel: Channel::WhatsApp,
                reason: format!("WhatsApp recipients are E.164 digits, got {to:?}"),
            });
        }

        let text = message.text.as_deref().unwrap_or_default();
        let mut body = serde_json::Map::new();
        body.insert(
            "messaging_product".to_owned(),
            Value::String("whatsapp".to_owned()),
        );
        body.insert(
            "recipient_type".to_owned(),
            Value::String("individual".to_owned()),
        );
        body.insert("to".to_owned(), Value::String(to.to_owned()));

        if let Some(attachment) = message.first_attachment() {
            let field = match attachment.kind {
                AttachmentKind::Image => "image",
                AttachmentKind::Document => "document",
                AttachmentKind::Audio | AttachmentKind::Voice => "audio",
                AttachmentKind::Video => "video",
                AttachmentKind::Sticker => "sticker",
                ref other => {
                    return Err(Error::InvalidMessage {
                        channel: Channel::WhatsApp,
                        reason: format!("WhatsApp cannot send {} attachments", other.as_str()),
                    });
                }
            };

            let mut media = serde_json::Map::new();
            if let Some(url) = &attachment.url {
                media.insert("link".to_owned(), Value::String(url.clone()));
            } else if let Some(id) = &attachment.id {
                media.insert("id".to_owned(), Value::String(id.clone()));
            } else {
                return Err(Error::InvalidMessage {
                    channel: Channel::WhatsApp,
                    reason: "attachment has neither a url nor an id".to_owned(),
                });
            }
            if field == "document" {
                if let Some(filename) = &attachment.filename {
                    media.insert("filename".to_owned(), Value::String(filename.clone()));
                }
            }
            if !text.is_empty() {
                let length = text.len();
                if length > MAX_CAPTION_BYTES {
                    return Err(Error::MessageTooLong {
                        channel: Channel::WhatsApp,
                        limit: MAX_CAPTION_BYTES,
                        actual: length,
                        unit: "bytes",
                    });
                }
                media.insert("caption".to_owned(), Value::String(text.to_owned()));
            }
            body.insert("type".to_owned(), Value::String(field.to_owned()));
            body.insert(field.to_owned(), Value::Object(media));
        } else {
            if text.is_empty() {
                return Err(Error::InvalidMessage {
                    channel: Channel::WhatsApp,
                    reason: "message has neither text nor an attachment".to_owned(),
                });
            }
            if message.edit_of.is_some() || message.reaction.is_some() {
                return Err(Error::InvalidMessage {
                    channel: Channel::WhatsApp,
                    reason: "edits and reactions need the Cloud API message id, not a send"
                        .to_owned(),
                });
            }
            let length = text.len();
            if length > MAX_TEXT_BYTES {
                return Err(Error::MessageTooLong {
                    channel: Channel::WhatsApp,
                    limit: MAX_TEXT_BYTES,
                    actual: length,
                    unit: "bytes",
                });
            }
            let mut text_body = serde_json::Map::new();
            text_body.insert("body".to_owned(), Value::String(text.to_owned()));
            body.insert("type".to_owned(), Value::String("text".to_owned()));
            body.insert("text".to_owned(), Value::Object(text_body));
        }

        json_payload(Channel::WhatsApp, "POST", &Value::Object(body))
    }
}

/// Maps one `messages[]` object, or drops it when it has no id to key on.
fn normalize_message(message: &Value) -> Option<NormalizedMessage> {
    let external_id = json::id(message, "id")?;
    let sender_id = json::id(message, "from")?;
    let provider_type = json::string(message, "type")
        .unwrap_or("unknown")
        .to_owned();

    let mut normalized = NormalizedMessage {
        channel: Channel::WhatsApp,
        conversation_id: sender_id.clone(),
        sender_id,
        external_id,
        text: None,
        timestamp: json::integer(message, "timestamp"),
        attachments: Vec::new(),
        reply_to: json::child(message, "context").and_then(|context| json::id(context, "id")),
        thread_id: None,
        edit_of: None,
        reaction: None,
        status: None,
        provider_type: Some(provider_type.clone()),
        raw: Some(message.clone()),
    };

    match provider_type.as_str() {
        "text" => {
            normalized.text = json::child(message, "text")
                .and_then(|text| json::string(text, "body"))
                .map(str::to_owned);
        }
        "image" => push_media(&mut normalized, message, "image", AttachmentKind::Image),
        "document" => push_media(
            &mut normalized,
            message,
            "document",
            AttachmentKind::Document,
        ),
        "audio" => {
            let is_voice =
                json::child(message, "audio").is_some_and(|audio| json::boolean(audio, "voice"));
            let kind = if is_voice {
                AttachmentKind::Voice
            } else {
                AttachmentKind::Audio
            };
            push_media(&mut normalized, message, "audio", kind);
        }
        "video" => push_media(&mut normalized, message, "video", AttachmentKind::Video),
        "sticker" => push_media(&mut normalized, message, "sticker", AttachmentKind::Sticker),
        "location" => {
            if let Some(location) = json::child(message, "location") {
                let mut attachment = Attachment::new(AttachmentKind::Location);
                attachment.caption = json::string(location, "name")
                    .or_else(|| json::string(location, "address"))
                    .map(str::to_owned);
                normalized.attachments.push(attachment);
            }
        }
        "contacts" => {
            if let Some(contact) = json::array(message, "contacts").first() {
                let mut attachment = Attachment::new(AttachmentKind::Contact);
                attachment.caption = json::child(contact, "name")
                    .and_then(|name| json::string(name, "formatted_name"))
                    .map(str::to_owned);
                normalized.attachments.push(attachment);
            }
        }
        "reaction" => {
            // The target message and the emoji live in the nested `reaction`
            // object, not on the message itself.
            if let Some(reaction) = json::child(message, "reaction") {
                if let Some(target) = json::id(reaction, "message_id") {
                    let emoji = json::string(reaction, "emoji")
                        .unwrap_or_default()
                        .to_owned();
                    normalized.reaction = Some(Reaction {
                        message_id: target,
                        removed: emoji.is_empty(),
                        emoji,
                    });
                }
            }
        }
        // Buttons, interactive replies, orders, system notices and future
        // types keep their provider name and full payload in `raw`.
        _ => {}
    }

    Some(normalized)
}

/// Maps a media object (`image`, `document`, `audio`, `video`, `sticker`).
fn media_attachment(message: &Value, field: &str, kind: AttachmentKind) -> Option<Attachment> {
    let media = json::child(message, field)?;

    let mut attachment = Attachment::new(kind);
    attachment.id = json::id(media, "id");
    attachment.url = json::string(media, "link").map(str::to_owned);
    attachment.mime_type = json::string(media, "mime_type").map(str::to_owned);
    attachment.filename = json::string(media, "filename").map(str::to_owned);
    attachment.caption = json::string(media, "caption").map(str::to_owned);
    // WhatsApp reports no size, width or duration for inbound media.
    Some(attachment)
}

/// Appends media to the event, lifting its caption into `text`.
fn push_media(
    normalized: &mut NormalizedMessage,
    message: &Value,
    field: &str,
    kind: AttachmentKind,
) {
    if let Some(attachment) = media_attachment(message, field, kind) {
        normalized.text.clone_from(&attachment.caption);
        normalized.attachments.push(attachment);
    }
}

/// Maps one `statuses[]` entry (delivery receipt) onto a status event.
fn normalize_status(status: &Value) -> Option<NormalizedMessage> {
    let external_id = json::id(status, "id")?;
    let recipient = json::id(status, "recipient_id")?;
    let state = json::string(status, "status").unwrap_or("unknown");

    Some(NormalizedMessage {
        channel: Channel::WhatsApp,
        conversation_id: recipient.clone(),
        sender_id: recipient,
        external_id,
        text: None,
        timestamp: json::integer(status, "timestamp"),
        attachments: Vec::new(),
        reply_to: None,
        thread_id: None,
        edit_of: None,
        reaction: None,
        status: Some(delivery_status(state)),
        provider_type: Some("status".to_owned()),
        raw: Some(status.clone()),
    })
}

fn delivery_status(state: &str) -> DeliveryStatus {
    match state {
        "sent" => DeliveryStatus::Sent,
        "delivered" => DeliveryStatus::Delivered,
        "read" => DeliveryStatus::Read,
        "failed" => DeliveryStatus::Failed,
        "deleted" => DeliveryStatus::Deleted,
        other => DeliveryStatus::Unknown(other.to_owned()),
    }
}
