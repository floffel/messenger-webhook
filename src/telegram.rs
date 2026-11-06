//! Telegram Bot API adapter.
//!
//! One update arrives per POST, but an update can be a new message, an edit, a
//! channel post or a reaction — all of them normalise here. Telegram has no
//! payload signature; it sends a secret token header that the bot registers
//! with `setWebhook`.
//!
//! Outbound, `encode` produces the `sendMessage` (or send-media) body and
//! enforces Telegram's 4096/1024 UTF-16 unit limits. An
//! [`OutboundMessage::edit_of`](crate::OutboundMessage::edit_of) becomes the
//! `editMessageText` body and an
//! [`OutboundMessage::reaction`](crate::OutboundMessage::reaction) becomes the
//! `setMessageReaction` body; the returned
//! [`ProviderPayload::operation`](crate::ProviderPayload::operation) tells the
//! caller which endpoint to post to.
//!
//! ```
//! use messenger_webhook::{Adapter, Channel, OutboundMessage, TelegramAdapter};
//!
//! let adapter = TelegramAdapter::new("bot-secret-bot-secret")?;
//! let message = OutboundMessage::text(Channel::Telegram, "-1001", "Hallo");
//! assert_eq!(adapter.encode(&message)?.json()?["text"], "Hallo");
//!
//! let too_long = OutboundMessage::text(Channel::Telegram, "-1001", &"a".repeat(4097));
//! assert!(adapter.encode(&too_long).is_err());
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

use serde_json::Value;

use crate::error::{Error, Result};
use crate::headers::Headers;
use crate::id;
use crate::json;
use crate::model::{
    Attachment, AttachmentKind, Channel, NormalizedMessage, OutboundMessage, Reaction,
};
use crate::model::{ProviderMethod, ProviderOperation, ProviderPayload};
use crate::rich::{byte_to_utf16_ranges, utf16_to_byte_ranges, TextFormat, TextSpan};
use crate::secret::Secret;
use crate::{check_body, json_payload, recipient, utf16_len, Adapter};

mod schema;

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

/// Most `MessageEntity` objects Telegram keeps on one message.
///
/// Telegram caps the array as a whole — not per entity type — and past the
/// ceiling it silently drops the extra runs, so a message arrives with the
/// tail unformatted and no error. The Bot API does not document a number;
/// `python-telegram-bot` pins it as `MAX_MESSAGE_ENTITIES = 100` ("Beyond this
/// cap telegram will simply ignore further formatting styles"), and a tdlib
/// `telegram-bot-api` report confirms a single message supports up to 100
/// entities
/// (<https://github.com/tdlib/telegram-bot-api/issues/820>). [`TelegramAdapter`]
/// rejects a larger array before the wire instead of losing formatting.
///
/// ```
/// assert_eq!(messenger_webhook::telegram::MAX_ENTITIES, 100);
/// ```
pub const MAX_ENTITIES: usize = 100;

/// Largest Telegram Bot API update body, in bytes.
///
/// The Bot API does not publish a webhook body ceiling. Its reference server
/// (tdlib `telegram-bot-api`) caps a serialised batch of updates at `1 << 22`
/// bytes and delivers one update per webhook `POST` (`Client.cpp`,
/// `Client::JsonUpdates`), so a delivered update body stays within that bound.
/// [`TelegramAdapter`] uses it as the ceiling and rejects a larger body before
/// hashing or parsing.
///
/// ```
/// assert_eq!(messenger_webhook::telegram::MAX_WEBHOOK_BODY_BYTES, 4 * 1024 * 1024);
/// ```
pub const MAX_WEBHOOK_BODY_BYTES: usize = 1 << 22;

/// Header Telegram sends the registered `secret_token` in.
pub const SECRET_TOKEN_HEADER: &str = "x-telegram-bot-api-secret-token";

/// Largest dimension a normalised attachment can carry.
///
/// Photo renditions are picked by area, so dimensions outside this bound are
/// ignored instead of letting a crafted update dominate the comparison.
const MAX_PHOTO_DIMENSION: u64 = u32::MAX as u64;

