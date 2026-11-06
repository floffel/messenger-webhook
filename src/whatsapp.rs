//! `WhatsApp` Cloud API adapter.
//!
//! Meta posts a batch of events to one endpoint, so messages and delivery
//! receipts arrive in the same `change` objects. Both are normalised here:
//! receipts come back as [`NormalizedMessage`] with
//! [`NormalizedMessage::status`] set and no text.
//!
//! Outbound, the same adapter turns a normalised message into the Cloud API
//! request body and refuses to produce one the provider would reject. A text,
//! media or interactive send and a [`Reaction`] all use the same
//! `POST /{phone-number-id}/messages` endpoint with a different `type`; the
//! returned [`ProviderPayload::operation`](crate::ProviderPayload::operation)
//! reports which:
//!
//! ```
//! use messenger_webhook::{Adapter, Channel, OutboundMessage, WhatsAppAdapter};
//!
//! let message = OutboundMessage::text(Channel::WhatsApp, "4915112345678", "Hallo");
//! let payload = WhatsAppAdapter::new("app-secret-app-secret")?.encode(&message)?;
//! assert_eq!(payload.json()?["text"]["body"], "Hallo");
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

use serde_json::Value;

use crate::error::{Error, Result};
use crate::headers::Headers;
use crate::id;
use crate::json;
use crate::model::{
    Attachment, AttachmentKind, Channel, DeliveryStatus, InteractiveReply, NormalizedMessage,
    OutboundMessage, ProviderMethod, ProviderOperation, ProviderPayload, Reaction,
};
use crate::secret::Secret;
use crate::signature::{self, META_SIGNATURE_HEADER};
use crate::{check_body, json_payload, recipient, Adapter};

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

/// Largest `WhatsApp` Cloud API webhook body Meta will deliver, in bytes.
///
/// Meta's Cloud API webhook documentation states, under *Payload size*:
/// "Webhook payloads can be up to 3 MB."
/// (<https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/overview>).
/// A body above it never arrives from Meta, so [`WhatsAppAdapter`] rejects it
/// before hashing or parsing instead of spending work on a caller that cannot
/// be the provider.
///
/// ```
/// assert_eq!(messenger_webhook::whatsapp::MAX_WEBHOOK_BODY_BYTES, 3 * 1024 * 1024);
/// ```
pub const MAX_WEBHOOK_BODY_BYTES: usize = 3 * 1024 * 1024;

/// Adapter for Meta's `WhatsApp` Cloud API webhooks.
///
/// The constructor takes the app secret; every inbound call has to be verified
/// before it is parsed.
///
/// ```
/// use messenger_webhook::{signature::meta_signature, Adapter, Headers, Secret, WhatsAppAdapter};
///
/// let secret = Secret::new("app-secret-app-secret")?;
/// let adapter = WhatsAppAdapter::new("app-secret-app-secret")?;
/// let body = br#"{"entry":[{"changes":[{"value":{"messages":[
///     {"id":"wamid.1","from":"4915112345678","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;
///
/// let mut headers = Headers::new();
/// headers.insert("x-hub-signature-256", meta_signature(&secret, body)?)?;
/// adapter.verify(&headers, body)?;
///
/// let messages = adapter.adapt(body)?;
/// assert_eq!(messages[0].text(), Some("Hallo"));
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct WhatsAppAdapter {
    secrets: Vec<Secret>,
}

impl WhatsAppAdapter {
    /// Creates an adapter that verifies with `app_secret` — the app secret from
    /// the Meta app dashboard.
    ///
    /// # Errors
    ///
    /// [`Error::MissingSecret`] when `app_secret` is empty, and
    /// [`Error::WeakSecret`] when it is shorter than [`crate::secret::MIN_SECRET_BYTES`].
    ///
    /// ```
    /// use messenger_webhook::{Adapter, WhatsAppAdapter};
    ///
    /// assert_eq!(
    ///     WhatsAppAdapter::new("app-secret-app-secret")?
    ///         .channel()
    ///         .as_str(),
    ///     "whatsapp"
    /// );
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn new(app_secret: impl Into<String>) -> Result<Self> {
        let app_secret = app_secret.into();
        if app_secret.is_empty() {
            return Err(Error::MissingSecret {
                channel: Channel::WhatsApp,
            });
        }
        Ok(Self {
            secrets: vec![Secret::new(app_secret)?],
        })
    }

