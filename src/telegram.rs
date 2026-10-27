//! Telegram Bot API adapter.
//!
//! One update arrives per POST, but an update can be a new message, an edit, a
//! channel post or a reaction — all of them normalise here. Telegram has no
//! payload signature; it sends a secret token header that the bot registers
//! with `setWebhook`.
//!
//! Outbound, `encode` produces the `sendMessage` (or send-media) body and
//! enforces Telegram's 4096/1024 UTF-16 unit limits:
//!
//! ```
//! use messenger_webhook::{Adapter, Channel, NormalizedMessage, TelegramAdapter};
//!
//! let adapter = TelegramAdapter::new("bot-secret");
//! let message = NormalizedMessage::text(Channel::Telegram, "-1001", "Hallo");
//! assert_eq!(adapter.encode(&message)?.json()?["text"], "Hallo");
//!
//! let too_long = NormalizedMessage::text(Channel::Telegram, "-1001", &"a".repeat(4097));
//! assert!(adapter.encode(&too_long).is_err());
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

use serde_json::Value;

use crate::error::{Error, Result};
use crate::headers::Headers;
use crate::id;
use crate::json;
use crate::model::ProviderPayload;
use crate::model::{Attachment, AttachmentKind, Channel, NormalizedMessage, Reaction};
use crate::secret::Secret;
use crate::{check_body, json_payload, recipient, utf16_len, Adapter, MAX_BODY_BYTES};

/// Maximum length of a Telegram text message, in UTF-16 code units.
///
/// ```
/// assert_eq!(messenger_webhook::telegram::MAX_TEXT_UTF16, 4096);
/// ```
pub const MAX_TEXT_UTF16: usize = 4096;

/// Maximum length of a Telegram media caption, in UTF-16 code units.
///
/// ```
/// assert_eq!(messenger_webhook::telegram::MAX_CAPTION_UTF16, 1024);
/// ```
pub const MAX_CAPTION_UTF16: usize = 1024;

/// Header Telegram sends the registered `secret_token` in.
pub const SECRET_TOKEN_HEADER: &str = "x-telegram-bot-api-secret-token";

/// Adapter for Telegram Bot API updates.
///
/// ```
/// use messenger_webhook::{Adapter, Headers, TelegramAdapter};
///
/// let adapter = TelegramAdapter::new("bot-secret");
/// let body = br#"{"update_id":1,"message":{"message_id":7,"from":{"id":42},
///     "chat":{"id":-1001},"date":1729600000,"text":"Hallo"}}"#;
///
/// let headers = Headers::from_iter([("x-telegram-bot-api-secret-token", "bot-secret")]);
/// adapter.verify(&headers, body)?;
/// assert_eq!(adapter.adapt(body)?[0].conversation_id, "-1001");
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct TelegramAdapter {
    secret: Secret,
}

impl TelegramAdapter {
    /// Creates an adapter that verifies with the `secret_token` given to
    /// `setWebhook`.
    ///
    /// ```
    /// use messenger_webhook::{Adapter, TelegramAdapter};
    ///
    /// assert_eq!(TelegramAdapter::new("s").channel().as_str(), "telegram");
    /// ```
    #[must_use]
    pub fn new(secret_token: impl Into<String>) -> Self {
        Self {
            secret: Secret::new(secret_token),
        }
    }

    /// The header the secret token arrives in.
    ///
    /// ```
    /// use messenger_webhook::telegram::SECRET_TOKEN_HEADER;
    ///
    /// assert_eq!(SECRET_TOKEN_HEADER, "x-telegram-bot-api-secret-token");
    /// ```
    pub const HEADER: &'static str = SECRET_TOKEN_HEADER;
}

impl Adapter for TelegramAdapter {
    fn channel(&self) -> Channel {
        Channel::Telegram
    }