/// Adapter for Telegram Bot API updates.
///
/// ```
/// use messenger_webhook::{Adapter, Headers, TelegramAdapter};
///
/// let adapter = TelegramAdapter::new("bot-secret-bot-secret")?;
/// let body = br#"{"update_id":1,"message":{"message_id":7,"from":{"id":42},
///     "chat":{"id":-1001},"date":1729600000,"text":"Hallo"}}"#;
///
/// let headers = Headers::from_iter([
///     ("x-telegram-bot-api-secret-token", "bot-secret-bot-secret"),
/// ]);
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
    /// # Errors
    ///
    /// [`Error::MissingSecret`] when `secret_token` is empty, and
    /// [`Error::WeakSecret`] when it is shorter than [`crate::secret::MIN_SECRET_BYTES`].
    ///
    /// ```
    /// use messenger_webhook::{Adapter, TelegramAdapter};
    ///
    /// assert_eq!(
    ///     TelegramAdapter::new("bot-secret-bot-secret")?
    ///         .channel()
    ///         .as_str(),
    ///     "telegram"
    /// );
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn new(secret_token: impl Into<String>) -> Result<Self> {
        let secret_token = secret_token.into();
        if secret_token.is_empty() {
            return Err(Error::MissingSecret {
                channel: Channel::Telegram,
            });
        }
        Ok(Self {
            secret: Secret::new(secret_token)?,
        })
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

    fn max_body_bytes(&self) -> usize {
        MAX_WEBHOOK_BODY_BYTES
    }

    fn verify(&self, headers: &Headers, body: &[u8]) -> Result<()> {
        check_body(body.len(), MAX_WEBHOOK_BODY_BYTES)?;
        let header = headers.exactly_one(Channel::Telegram, SECRET_TOKEN_HEADER)?;
        self.secret
            .verify_channel(&Channel::Telegram, header.as_bytes())
    }

    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
        check_body(payload.len(), MAX_WEBHOOK_BODY_BYTES)?;
        let root: Value = serde_json::from_slice(payload)?;
        if !root.is_object() {
            return Err(Error::Payload("expected a JSON object".to_owned()));
        }

        let update = schema::Update::new(&root);
        let update_id = json::id(&root, schema::UPDATE_ID);
        let mut events = Vec::new();
        for kind in schema::UPDATE_KINDS {
            let Some(object) = update.object(kind) else {
                continue;
            };
            let discriminator = update_id.as_ref().map_or_else(
                || format!("/{}", kind.field()),
                |id| format!("/{}#update_id={id}", kind.field()),
            );
            match kind {
                schema::UpdateKind::MessageReaction => {
                    events.push(normalize_reaction(object, &discriminator)?);
                }
                _ => events.push(normalize_message(object, kind, &discriminator)?),
            }
        }
        Ok(events)
    }

    fn encode(&self, message: &OutboundMessage) -> Result<ProviderPayload> {
        let chat_id = recipient(Channel::Telegram, message)?;
        if let Some(reaction) = &message.reaction {
            return encode_reaction(chat_id, message, reaction);
        }
        if let Some(target) = &message.edit_of {
            return encode_edit(chat_id, message, target);
        }
        let mut body = serde_json::Map::new();
        body.insert(
            schema::CHAT_ID.to_owned(),
            Value::String(chat_id.to_owned()),
        );

        let text = message.text.as_deref().unwrap_or_default();
        if let Some(attachment) = message.first_attachment() {
            telegram_attachment_fields(&mut body, attachment, text)?;
            if !message.spans().is_empty() {
                if text.is_empty() {
                    return Err(Error::InvalidMessage {
                        channel: Channel::Telegram,
                        reason: "formatted runs need caption text".to_owned(),
                    });
                }
                body.insert(
                    schema::CAPTION_ENTITIES.to_owned(),
                    telegram_entities(text, message.spans())?,
                );
            }
        } else {
            if text.is_empty() {
                return Err(Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "message has neither text nor an attachment".to_owned(),
                });
            }
            let length = utf16_len(text);
            if length > MAX_TEXT_UTF16 {
                return Err(Error::MessageTooLong {
                    channel: Channel::Telegram,
                    limit: MAX_TEXT_UTF16,
                    actual: length,
                    unit: crate::LimitUnit::Utf16Units,
                });
            }
            body.insert(schema::TEXT.to_owned(), Value::String(text.to_owned()));
            if !message.spans().is_empty() {
                body.insert(
                    schema::ENTITIES.to_owned(),
                    telegram_entities(text, message.spans())?,
                );
            }
        }

        if let Some(thread) = &message.thread_id {
            body.insert(
                schema::MESSAGE_THREAD_ID.to_owned(),
                Value::from(telegram_id(thread, "thread_id")?),
            );
        }
        if let Some(reply) = &message.reply_to {
            let mut parameters = serde_json::Map::new();
            parameters.insert(
                schema::MESSAGE_ID.to_owned(),
                Value::from(telegram_id(reply, "reply_to")?),
            );
            body.insert(
                schema::REPLY_PARAMETERS.to_owned(),
                Value::Object(parameters),
            );
        }

        json_payload(
            Channel::Telegram,
            ProviderMethod::Post,
            &Value::Object(body),
        )
        .map(|payload| payload.with_operation(ProviderOperation::Message, None))
    }
}