    /// Creates an adapter that verifies against a rotation window.
    ///
    /// Use this when Meta app secrets are being rotated: pass the previous and
    /// current secret so deliveries signed with either are accepted until the
    /// old one has drained. Verification tries every secret with an
    /// independent constant-time comparison and returns
    /// [`Error::BadSignature`] only when none matched.
    ///
    /// # Errors
    ///
    /// [`Error::MissingSecret`] when no secret is supplied. Build each value
    /// through [`Secret::new`] first, which rejects weak secrets before they
    /// reach the adapter.
    ///
    /// ```
    /// use messenger_webhook::{signature::meta_signature, Adapter, Headers, Secret, WhatsAppAdapter};
    ///
    /// let previous = Secret::new("previous-secret-previous")?;
    /// let current = Secret::new("current-secret-current")?;
    /// let adapter = WhatsAppAdapter::with_secret_rotation([previous.clone(), current])?;
    /// let mut headers = Headers::new();
    /// headers.insert("x-hub-signature-256", meta_signature(&previous, b"{}")?)?;
    ///
    /// adapter.verify(&headers, b"{}")?;
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn with_secret_rotation<I>(app_secrets: I) -> Result<Self>
    where
        I: IntoIterator<Item = Secret>,
    {
        let secrets = app_secrets.into_iter().collect::<Vec<_>>();
        if secrets.is_empty() {
            return Err(Error::MissingSecret {
                channel: Channel::WhatsApp,
            });
        }
        Ok(Self { secrets })
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

    fn max_body_bytes(&self) -> usize {
        MAX_WEBHOOK_BODY_BYTES
    }

    fn verify(&self, headers: &Headers, body: &[u8]) -> Result<()> {
        // Size before hashing: never spend work on an oversized body.
        check_body(body.len(), MAX_WEBHOOK_BODY_BYTES)?;
        if self.secrets.is_empty() || self.secrets.iter().any(Secret::is_empty) {
            return Err(Error::MissingSecret {
                channel: Channel::WhatsApp,
            });
        }
        let header = headers.exactly_one(Channel::WhatsApp, META_SIGNATURE_HEADER)?;
        signature::verify_meta_any(&self.secrets, header, body)
    }

    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
        check_body(payload.len(), MAX_WEBHOOK_BODY_BYTES)?;
        let root: Value = serde_json::from_slice(payload)?;
        if !root.is_object() {
            return Err(Error::Payload("expected a JSON object".to_owned()));
        }

        let mut events = Vec::new();
        for (entry_index, entry) in json::array(&root, "entry").iter().enumerate() {
            for (change_index, change) in json::array(entry, "changes").iter().enumerate() {
                let Some(value) = json::child(change, "value") else {
                    continue;
                };
                let value_path = format!("/entry/{entry_index}/changes/{change_index}/value");
                for (message_index, message) in json::array(value, "messages").iter().enumerate() {
                    if !is_message(message) {
                        continue;
                    }
                    let path = format!("{value_path}/messages/{message_index}");
                    events.push(normalize_message(message, &path)?);
                }
                for (status_index, status) in json::array(value, "statuses").iter().enumerate() {
                    if !is_status(status) {
                        continue;
                    }
                    let path = format!("{value_path}/statuses/{status_index}");
                    events.push(normalize_status(status, &path)?);
                }
            }
        }
        Ok(events)
    }