    fn verify(&self, headers: &Headers, body: &[u8]) -> Result<()> {
        check_body(MAX_BODY_BYTES, body.len())?;
        if self.secret.is_empty() {
            return Err(Error::MissingSecret {
                channel: Channel::Telegram,
            });
        }
        let header = headers
            .get(SECRET_TOKEN_HEADER)
            .ok_or(Error::MissingVerificationHeader {
                channel: Channel::Telegram,
                header: SECRET_TOKEN_HEADER,
            })?;
        if self.secret.ct_eq(header.as_bytes()) {
            Ok(())
        } else {
            Err(Error::BadSignature {
                channel: Channel::Telegram,
            })
        }
    }

    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
        check_body(MAX_BODY_BYTES, payload.len())?;
        let root: Value =
            serde_json::from_slice(payload).map_err(|error| Error::Payload(error.to_string()))?;
        if !root.is_object() {
            return Err(Error::Payload("expected a JSON object".to_owned()));
        }

        let mut events = Vec::new();
        for (field, provider_type) in [
            ("message", "message"),
            ("edited_message", "edited_message"),
            ("channel_post", "channel_post"),
            ("edited_channel_post", "edited_channel_post"),
        ] {
            if let Some(message) = json::child(&root, field).filter(|value| value.is_object()) {
                events.push(normalize_message(message, provider_type));
            }
        }
        if let Some(reaction) =
            json::child(&root, "message_reaction").filter(|value| value.is_object())
        {
            events.push(normalize_reaction(reaction));
        }
        Ok(events)
    }

    fn encode(&self, message: &NormalizedMessage) -> Result<ProviderPayload> {
        let chat_id = recipient(Channel::Telegram, message)?;
        let mut body = serde_json::Map::new();
        body.insert("chat_id".to_owned(), Value::String(chat_id.to_owned()));

        let text = message.text.as_deref().unwrap_or_default();
        if let Some(attachment) = message.first_attachment() {
            let field = match attachment.kind {
                AttachmentKind::Image => "photo",
                AttachmentKind::Video => "video",
                AttachmentKind::Audio => "audio",
                AttachmentKind::Voice => "voice",
                AttachmentKind::Document => "document",
                AttachmentKind::Sticker => "sticker",
                ref other => {
                    return Err(Error::InvalidMessage {
                        channel: Channel::Telegram,
                        reason: format!(
                            "Telegram cannot send {} attachments; use a document",
                            other.as_str()
                        ),
                    });
                }
            };
            let reference = attachment
                .id
                .clone()
                .or_else(|| attachment.url.clone())
                .ok_or_else(|| Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "attachment has neither a file id nor a url".to_owned(),
                })?;
            if !text.is_empty() {
                let length = utf16_len(text);
                if length > MAX_CAPTION_UTF16 {
                    return Err(Error::MessageTooLong {
                        channel: Channel::Telegram,
                        limit: MAX_CAPTION_UTF16,
                        actual: length,
                        unit: "UTF-16 units",
                    });
                }
                body.insert("caption".to_owned(), Value::String(text.to_owned()));
            }
            body.insert(field.to_owned(), Value::String(reference));
        } else {
            if text.is_empty() {
                return Err(Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "message has neither text nor an attachment".to_owned(),
                });
            }
            if message.edit_of.is_some() {
                return Err(Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "edits go to editMessageText with the original message id".to_owned(),
                });
            }
            let length = utf16_len(text);
            if length > MAX_TEXT_UTF16 {
                return Err(Error::MessageTooLong {
                    channel: Channel::Telegram,
                    limit: MAX_TEXT_UTF16,
                    actual: length,
                    unit: "UTF-16 units",
                });
            }
            body.insert("text".to_owned(), Value::String(text.to_owned()));
        }

        if let Some(thread) = &message.thread_id {
            body.insert(
                "message_thread_id".to_owned(),
                Value::from(telegram_id(thread, "thread_id")?),
            );
        }
        if let Some(reply) = &message.reply_to {
            let mut parameters = serde_json::Map::new();
            parameters.insert(
                "message_id".to_owned(),
                Value::from(telegram_id(reply, "reply_to")?),
            );
            body.insert("reply_parameters".to_owned(), Value::Object(parameters));
        }

        json_payload(Channel::Telegram, "POST", &Value::Object(body))
    }
}