/// Builds the `editMessageText` body for an outbound text edit.
///
/// Telegram addresses an edit by the original `message_id` plus the same
/// `chat_id` a send uses. Only text edits are encoded: `editMessageMedia` and
/// `editMessageCaption` need a provider `file_id` this crate does not model, so
/// an edit that carries an attachment is rejected before a request is built.
/// See *editMessageText* in the Bot API reference
/// (<https://core.telegram.org/bots/api#editmessagetext>).
fn encode_edit(chat_id: &str, message: &OutboundMessage, target: &str) -> Result<ProviderPayload> {
    if message.first_attachment().is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "an edit cannot carry an attachment; editMessageMedia and \
                     editMessageCaption need a provider file id"
                .to_owned(),
        });
    }
    if message.reaction.is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "an edit cannot also be a reaction".to_owned(),
        });
    }
    if message.interactive_reply.is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "Telegram has no interactive edit; send a new message".to_owned(),
        });
    }
    let text = message.text.as_deref().unwrap_or_default();
    if text.is_empty() {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "an edit needs the replacement text".to_owned(),
        });
    }
    let length = utf16_len(text);
    if length > MAX_TEXT_UTF16 {
        return Err(Error::MessageTooLong {
            channel: Channel::Telegram,
            limit: MAX_TEXT_UTF16,
            actual: length,
            unit: crate::LimitUnit::Utf16Units,
        });
    }

    let mut body = serde_json::Map::new();
    body.insert(
        schema::CHAT_ID.to_owned(),
        Value::String(chat_id.to_owned()),
    );
    body.insert(
        schema::MESSAGE_ID.to_owned(),
        Value::from(telegram_id(target, "edit_of")?),
    );
    body.insert(schema::TEXT.to_owned(), Value::String(text.to_owned()));
    if !message.spans().is_empty() {
        body.insert(
            schema::ENTITIES.to_owned(),
            telegram_entities(text, message.spans())?,
        );
    }

    json_payload(
        Channel::Telegram,
        ProviderMethod::Post,
        &Value::Object(body),
    )
    .map(|payload| payload.with_operation(ProviderOperation::Edit, None))
}

/// Builds the `setMessageReaction` body for an outbound reaction.
///
/// The reaction sits on the message Telegram delivered, addressed by
/// `message_id`. An added reaction is one emoji `ReactionType`; a removal
/// ([`Reaction::remove`]) is the documented empty list, which clears whatever
/// reaction the bot had left. See *setMessageReaction* in the Bot API reference
/// (<https://core.telegram.org/bots/api#setmessagereaction>).
fn encode_reaction(
    chat_id: &str,
    message: &OutboundMessage,
    reaction: &Reaction,
) -> Result<ProviderPayload> {
    if message.first_attachment().is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "a reaction cannot carry an attachment".to_owned(),
        });
    }
    if message.edit_of.is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "a reaction cannot also be an edit".to_owned(),
        });
    }
    if message.interactive_reply.is_some() {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "a reaction cannot carry an interactive reply".to_owned(),
        });
    }
    if message.text.as_deref().is_some_and(|text| !text.is_empty()) {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "a reaction cannot carry message text".to_owned(),
        });
    }
    let target = reaction.message_id().trim();
    if target.is_empty() {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "a reaction needs the message id it reacts to".to_owned(),
        });
    }

    let reaction_types = if reaction.removed() {
        Value::Array(Vec::new())
    } else {
        let mut emoji = serde_json::Map::new();
        emoji.insert(
            schema::TYPE.to_owned(),
            Value::String(schema::REACTION_EMOJI.to_owned()),
        );
        emoji.insert(
            schema::EMOJI.to_owned(),
            Value::String(reaction.emoji().to_owned()),
        );
        Value::Array(vec![Value::Object(emoji)])
    };

    let mut body = serde_json::Map::new();
    body.insert(
        schema::CHAT_ID.to_owned(),
        Value::String(chat_id.to_owned()),
    );
    body.insert(
        schema::MESSAGE_ID.to_owned(),
        Value::from(telegram_id(target, "reaction message_id")?),
    );
    body.insert(schema::REACTION.to_owned(), reaction_types);

    json_payload(
        Channel::Telegram,
        ProviderMethod::Post,
        &Value::Object(body),
    )
    .map(|payload| payload.with_operation(ProviderOperation::Reaction, None))
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