    fn encode(&self, message: &OutboundMessage) -> Result<ProviderPayload> {
        let to = recipient(Channel::WhatsApp, message)?;

        let text = message.text.as_deref().unwrap_or_default();
        if let Some(reaction) = &message.reaction {
            return encode_reaction(to, message, reaction);
        }
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

        if let Some(reply) = &message.interactive_reply {
            let interactive = encode_reply_button(message, reply, text)?;
            body.insert("type".to_owned(), Value::String("interactive".to_owned()));
            body.insert("interactive".to_owned(), Value::Object(interactive));
        } else if let Some(attachment) = message.first_attachment() {
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
                        unit: crate::LimitUnit::Bytes,
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
            if message.edit_of.is_some() {
                return Err(Error::InvalidMessage {
                    channel: Channel::WhatsApp,
                    reason: "the Cloud API documents no edit endpoint; send a new message instead"
                        .to_owned(),
                });
            }
            let length = text.len();
            if length > MAX_TEXT_BYTES {
                return Err(Error::MessageTooLong {
                    channel: Channel::WhatsApp,
                    limit: MAX_TEXT_BYTES,
                    actual: length,
                    unit: crate::LimitUnit::Bytes,
                });
            }
            let mut text_body = serde_json::Map::new();
            text_body.insert("body".to_owned(), Value::String(text.to_owned()));
            body.insert("type".to_owned(), Value::String("text".to_owned()));
            body.insert("text".to_owned(), Value::Object(text_body));
        }

        json_payload(
            Channel::WhatsApp,
            ProviderMethod::Post,
            &Value::Object(body),
        )
        .map(|payload| payload.with_operation(ProviderOperation::Message, None))
    }
}

/// Builds the Cloud API `reaction` message Meta documents.
///
/// The body is the same `POST /{phone-number-id}/messages` envelope as a text
/// send, with `type: "reaction"`; the target carries the `wamid.` id Meta
/// assigned when it delivered the message, and an empty `emoji` removes the
/// reaction. See *Reaction messages* in the Cloud API messages reference
/// (<https://developers.facebook.com/docs/whatsapp/cloud-api/guides/send-messages#reaction-messages>).
fn encode_reaction(
    to: &str,
    message: &OutboundMessage,
    reaction: &Reaction,
) -> Result<ProviderPayload> {
    if message.first_attachment().is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::WhatsApp,
            reason: "a reaction cannot carry an attachment".to_owned(),
        });
    }
    if message.edit_of.is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::WhatsApp,
            reason: "a reaction cannot also be an edit".to_owned(),
        });
    }
    if message.interactive_reply.is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::WhatsApp,
            reason: "a reaction cannot carry an interactive reply".to_owned(),
        });
    }
    if message.text.as_deref().is_some_and(|text| !text.is_empty()) {
        return Err(Error::InvalidMessage {
            channel: Channel::WhatsApp,
            reason: "a reaction cannot carry message text".to_owned(),
        });
    }
    let target = reaction.message_id().trim();
    if target.is_empty() {
        return Err(Error::InvalidMessage {
            channel: Channel::WhatsApp,
            reason: "a reaction needs the Cloud API message id it reacts to".to_owned(),
        });
    }

    let mut reaction_body = serde_json::Map::new();
    reaction_body.insert(
        "message_id".to_owned(),
        Value::String(reaction.message_id().to_owned()),
    );
    reaction_body.insert(
        "emoji".to_owned(),
        Value::String(reaction.emoji().to_owned()),
    );

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
    body.insert("type".to_owned(), Value::String("reaction".to_owned()));
    body.insert("reaction".to_owned(), Value::Object(reaction_body));

    json_payload(
        Channel::WhatsApp,
        ProviderMethod::Post,
        &Value::Object(body),
    )
    .map(|payload| payload.with_operation(ProviderOperation::Reaction, None))
}