/// Telegram ids are signed 64-bit integers; anything else would be rejected by
/// the API, so it is rejected here first.
fn telegram_id(text: &str, field: &str) -> Result<i64> {
    text.trim()
        .parse::<i64>()
        .map_err(|_| Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: format!("{field} {text:?} is not an integer id"),
        })
}

/// Maps one `Message` object.
fn normalize_message(message: &Value, provider_type: &str) -> NormalizedMessage {
    let external_id = id::identify(message, "message_id", &Channel::Telegram, "message");
    let chat = json::child(message, "chat");
    let conversation_id = chat
        .and_then(|chat| json::id(chat, "id"))
        .unwrap_or_default();
    // Channel posts have no `from`; the chat itself is the author.
    let sender_id = json::child(message, "from")
        .and_then(|from| json::id(from, "id"))
        .or_else(|| json::child(message, "sender_chat").and_then(|chat| json::id(chat, "id")))
        .unwrap_or_else(|| conversation_id.clone());

    let text = json::string(message, "text")
        .or_else(|| json::string(message, "caption"))
        .map(str::to_owned);
    let is_edit = provider_type.starts_with("edited_");

    NormalizedMessage {
        channel: Channel::Telegram,
        conversation_id,
        sender_id,
        edit_of: is_edit.then(|| external_id.clone()),
        external_id,
        text,
        timestamp: json::integer(message, "edit_date").or_else(|| json::integer(message, "date")),
        attachments: attachments(message),
        reply_to: json::child(message, "reply_to_message")
            .and_then(|reply| json::id(reply, "message_id")),
        thread_id: json::id(message, "message_thread_id"),
        reaction: None,
        status: None,
        provider_type: Some(provider_type.to_owned()),
        raw: Some(message.clone()),
    }
}