/// Maps a media attachment onto the top-level field Telegram accepts.
fn telegram_media_field(kind: &AttachmentKind) -> Result<&'static str> {
    match kind {
        AttachmentKind::Image => Ok(schema::PHOTO),
        AttachmentKind::Video => Ok(schema::VIDEO),
        AttachmentKind::Audio => Ok(schema::AUDIO),
        AttachmentKind::Voice => Ok(schema::VOICE),
        AttachmentKind::Document => Ok(schema::DOCUMENT),
        AttachmentKind::Sticker => Ok(schema::STICKER),
        other => Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: format!(
                "Telegram cannot send {} attachments; use a document",
                other.as_str()
            ),
        }),
    }
}

/// Adds the fields Telegram expects for one outbound attachment.
fn telegram_attachment_fields(
    body: &mut serde_json::Map<String, Value>,
    attachment: &Attachment,
    text: &str,
) -> Result<()> {
    match attachment.kind {
        AttachmentKind::Location => {
            if !text.is_empty() {
                return Err(Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "Telegram location attachments cannot carry a caption".to_owned(),
                });
            }
            let (latitude, longitude) = telegram_location_fields(attachment)?;
            body.insert(schema::LATITUDE.to_owned(), Value::Number(latitude));
            body.insert(schema::LONGITUDE.to_owned(), Value::Number(longitude));
        }
        AttachmentKind::Contact => {
            if !text.is_empty() {
                return Err(Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "Telegram contact attachments cannot carry a caption".to_owned(),
                });
            }
            telegram_contact_fields(body, attachment)?;
        }
        _ => {
            let field = telegram_media_field(&attachment.kind)?;
            if !text.is_empty() {
                let length = utf16_len(text);
                if length > MAX_CAPTION_UTF16 {
                    return Err(Error::MessageTooLong {
                        channel: Channel::Telegram,
                        limit: MAX_CAPTION_UTF16,
                        actual: length,
                        unit: crate::LimitUnit::Utf16Units,
                    });
                }
                body.insert(schema::CAPTION.to_owned(), Value::String(text.to_owned()));
            }
            let reference = attachment
                .id
                .clone()
                .or_else(|| attachment.url.clone())
                .ok_or_else(|| Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "attachment has neither a file id nor a url".to_owned(),
                })?;
            body.insert(field.to_owned(), Value::String(reference));
        }
    }
    Ok(())
}

/// Encodes byte spans as Telegram `MessageEntity` objects.
fn telegram_entities(text: &str, spans: &[TextSpan]) -> Result<Value> {
    if spans.len() > MAX_ENTITIES {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: format!(
                "Telegram keeps at most {MAX_ENTITIES} entities per message, got {}",
                spans.len()
            ),
        });
    }
    let ranges = byte_to_utf16_ranges(
        text,
        spans.iter().map(|span| Some((span.start(), span.end()))),
    );
    let mut entities = Vec::with_capacity(spans.len());
    for (span, range) in spans.iter().zip(ranges) {
        let Some((offset, length)) = range else {
            return Err(Error::InvalidMessage {
                channel: Channel::Telegram,
                reason: format!(
                    "span {}..{} does not slice message text",
                    span.start(),
                    span.end()
                ),
            });
        };
        entities.push(telegram_entity(span.format(), offset, length)?);
    }
    Ok(Value::Array(entities))
}