/// Builds the `interactive` object for a reply button, validating the request
/// before it reaches the wire.
fn encode_reply_button(
    message: &OutboundMessage,
    reply: &InteractiveReply,
    text: &str,
) -> Result<serde_json::Map<String, Value>> {
    if message.first_attachment().is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::WhatsApp,
            reason: "an interactive reply cannot carry an attachment".to_owned(),
        });
    }
    if message.edit_of.is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::WhatsApp,
            reason: "the Cloud API documents no edit endpoint; send a new message instead"
                .to_owned(),
        });
    }
    if text.is_empty() {
        return Err(Error::InvalidMessage {
            channel: Channel::WhatsApp,
            reason: "an interactive reply needs body text".to_owned(),
        });
    }
    let length = text.len();
    if length > MAX_TEXT_BYTES {
        return Err(Error::MessageTooLong {
            channel: Channel::WhatsApp,
            limit: MAX_TEXT_BYTES,
            actual: length,
            unit: crate::LimitUnit::Bytes,
        });
    }

    let mut interactive = serde_json::Map::new();
    interactive.insert("type".to_owned(), Value::String("button".to_owned()));

    let mut interactive_body = serde_json::Map::new();
    interactive_body.insert("text".to_owned(), Value::String(text.to_owned()));
    interactive.insert("body".to_owned(), Value::Object(interactive_body));

    let mut reply_payload = serde_json::Map::new();
    reply_payload.insert("id".to_owned(), Value::String(reply.id().to_owned()));
    reply_payload.insert("title".to_owned(), Value::String(reply.title().to_owned()));
    let mut button = serde_json::Map::new();
    button.insert("type".to_owned(), Value::String("reply".to_owned()));
    button.insert("reply".to_owned(), Value::Object(reply_payload));

    let mut action = serde_json::Map::new();
    action.insert(
        "buttons".to_owned(),
        Value::Array(vec![Value::Object(button)]),
    );
    interactive.insert("action".to_owned(), Value::Object(action));
    Ok(interactive)
}

/// Maps one `messages[]` object, or drops it when it has no id to key on.
fn normalize_message(message: &Value, envelope_path: &str) -> Result<NormalizedMessage> {
    let external_id = id::identify(message, "id", &Channel::WhatsApp, "message", envelope_path)?;
    let sender_id = json::id(message, "from").unwrap_or_default();
    let provider_type = json::string(message, "type")
        .unwrap_or("unknown")
        .to_owned();

    let mut normalized =
        NormalizedMessage::carrier(Channel::WhatsApp, external_id, sender_id.clone(), sender_id);
    normalized.timestamp = json::integer(message, "timestamp");
    normalized.reply_to =
        json::child(message, "context").and_then(|context| json::id(context, "id"));
    normalized.provider_type = Some(provider_type.clone());
    normalized.raw = Some(message.clone());

    match provider_type.as_str() {
        "text" => {
            normalized.set_text(
                json::child(message, "text")
                    .and_then(|text| json::string(text, "body"))
                    .map(str::to_owned),
            );
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
                attachment.latitude = json::number(location, "latitude");
                attachment.longitude = json::number(location, "longitude");
                normalized.attachments.push(attachment);
            }
        }
        "contacts" => {
            if let Some(contact) = json::array(message, "contacts").first() {
                let mut attachment = Attachment::new(AttachmentKind::Contact);
                let name = json::child(contact, "name");
                attachment.caption = name
                    .and_then(|name| json::string(name, "formatted_name"))
                    .or_else(|| name.and_then(|name| json::string(name, "first_name")))
                    .map(str::to_owned);
                attachment.first_name = name
                    .and_then(|name| json::string(name, "first_name"))
                    .map(str::to_owned);
                attachment.last_name = name
                    .and_then(|name| json::string(name, "last_name"))
                    .map(str::to_owned);
                attachment.phone_number = json::array(contact, "phones")
                    .first()
                    .and_then(|phone| {
                        json::string(phone, "phone").or_else(|| json::string(phone, "wa_id"))
                    })
                    .map(str::to_owned);
                normalized.attachments.push(attachment);
            }
        }
        "reaction" => {
            // The target message and the emoji live in the nested `reaction`
            // object, not on the message itself.
            if let Some(reaction) = json::child(message, "reaction") {
                if let Some(target) = json::id(reaction, "message_id") {
                    let emoji = json::string(reaction, "emoji").unwrap_or_default();
                    normalized.reaction = Some(if emoji.is_empty() {
                        Reaction::remove(target)
                    } else {
                        Reaction::add(target, emoji)?
                    });
                }
            }
        }
        "interactive" => normalize_interactive(&mut normalized, message, &provider_type),
        "button" => {
            // A template quick-reply payload: `payload` (the id a bot echoes
            // back) and `text` (the title shown to the reader).
            if let Some(reply) = json::child(message, "button").and_then(interactive_reply) {
                normalized.set_text(Some(reply.title().to_owned()));
                normalized.interactive_reply = Some(reply);
            }
        }
        // Orders, system notices and future types keep their provider name and
        // full payload in `raw`. If the provider included readable text, carry
        // it instead of hiding it in the raw payload.
        _ => normalized.set_text(unknown_text(message, &provider_type)),
    }

    Ok(normalized)
}