/// Collects every media field Telegram may attach to a message.
fn attachments(message: &Value) -> Vec<Attachment> {
    let mut out = Vec::new();

    // Telegram sends every rendition of a photo; keep the largest one.
    if let Some(photo) = json::child(message, "photo").and_then(Value::as_array) {
        let largest = photo.iter().max_by_key(|size| {
            json::unsigned(size, "width").unwrap_or(0) * json::unsigned(size, "height").unwrap_or(0)
        });
        if let Some(size) = largest {
            let mut attachment = Attachment::new(AttachmentKind::Image);
            attachment.id = json::id(size, "file_id");
            attachment.size_bytes = json::unsigned(size, "file_size");
            attachment.width = json::narrow(json::unsigned(size, "width"));
            attachment.height = json::narrow(json::unsigned(size, "height"));
            out.push(attachment);
        }
    }

    if let Some(document) = json::child(message, "document") {
        let mut attachment = Attachment::new(AttachmentKind::Document);
        attachment.id = json::id(document, "file_id");
        attachment.mime_type = json::string(document, "mime_type").map(str::to_owned);
        attachment.filename = json::string(document, "file_name").map(str::to_owned);
        attachment.size_bytes = json::unsigned(document, "file_size");
        out.push(attachment);
    }

    if let Some(audio) = json::child(message, "audio") {
        let mut attachment = Attachment::new(AttachmentKind::Audio);
        attachment.id = json::id(audio, "file_id");
        attachment.mime_type = json::string(audio, "mime_type").map(str::to_owned);
        attachment.filename = json::string(audio, "file_name").map(str::to_owned);
        attachment.size_bytes = json::unsigned(audio, "file_size");
        attachment.duration_seconds = json::narrow(json::unsigned(audio, "duration"));
        out.push(attachment);
    }

    if let Some(voice) = json::child(message, "voice") {
        let mut attachment = Attachment::new(AttachmentKind::Voice);
        attachment.id = json::id(voice, "file_id");
        attachment.mime_type = json::string(voice, "mime_type").map(str::to_owned);
        attachment.size_bytes = json::unsigned(voice, "file_size");
        attachment.duration_seconds = json::narrow(json::unsigned(voice, "duration"));
        out.push(attachment);
    }

    for (field, kind) in [
        ("video", AttachmentKind::Video),
        ("video_note", AttachmentKind::Video),
        ("animation", AttachmentKind::Video),
    ] {
        if let Some(video) = json::child(message, field) {
            let mut attachment = Attachment::new(kind);
            attachment.id = json::id(video, "file_id");
            attachment.mime_type = json::string(video, "mime_type").map(str::to_owned);
            attachment.filename = json::string(video, "file_name").map(str::to_owned);
            attachment.size_bytes = json::unsigned(video, "file_size");
            attachment.width = json::narrow(json::unsigned(video, "width"));
            attachment.height = json::narrow(json::unsigned(video, "height"));
            attachment.duration_seconds = json::narrow(json::unsigned(video, "duration"));
            out.push(attachment);
        }
    }

    if let Some(sticker) = json::child(message, "sticker") {
        let mut attachment = Attachment::new(AttachmentKind::Sticker);
        attachment.id = json::id(sticker, "file_id");
        attachment.size_bytes = json::unsigned(sticker, "file_size");
        attachment.width = json::narrow(json::unsigned(sticker, "width"));
        attachment.height = json::narrow(json::unsigned(sticker, "height"));
        attachment.caption = json::string(sticker, "emoji").map(str::to_owned);
        out.push(attachment);
    }

    if let Some(location) = json::child(message, "location") {
        let mut attachment = Attachment::new(AttachmentKind::Location);
        attachment.caption = json::string(location, "name").map(str::to_owned);
        out.push(attachment);
    }

    if let Some(contact) = json::child(message, "contact") {
        let mut attachment = Attachment::new(AttachmentKind::Contact);
        attachment.caption = json::string(contact, "first_name")
            .or_else(|| json::string(contact, "title"))
            .map(str::to_owned);
        out.push(attachment);
    }

    if json::child(message, "poll").is_some() {
        out.push(Attachment::new(AttachmentKind::Other("poll".to_owned())));
    }

    out
}

/// Maps a `message_reaction` update; the reaction is keyed by the message it
/// annotates.
fn normalize_reaction(reaction: &Value) -> NormalizedMessage {
    let chat = json::child(reaction, "chat");
    let conversation_id = chat
        .and_then(|chat| json::id(chat, "id"))
        .unwrap_or_default();
    let message_id = id::identify(reaction, "message_id", &Channel::Telegram, "reaction");
    let sender_id = json::child(reaction, "user")
        .and_then(|user| json::id(user, "id"))
        .unwrap_or_else(|| conversation_id.clone());

    let emoji = json::array(reaction, "new_reaction")
        .iter()
        .find_map(|entry| {
            json::string(entry, "emoji")
                .or_else(|| json::string(entry, "custom_emoji_id"))
                .map(str::to_owned)
        })
        .unwrap_or_default();

    NormalizedMessage {
        channel: Channel::Telegram,
        external_id: message_id.clone(),
        conversation_id,
        sender_id,
        text: None,
        timestamp: json::integer(reaction, "date"),
        attachments: Vec::new(),
        reply_to: None,
        thread_id: None,
        edit_of: None,
        reaction: Some(Reaction {
            message_id,
            removed: emoji.is_empty(),
            emoji,
        }),
        status: None,
        provider_type: Some("message_reaction".to_owned()),
        raw: Some(reaction.clone()),
    }
}