/// Maps one [`TextFormat`] onto the `MessageEntity` object Telegram accepts.
fn telegram_entity(format: &TextFormat, offset: u64, length: u64) -> Result<Value> {
    let mut entity = serde_json::Map::new();
    entity.insert(schema::OFFSET.to_owned(), Value::from(offset));
    entity.insert(schema::LENGTH.to_owned(), Value::from(length));
    match format {
        TextFormat::Bold => insert_type(&mut entity, schema::ENTITY_BOLD),
        TextFormat::Italic => insert_type(&mut entity, schema::ENTITY_ITALIC),
        TextFormat::Underline => insert_type(&mut entity, schema::ENTITY_UNDERLINE),
        TextFormat::Strikethrough => insert_type(&mut entity, schema::ENTITY_STRIKETHROUGH),
        TextFormat::Spoiler => insert_type(&mut entity, schema::ENTITY_SPOILER),
        TextFormat::Code => insert_type(&mut entity, schema::ENTITY_CODE),
        TextFormat::Pre { language } => {
            insert_type(&mut entity, schema::ENTITY_PRE);
            if let Some(language) = language.as_ref().filter(|language| !language.is_empty()) {
                entity.insert(schema::LANGUAGE.to_owned(), Value::String(language.clone()));
            }
        }
        TextFormat::Link { url } => {
            insert_type(&mut entity, schema::ENTITY_TEXT_LINK);
            entity.insert(schema::URL.to_owned(), Value::String(url.clone()));
        }
        TextFormat::Mention => insert_type(&mut entity, schema::ENTITY_MENTION),
        TextFormat::Hashtag => insert_type(&mut entity, schema::ENTITY_HASHTAG),
        TextFormat::Cashtag => insert_type(&mut entity, schema::ENTITY_CASHTAG),
        TextFormat::BotCommand => insert_type(&mut entity, schema::ENTITY_BOT_COMMAND),
        TextFormat::Url => insert_type(&mut entity, schema::ENTITY_URL),
        TextFormat::Email => insert_type(&mut entity, schema::ENTITY_EMAIL),
        TextFormat::PhoneNumber => insert_type(&mut entity, schema::ENTITY_PHONE_NUMBER),
        TextFormat::TextMention { user_id } => {
            return Err(Error::InvalidMessage {
                channel: Channel::Telegram,
                reason: format!(
                    "Telegram text_mention needs a full user object, got user_id {user_id:?}"
                ),
            });
        }
        TextFormat::CustomEmoji { id } => {
            insert_type(&mut entity, schema::ENTITY_CUSTOM_EMOJI);
            if id.is_empty() {
                return Err(Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "custom_emoji id is empty".to_owned(),
                });
            }
            entity.insert(
                schema::CUSTOM_EMOJI_ID.to_owned(),
                Value::String(id.clone()),
            );
        }
        TextFormat::Blockquote => insert_type(&mut entity, schema::ENTITY_BLOCKQUOTE),
        TextFormat::ExpandableBlockquote => {
            insert_type(&mut entity, schema::ENTITY_EXPANDABLE_BLOCKQUOTE);
        }
        TextFormat::Other(kind) => {
            if kind.is_empty() {
                return Err(Error::InvalidMessage {
                    channel: Channel::Telegram,
                    reason: "MessageEntity type is empty".to_owned(),
                });
            }
            insert_type(&mut entity, kind);
        }
    }
    Ok(Value::Object(entity))
}

/// Inserts the `type` field shared by every entity.
fn insert_type(entity: &mut serde_json::Map<String, Value>, kind: &str) {
    entity.insert(schema::TYPE.to_owned(), Value::String(kind.to_owned()));
}

/// Builds the `latitude` and `longitude` fields Telegram accepts for
/// `sendLocation`.
fn telegram_location_fields(
    attachment: &Attachment,
) -> Result<(serde_json::Number, serde_json::Number)> {
    let latitude = attachment
        .latitude
        .clone()
        .ok_or_else(|| Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "location attachment has no latitude".to_owned(),
        })?;
    let longitude = attachment
        .longitude
        .clone()
        .ok_or_else(|| Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "location attachment has no longitude".to_owned(),
        })?;
    let latitude_value = latitude.as_f64().ok_or_else(|| Error::InvalidMessage {
        channel: Channel::Telegram,
        reason: "location latitude is not a finite number".to_owned(),
    })?;
    if !(-90.0..=90.0).contains(&latitude_value) {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "location latitude must be between -90 and 90".to_owned(),
        });
    }
    let longitude_value = longitude.as_f64().ok_or_else(|| Error::InvalidMessage {
        channel: Channel::Telegram,
        reason: "location longitude is not a finite number".to_owned(),
    })?;
    if !(-180.0..=180.0).contains(&longitude_value) {
        return Err(Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "location longitude must be between -180 and 180".to_owned(),
        });
    }
    Ok((latitude, longitude))
}