/// Fills `normalized` from an `interactive` message.
///
/// The chosen option lives under `interactive.button_reply` or
/// `interactive.list_reply`. A Flow response (`interactive.nfm_reply`) carries
/// no id + title a bot can answer, so it stays reachable through `raw`. The
/// title is also lifted into `text` so existing consumers keep a readable
/// fallback.
fn normalize_interactive(normalized: &mut NormalizedMessage, message: &Value, provider_type: &str) {
    if let Some(content) = json::child(message, "interactive") {
        let option =
            json::child(content, "button_reply").or_else(|| json::child(content, "list_reply"));
        if let Some(reply) = option.and_then(interactive_reply) {
            normalized.set_text(Some(reply.title().to_owned()));
            normalized.interactive_reply = Some(reply);
        }
    }
    if normalized.text().is_none() {
        normalized.set_text(unknown_text(message, provider_type));
    }
}

/// Builds a validated [`InteractiveReply`] from a `{ id, title }`-shaped
/// provider object, or drops it when the provider sent an unusable option.
///
/// Inbound decoding is permissive: a button the provider could not have
/// generated (empty id, oversized title) is not worth failing the whole batch
/// over, so it stays reachable through `raw` while the typed field is left
/// unset. Outbound, the same value is built through [`InteractiveReply::new`],
/// where an invalid option is a hard error.
fn interactive_reply(option: &Value) -> Option<InteractiveReply> {
    let id = json::string(option, "id").or_else(|| json::string(option, "payload"))?;
    let title = json::string(option, "title").or_else(|| json::string(option, "text"))?;
    InteractiveReply::new(id, title).ok()
}

/// Lifts text from an unrecognised message shape without inventing media.
fn unknown_text(message: &Value, provider_type: &str) -> Option<String> {
    json::child(message, "text")
        .and_then(|text| json::string(text, "body"))
        .or_else(|| {
            json::child(message, provider_type).and_then(|content| json::string(content, "body"))
        })
        .or_else(|| {
            json::child(message, provider_type).and_then(|content| json::string(content, "caption"))
        })
        .or_else(|| json::string(message, "body"))
        .map(str::to_owned)
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
        normalized.set_text(attachment.caption.clone());
        normalized.attachments.push(attachment);
    }
}

/// Maps one `statuses[]` entry (delivery receipt) onto a status event.
fn normalize_status(status: &Value, envelope_path: &str) -> Result<NormalizedMessage> {
    let external_id = id::identify(status, "id", &Channel::WhatsApp, "status", envelope_path)?;
    let recipient = json::id(status, "recipient_id").unwrap_or_default();
    let state = json::string(status, "status").unwrap_or("unknown");

    let mut normalized =
        NormalizedMessage::carrier(Channel::WhatsApp, external_id, recipient.clone(), recipient);
    normalized.timestamp = json::integer(status, "timestamp");
    normalized.status = Some(delivery_status(state));
    normalized.provider_type = Some("status".to_owned());
    normalized.raw = Some(status.clone());
    Ok(normalized)
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

/// Whether `entry` carries a field a `messages[]` object has.
///
/// A body from a different provider has none of them, and inventing an event
/// (and an id) out of foreign JSON would be worse than skipping it. Together
/// with its empty-object counterpart this is the only case in which an entry is
/// dropped.
fn is_message(entry: &Value) -> bool {
    ["id", "from", "type"]
        .iter()
        .any(|key| json::child(entry, key).is_some())
}

/// Whether `entry` carries a field a `statuses[]` object has.
fn is_status(entry: &Value) -> bool {
    ["id", "recipient_id", "status"]
        .iter()
        .any(|key| json::child(entry, key).is_some())
}