/// Adds the fields Telegram accepts for `sendContact`.
fn telegram_contact_fields(
    body: &mut serde_json::Map<String, Value>,
    attachment: &Attachment,
) -> Result<()> {
    let phone_number = attachment
        .phone_number
        .as_ref()
        .ok_or_else(|| Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "contact attachment has no phone number".to_owned(),
        })?;
    let first_name = attachment
        .first_name
        .as_ref()
        .ok_or_else(|| Error::InvalidMessage {
            channel: Channel::Telegram,
            reason: "contact attachment has no first name".to_owned(),
        })?;
    let mut contact = serde_json::Map::new();
    contact.insert(
        "phone_number".to_owned(),
        Value::String(phone_number.clone()),
    );
    contact.insert("first_name".to_owned(), Value::String(first_name.clone()));
    if let Some(last_name) = &attachment.last_name {
        contact.insert("last_name".to_owned(), Value::String(last_name.clone()));
    }
    body.extend(contact);
    Ok(())
}

/// Maps one `Message` object.
fn normalize_message(
    message: &Value,
    kind: schema::UpdateKind,
    envelope_discriminator: &str,
) -> Result<NormalizedMessage> {
    let message = schema::Message::new(message);
    let external_id = id::identify(
        message.raw(),
        schema::MESSAGE_ID,
        &Channel::Telegram,
        "message",
        envelope_discriminator,
    )?;
    let conversation_id = message.chat_id().unwrap_or_default();
    // Channel posts have no `from`; the chat itself is the author.
    let sender_id = message
        .user_id()
        .or_else(|| message.sender_chat_id())
        .unwrap_or_else(|| conversation_id.clone());

    let mut normalized =
        NormalizedMessage::carrier(Channel::Telegram, external_id, conversation_id, sender_id);
    normalized.edit_of = kind.is_edit().then(|| normalized.external_id.clone());
    normalized.timestamp = message.timestamp();
    normalized.attachments = attachments(message);
    normalized.reply_to = message.reply_to();
    normalized.thread_id = message.thread_id();
    normalized.provider_type = Some(kind.provider_type().to_owned());
    normalized.raw = Some(message.raw().clone());
    normalized.set_text(message.text_or_caption());
    normalized.set_spans(spans(message));
    Ok(normalized)
}

/// Collects every media field Telegram may attach to a message.
fn attachments(message: schema::Message<'_>) -> Vec<Attachment> {
    let mut out = Vec::new();

    // Telegram sends every rendition of a photo; keep the largest one.
    let photo = message.array(schema::PHOTO);
    if !photo.is_empty() {
        let largest = photo
            .iter()
            .filter_map(|size| photo_area(size).map(|area| (size, area)))
            .max_by_key(|(_, area)| *area)
            .map(|(size, _)| size);
        if let Some(size) = largest {
            let size = schema::PhotoSize::new(size);
            let mut attachment = Attachment::new(AttachmentKind::Image);
            attachment.id = size.file_id();
            attachment.size_bytes = size.size_bytes();
            attachment.width = size.width();
            attachment.height = size.height();
            out.push(attachment);
        }
    }

    if let Some(document) = message.object(schema::DOCUMENT) {
        let document = schema::Message::new(document);
        let mut attachment = Attachment::new(AttachmentKind::Document);
        attachment.id = document.id(schema::FILE_ID);
        attachment.mime_type = document.string(schema::MIME_TYPE).map(str::to_owned);
        attachment.filename = document.string(schema::FILE_NAME).map(str::to_owned);
        attachment.size_bytes = document.unsigned(schema::FILE_SIZE);
        out.push(attachment);
    }

    if let Some(audio) = message.object(schema::AUDIO) {
        let audio = schema::Message::new(audio);
        let mut attachment = Attachment::new(AttachmentKind::Audio);
        attachment.id = audio.id(schema::FILE_ID);
        attachment.mime_type = audio.string(schema::MIME_TYPE).map(str::to_owned);
        attachment.filename = audio.string(schema::FILE_NAME).map(str::to_owned);
        attachment.size_bytes = audio.unsigned(schema::FILE_SIZE);
        attachment.duration_seconds = json::narrow(audio.unsigned(schema::DURATION));
        out.push(attachment);
    }

    if let Some(voice) = message.object(schema::VOICE) {
        let voice = schema::Message::new(voice);
        let mut attachment = Attachment::new(AttachmentKind::Voice);
        attachment.id = voice.id(schema::FILE_ID);
        attachment.mime_type = voice.string(schema::MIME_TYPE).map(str::to_owned);
        attachment.size_bytes = voice.unsigned(schema::FILE_SIZE);
        attachment.duration_seconds = json::narrow(voice.unsigned(schema::DURATION));
        out.push(attachment);
    }

    for field in schema::VIDEO_FIELDS {
        if let Some(video) = message.object(field) {
            let video = schema::Message::new(video);
            let mut attachment = Attachment::new(AttachmentKind::Video);
            attachment.id = video.id(schema::FILE_ID);
            attachment.mime_type = video.string(schema::MIME_TYPE).map(str::to_owned);
            attachment.filename = video.string(schema::FILE_NAME).map(str::to_owned);
            attachment.size_bytes = video.unsigned(schema::FILE_SIZE);
            attachment.width = json::narrow(video.unsigned(schema::WIDTH));
            attachment.height = json::narrow(video.unsigned(schema::HEIGHT));
            attachment.duration_seconds = json::narrow(video.unsigned(schema::DURATION));
            out.push(attachment);
        }
    }

    if let Some(sticker) = message.object(schema::STICKER) {
        let sticker = schema::Message::new(sticker);
        let mut attachment = Attachment::new(AttachmentKind::Sticker);
        attachment.id = sticker.id(schema::FILE_ID);
        attachment.size_bytes = sticker.unsigned(schema::FILE_SIZE);
        attachment.width = json::narrow(sticker.unsigned(schema::WIDTH));
        attachment.height = json::narrow(sticker.unsigned(schema::HEIGHT));
        attachment.caption = sticker.string(schema::EMOJI).map(str::to_owned);
        out.push(attachment);
    }

    if let Some(location) = message.object(schema::LOCATION) {
        let location = schema::Message::new(location);
        let mut attachment = Attachment::new(AttachmentKind::Location);
        attachment.caption = location.string(schema::NAME).map(str::to_owned);
        attachment.latitude = location.number(schema::LATITUDE);
        attachment.longitude = location.number(schema::LONGITUDE);
        out.push(attachment);
    }

    if let Some(contact) = message.object(schema::CONTACT) {
        let contact = schema::Message::new(contact);
        let mut attachment = Attachment::new(AttachmentKind::Contact);
        attachment.phone_number = contact.string(schema::PHONE_NUMBER).map(str::to_owned);
        attachment.first_name = contact.string(schema::FIRST_NAME).map(str::to_owned);
        attachment.last_name = contact.string(schema::LAST_NAME).map(str::to_owned);
        attachment.caption = contact
            .string(schema::FIRST_NAME)
            .or_else(|| contact.string(schema::TITLE))
            .map(str::to_owned);
        out.push(attachment);
    }

    if message.object(schema::POLL).is_some() {
        out.push(Attachment::new(AttachmentKind::Other("poll".to_owned())));
    }

    out
}

/// Area of a photo rendition, or `None` when its dimensions are unusable.
fn photo_area(size: &Value) -> Option<u128> {
    let size = schema::PhotoSize::new(size);
    let width = size.raw_width()?;
    let height = size.raw_height()?;
    if width > MAX_PHOTO_DIMENSION || height > MAX_PHOTO_DIMENSION {
        return None;
    }
    Some(u128::from(width) * u128::from(height))
}

/// Maps a `message_reaction` update; the reaction is keyed by the message it
/// annotates.
fn normalize_reaction(reaction: &Value, envelope_discriminator: &str) -> Result<NormalizedMessage> {
    let reaction = schema::MessageReactionUpdated::new(reaction);
    let conversation_id = reaction.chat_id().unwrap_or_default();
    let message_id = id::identify(
        reaction.raw(),
        schema::MESSAGE_ID,
        &Channel::Telegram,
        "reaction",
        envelope_discriminator,
    )?;
    let sender_id = reaction
        .user_id()
        .unwrap_or_else(|| conversation_id.clone());

    let old_reactions = reaction.old_reactions();
    let new_reactions = reaction.new_reactions();
    let emoji = new_reactions.first().cloned().unwrap_or_default();
    let removed = reaction.new_reaction_is_empty();
    let added_reactions = new_reactions
        .iter()
        .filter(|reaction| !old_reactions.contains(reaction))
        .cloned()
        .collect();
    let removed_reactions = old_reactions
        .iter()
        .filter(|reaction| !new_reactions.contains(reaction))
        .cloned()
        .collect();

    let mut normalized = NormalizedMessage::carrier(
        Channel::Telegram,
        message_id.clone(),
        conversation_id,
        sender_id,
    );
    normalized.timestamp = reaction.timestamp();
    normalized.reaction = Some(Reaction::from_parts(
        message_id,
        emoji,
        removed,
        added_reactions,
        removed_reactions,
    )?);
    normalized.provider_type = Some("message_reaction".to_owned());
    normalized.raw = Some(reaction.raw().clone());
    Ok(normalized)
}

/// Collects the formatting Telegram marks on a message.
///
/// `entities` describes `text`, `caption_entities` describes `caption`; the
/// offsets are UTF-16 code units, so they go through
/// [`utf16_to_byte_ranges`](crate::rich::utf16_to_byte_ranges) before a span is
/// built. An entity whose range cannot be expressed in bytes — a boundary
/// inside a surrogate pair, an offset past the end — is dropped: the plain text
/// is still complete, and inventing a run would mark text the provider did not
/// mark.
fn spans(message: schema::Message<'_>) -> Vec<TextSpan> {
    let (text, field) = match (message.text(), message.caption()) {
        (Some(text), _) => (text, schema::ENTITIES),
        (None, Some(caption)) => (caption, schema::CAPTION_ENTITIES),
        (None, None) => return Vec::new(),
    };
    let entities = message.array(field);
    let ranges = utf16_to_byte_ranges(
        text,
        entities
            .iter()
            .map(|entity| schema::MessageEntity::new(entity).range()),
    );
    entities
        .iter()
        .zip(ranges)
        .filter_map(|(entity, range)| one_span(text, schema::MessageEntity::new(entity), range?))
        .collect()
}

/// Maps one `MessageEntity` object onto a span of `text`.
fn one_span(
    text: &str,
    entity: schema::MessageEntity<'_>,
    (start, end): (usize, usize),
) -> Option<TextSpan> {
    let kind = entity.kind()?;
    TextSpan::new(text, start, end, format_of(kind, entity)).ok()
}

/// Maps a Telegram `MessageEntity.type` onto a [`TextFormat`].
///
/// A type whose payload is missing — a `text_link` without `url`, a
/// `custom_emoji` without its id — keeps its provider name as
/// [`TextFormat::Other`] instead of losing the run, so a renderer still sees
/// that the provider marked it.
fn format_of(kind: &str, entity: schema::MessageEntity<'_>) -> TextFormat {
    match kind {
        schema::ENTITY_BOLD => TextFormat::Bold,
        schema::ENTITY_ITALIC => TextFormat::Italic,
        schema::ENTITY_UNDERLINE => TextFormat::Underline,
        schema::ENTITY_STRIKETHROUGH => TextFormat::Strikethrough,
        schema::ENTITY_SPOILER => TextFormat::Spoiler,
        schema::ENTITY_CODE => TextFormat::Code,
        schema::ENTITY_PRE => TextFormat::Pre {
            language: entity
                .language()
                .filter(|language| !language.is_empty())
                .map(str::to_owned),
        },
        schema::ENTITY_TEXT_LINK => entity.url().filter(|url| !url.is_empty()).map_or_else(
            || TextFormat::Other(kind.to_owned()),
            |url| TextFormat::Link {
                url: url.to_owned(),
            },
        ),
        schema::ENTITY_TEXT_MENTION => entity.text_mention_user_id().map_or_else(
            || TextFormat::Other(kind.to_owned()),
            |user_id| TextFormat::TextMention { user_id },
        ),
        schema::ENTITY_CUSTOM_EMOJI => entity
            .custom_emoji_id()
            .filter(|id| !id.is_empty())
            .map_or_else(
                || TextFormat::Other(kind.to_owned()),
                |id| TextFormat::CustomEmoji { id: id.to_owned() },
            ),
        schema::ENTITY_MENTION => TextFormat::Mention,
        schema::ENTITY_HASHTAG => TextFormat::Hashtag,
        schema::ENTITY_CASHTAG => TextFormat::Cashtag,
        schema::ENTITY_BOT_COMMAND => TextFormat::BotCommand,
        schema::ENTITY_URL => TextFormat::Url,
        schema::ENTITY_EMAIL => TextFormat::Email,
        schema::ENTITY_PHONE_NUMBER => TextFormat::PhoneNumber,
        schema::ENTITY_BLOCKQUOTE => TextFormat::Blockquote,
        schema::ENTITY_EXPANDABLE_BLOCKQUOTE => TextFormat::ExpandableBlockquote,
        // `date_time` and anything newer stay reachable under the provider's
        // own name; `raw` still carries the untouched entity object.
        other => TextFormat::Other(other.to_owned()),
    }
}
