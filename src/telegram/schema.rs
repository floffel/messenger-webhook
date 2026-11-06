//! Telegram Bot API object names and permissive field access.
//!
//! The adapter deliberately decodes Telegram updates with [`crate::json`]
//! accessors and carries the untouched provider object in
//! [`NormalizedMessage::raw`](crate::NormalizedMessage::raw). That keeps the
//! normalisation layer tolerant of fields and update kinds this crate does not
//! model yet: an unrecognised key never fails the batch, and a JSON scalar that
//! arrives as either a string or a number is read the same way.
//!
//! The maintained Telegram model crates were evaluated for this issue.
//! `frankenstein` is licensed under WTFPL, which is outside this repository's
//! `deny.toml` allow list. `teloxide`/`teloxide-core` are MIT, but their model
//! types arrive with an HTTP/runtime stack and stricter enum modelling than this
//! webhook-only crate wants for inbound decoding. The hand-rolled alternative
//! the issue permits is therefore kept, but only here: field names for every
//! Telegram object used during normalisation live in one place, are documented
//! against <https://core.telegram.org/bots/api>, and are pinned by unit tests in
//! this module so drift is reviewed as a model change rather than hidden inside
//! mapping code.

use crate::json;
use serde_json::Value;

pub(super) const MESSAGE_REACTION: &str = "message_reaction";

pub(super) const MESSAGE_ID: &str = "message_id";
pub(super) const MESSAGE_THREAD_ID: &str = "message_thread_id";
pub(super) const CHAT_ID: &str = "chat_id";
pub(super) const CHAT: &str = "chat";
pub(super) const FROM: &str = "from";
pub(super) const SENDER_CHAT: &str = "sender_chat";
pub(super) const USER: &str = "user";
pub(super) const ID: &str = "id";
pub(super) const DATE: &str = "date";
pub(super) const EDIT_DATE: &str = "edit_date";
pub(super) const TEXT: &str = "text";
pub(super) const CAPTION: &str = "caption";
pub(super) const REPLY_TO_MESSAGE: &str = "reply_to_message";
pub(super) const REPLY_PARAMETERS: &str = "reply_parameters";

pub(super) const PHOTO: &str = "photo";
pub(super) const DOCUMENT: &str = "document";
pub(super) const AUDIO: &str = "audio";
pub(super) const VOICE: &str = "voice";
pub(super) const VIDEO: &str = "video";
pub(super) const VIDEO_NOTE: &str = "video_note";
pub(super) const ANIMATION: &str = "animation";
pub(super) const STICKER: &str = "sticker";
pub(super) const LOCATION: &str = "location";
pub(super) const CONTACT: &str = "contact";
pub(super) const POLL: &str = "poll";

pub(super) const FILE_ID: &str = "file_id";
pub(super) const FILE_NAME: &str = "file_name";
pub(super) const FILE_SIZE: &str = "file_size";
pub(super) const MIME_TYPE: &str = "mime_type";
pub(super) const DURATION: &str = "duration";
pub(super) const WIDTH: &str = "width";
pub(super) const HEIGHT: &str = "height";
pub(super) const EMOJI: &str = "emoji";
pub(super) const LATITUDE: &str = "latitude";
pub(super) const LONGITUDE: &str = "longitude";
pub(super) const NAME: &str = "name";
pub(super) const PHONE_NUMBER: &str = "phone_number";
pub(super) const FIRST_NAME: &str = "first_name";
pub(super) const LAST_NAME: &str = "last_name";
pub(super) const TITLE: &str = "title";

pub(super) const OLD_REACTION: &str = "old_reaction";
pub(super) const NEW_REACTION: &str = "new_reaction";
pub(super) const CUSTOM_EMOJI_ID: &str = "custom_emoji_id";

pub(super) const ENTITIES: &str = "entities";
pub(super) const CAPTION_ENTITIES: &str = "caption_entities";
pub(super) const TYPE: &str = "type";
pub(super) const OFFSET: &str = "offset";
pub(super) const LENGTH: &str = "length";
pub(super) const URL: &str = "url";
pub(super) const LANGUAGE: &str = "language";

pub(super) const VIDEO_FIELDS: [&str; 3] = [VIDEO, VIDEO_NOTE, ANIMATION];

pub(super) const ENTITY_MENTION: &str = "mention";
pub(super) const ENTITY_HASHTAG: &str = "hashtag";
pub(super) const ENTITY_CASHTAG: &str = "cashtag";
pub(super) const ENTITY_BOT_COMMAND: &str = "bot_command";
pub(super) const ENTITY_URL: &str = "url";
pub(super) const ENTITY_EMAIL: &str = "email";
pub(super) const ENTITY_PHONE_NUMBER: &str = "phone_number";
pub(super) const ENTITY_BOLD: &str = "bold";
pub(super) const ENTITY_ITALIC: &str = "italic";
pub(super) const ENTITY_UNDERLINE: &str = "underline";
pub(super) const ENTITY_STRIKETHROUGH: &str = "strikethrough";
pub(super) const ENTITY_SPOILER: &str = "spoiler";
pub(super) const ENTITY_BLOCKQUOTE: &str = "blockquote";
pub(super) const ENTITY_EXPANDABLE_BLOCKQUOTE: &str = "expandable_blockquote";
pub(super) const ENTITY_CODE: &str = "code";
pub(super) const ENTITY_PRE: &str = "pre";
pub(super) const ENTITY_TEXT_LINK: &str = "text_link";
pub(super) const ENTITY_TEXT_MENTION: &str = "text_mention";
pub(super) const ENTITY_CUSTOM_EMOJI: &str = "custom_emoji";

/// One Telegram update field this adapter can normalise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateKind {
    /// `message`.
    Message,
    /// `edited_message`.
    EditedMessage,
    /// `channel_post`.
    ChannelPost,
    /// `edited_channel_post`.
    EditedChannelPost,
    /// `message_reaction`.
    MessageReaction,
}

impl UpdateKind {
    /// The top-level update field Telegram sends for this kind.
    pub(super) const fn field(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::EditedMessage => "edited_message",
            Self::ChannelPost => "channel_post",
            Self::EditedChannelPost => "edited_channel_post",
            Self::MessageReaction => MESSAGE_REACTION,
        }
    }

    /// The value stored in [`NormalizedMessage::provider_type`](crate::NormalizedMessage::provider_type).
    pub(super) const fn provider_type(self) -> &'static str {
        self.field()
    }

    /// Whether this update represents an edit of an earlier message.
    pub(super) const fn is_edit(self) -> bool {
        matches!(self, Self::EditedMessage | Self::EditedChannelPost)
    }
}

pub(super) const UPDATE_KINDS: [UpdateKind; 5] = [
    UpdateKind::Message,
    UpdateKind::EditedMessage,
    UpdateKind::ChannelPost,
    UpdateKind::EditedChannelPost,
    UpdateKind::MessageReaction,
];

/// A borrowed Telegram `Update`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Update<'a> {
    value: &'a Value,
}

impl<'a> Update<'a> {
    /// Wraps one `Update` object.
    pub(super) const fn new(value: &'a Value) -> Self {
        Self { value }
    }

    /// The object carried under `kind`, when Telegram sent an object.
    pub(super) fn object(self, kind: UpdateKind) -> Option<&'a Value> {
        child(self.value, kind.field()).filter(|value| value.is_object())
    }
}

/// A borrowed Telegram `Message`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Message<'a> {
    value: &'a Value,
}

impl<'a> Message<'a> {
    /// Wraps one `Message` object.
    pub(super) const fn new(value: &'a Value) -> Self {
        Self { value }
    }

    /// The untouched `Message` object.
    pub(super) const fn raw(self) -> &'a Value {
        self.value
    }

    /// The provider's `chat.id`.
    pub(super) fn chat_id(self) -> Option<String> {
        child(self.value, CHAT).and_then(|chat| id(chat, ID))
    }

    /// The provider's `from.id`.
    pub(super) fn user_id(self) -> Option<String> {
        child(self.value, FROM).and_then(|from| id(from, ID))
    }

    /// The provider's `sender_chat.id`.
    pub(super) fn sender_chat_id(self) -> Option<String> {
        child(self.value, SENDER_CHAT).and_then(|chat| id(chat, ID))
    }

    /// The provider's `text` or, for media, `caption`.
    pub(super) fn text_or_caption(self) -> Option<String> {
        string(self.value, TEXT)
            .or_else(|| string(self.value, CAPTION))
            .map(str::to_owned)
    }

    /// The provider's plain `text`, without falling back to `caption`.
    pub(super) fn text(self) -> Option<&'a str> {
        string(self.value, TEXT)
    }

    /// The provider's `caption`.
    pub(super) fn caption(self) -> Option<&'a str> {
        string(self.value, CAPTION)
    }

    /// The event timestamp, preferring `edit_date` over `date`.
    pub(super) fn timestamp(self) -> Option<i64> {
        integer(self.value, EDIT_DATE).or_else(|| integer(self.value, DATE))
    }

    /// The replied-to message id, when present.
    pub(super) fn reply_to(self) -> Option<String> {
        child(self.value, REPLY_TO_MESSAGE).and_then(|reply| id(reply, MESSAGE_ID))
    }

    /// The forum topic id, when present.
    pub(super) fn thread_id(self) -> Option<String> {
        id(self.value, MESSAGE_THREAD_ID)
    }

    /// A child object by Telegram field name.
    pub(super) fn object(self, field: &str) -> Option<&'a Value> {
        child(self.value, field).filter(|value| value.is_object())
    }

    /// An array by Telegram field name.
    pub(super) fn array(self, field: &str) -> &'a [Value] {
        array(self.value, field)
    }

    /// A string field by Telegram field name.
    pub(super) fn string(self, field: &str) -> Option<&'a str> {
        string(self.value, field)
    }

    /// An id field by Telegram field name.
    pub(super) fn id(self, field: &str) -> Option<String> {
        id(self.value, field)
    }

    /// An unsigned integer field by Telegram field name.
    pub(super) fn unsigned(self, field: &str) -> Option<u64> {
        unsigned(self.value, field)
    }

    /// A finite number field by Telegram field name.
    pub(super) fn number(self, field: &str) -> Option<serde_json::Number> {
        number(self.value, field)
    }
}

/// A borrowed Telegram `PhotoSize`.
#[derive(Debug, Clone, Copy)]
pub(super) struct PhotoSize<'a> {
    value: &'a Value,
}

impl<'a> PhotoSize<'a> {
    /// Wraps one `PhotoSize` object.
    pub(super) const fn new(value: &'a Value) -> Self {
        Self { value }
    }

    /// The provider's `file_id`.
    pub(super) fn file_id(self) -> Option<String> {
        id(self.value, FILE_ID)
    }

    /// The provider's `file_size`.
    pub(super) fn size_bytes(self) -> Option<u64> {
        unsigned(self.value, FILE_SIZE)
    }

    /// The provider's `width`, narrowed to the neutral model.
    pub(super) fn width(self) -> Option<u32> {
        json::narrow(unsigned(self.value, WIDTH))
    }

    /// The provider's `height`, narrowed to the neutral model.
    pub(super) fn height(self) -> Option<u32> {
        json::narrow(unsigned(self.value, HEIGHT))
    }

    /// The provider's raw `width` before the neutral model narrows it.
    pub(super) fn raw_width(self) -> Option<u64> {
        unsigned(self.value, WIDTH)
    }

    /// The provider's raw `height` before the neutral model narrows it.
    pub(super) fn raw_height(self) -> Option<u64> {
        unsigned(self.value, HEIGHT)
    }
}

/// A borrowed Telegram `MessageEntity`.
#[derive(Debug, Clone, Copy)]
pub(super) struct MessageEntity<'a> {
    value: &'a Value,
}

impl<'a> MessageEntity<'a> {
    /// Wraps one `MessageEntity` object.
    pub(super) const fn new(value: &'a Value) -> Self {
        Self { value }
    }

    /// The provider's `type`.
    pub(super) fn kind(self) -> Option<&'a str> {
        string(self.value, TYPE)
    }

    /// The provider's UTF-16 `offset`.
    pub(super) fn offset(self) -> Option<u64> {
        unsigned(self.value, OFFSET)
    }

    /// The provider's UTF-16 `length`.
    pub(super) fn length(self) -> Option<u64> {
        unsigned(self.value, LENGTH)
    }

    /// The provider's UTF-16 range.
    pub(super) fn range(self) -> Option<(u64, u64)> {
        Some((self.offset()?, self.length()?))
    }

    /// The provider's `url` payload for `text_link`.
    pub(super) fn url(self) -> Option<&'a str> {
        string(self.value, URL)
    }

    /// The provider's `language` payload for `pre`.
    pub(super) fn language(self) -> Option<&'a str> {
        string(self.value, LANGUAGE)
    }

    /// The provider's `user.id` payload for `text_mention`.
    pub(super) fn text_mention_user_id(self) -> Option<String> {
        child(self.value, USER).and_then(|user| id(user, ID))
    }

    /// The provider's `custom_emoji_id` payload.
    pub(super) fn custom_emoji_id(self) -> Option<&'a str> {
        string(self.value, CUSTOM_EMOJI_ID)
    }
}

/// A borrowed Telegram `MessageReactionUpdated`.
#[derive(Debug, Clone, Copy)]
pub(super) struct MessageReactionUpdated<'a> {
    value: &'a Value,
}

impl<'a> MessageReactionUpdated<'a> {
    /// Wraps one `MessageReactionUpdated` object.
    pub(super) const fn new(value: &'a Value) -> Self {
        Self { value }
    }

    /// The untouched `MessageReactionUpdated` object.
    pub(super) const fn raw(self) -> &'a Value {
        self.value
    }

    /// The provider's `chat.id`.
    pub(super) fn chat_id(self) -> Option<String> {
        child(self.value, CHAT).and_then(|chat| id(chat, ID))
    }

    /// The provider's `user.id`.
    pub(super) fn user_id(self) -> Option<String> {
        child(self.value, USER).and_then(|user| id(user, ID))
    }

    /// The provider's `date`.
    pub(super) fn timestamp(self) -> Option<i64> {
        integer(self.value, DATE)
    }

    /// The previous reaction state.
    pub(super) fn old_reactions(self) -> Vec<String> {
        self.reactions(OLD_REACTION)
    }

    /// The current reaction state.
    pub(super) fn new_reactions(self) -> Vec<String> {
        self.reactions(NEW_REACTION)
    }

    /// Whether Telegram sent an explicit empty `new_reaction` list.
    pub(super) fn new_reaction_is_empty(self) -> bool {
        matches!(child(self.value, NEW_REACTION), Some(Value::Array(entries)) if entries.is_empty())
    }

    fn reactions(self, field: &str) -> Vec<String> {
        array(self.value, field)
            .iter()
            .filter_map(|entry| Reaction::new(entry).key())
            .collect()
    }
}

/// A borrowed Telegram `ReactionType`.
#[derive(Debug, Clone, Copy)]
struct Reaction<'a> {
    value: &'a Value,
}

impl<'a> Reaction<'a> {
    const fn new(value: &'a Value) -> Self {
        Self { value }
    }

    fn key(self) -> Option<String> {
        string(self.value, EMOJI)
            .or_else(|| string(self.value, CUSTOM_EMOJI_ID))
            .map(str::to_owned)
    }
}

/// Borrows `field` from a Telegram object.
pub(super) fn child<'a>(object: &'a Value, field: &str) -> Option<&'a Value> {
    json::child(object, field)
}

/// Borrows a string field from a Telegram object.
pub(super) fn string<'a>(object: &'a Value, field: &str) -> Option<&'a str> {
    json::string(object, field)
}

/// Lends an array field from a Telegram object.
pub(super) fn array<'a>(object: &'a Value, field: &str) -> &'a [Value] {
    json::array(object, field)
}

/// Reads an id that may arrive as a string or as a number.
pub(super) fn id(object: &Value, field: &str) -> Option<String> {
    json::id(object, field)
}

/// Reads an integer that may arrive as a number or as a numeric string.
pub(super) fn integer(object: &Value, field: &str) -> Option<i64> {
    json::integer(object, field)
}

/// Reads an unsigned integer that may arrive as a number or as a numeric string.
pub(super) fn unsigned(object: &Value, field: &str) -> Option<u64> {
    json::unsigned(object, field)
}

/// Reads a finite JSON number that may arrive as a number or as a numeric string.
pub(super) fn number(object: &Value, field: &str) -> Option<serde_json::Number> {
    json::number(object, field)
}

#[cfg(test)]
mod tests {
    use super::{
        UpdateKind, ANIMATION, AUDIO, CAPTION, CAPTION_ENTITIES, CHAT, CHAT_ID, CONTACT,
        CUSTOM_EMOJI_ID, DATE, DOCUMENT, DURATION, EDIT_DATE, EMOJI, ENTITIES, ENTITY_BLOCKQUOTE,
        ENTITY_BOLD, ENTITY_BOT_COMMAND, ENTITY_CASHTAG, ENTITY_CODE, ENTITY_CUSTOM_EMOJI,
        ENTITY_EMAIL, ENTITY_EXPANDABLE_BLOCKQUOTE, ENTITY_HASHTAG, ENTITY_ITALIC, ENTITY_MENTION,
        ENTITY_PHONE_NUMBER, ENTITY_PRE, ENTITY_SPOILER, ENTITY_STRIKETHROUGH, ENTITY_TEXT_LINK,
        ENTITY_TEXT_MENTION, ENTITY_UNDERLINE, ENTITY_URL, FILE_ID, FILE_NAME, FILE_SIZE,
        FIRST_NAME, FROM, HEIGHT, ID, LANGUAGE, LAST_NAME, LATITUDE, LENGTH, LOCATION, LONGITUDE,
        MESSAGE_ID, MESSAGE_REACTION, MESSAGE_THREAD_ID, MIME_TYPE, NAME, NEW_REACTION, OFFSET,
        OLD_REACTION, PHONE_NUMBER, PHOTO, POLL, REPLY_PARAMETERS, REPLY_TO_MESSAGE, SENDER_CHAT,
        STICKER, TEXT, TITLE, TYPE, UPDATE_KINDS, URL, USER, VIDEO, VIDEO_FIELDS, VIDEO_NOTE,
        VOICE, WIDTH,
    };

    /// The exact object field names from
    /// <https://core.telegram.org/bots/api>. A rename here is a Telegram model
    /// change and must be reviewed, not absorbed silently into mapping code.
    #[test]
    fn telegram_field_names_are_pinned() {
        let fields = [
            (MESSAGE_REACTION, "message_reaction"),
            (MESSAGE_ID, "message_id"),
            (MESSAGE_THREAD_ID, "message_thread_id"),
            (CHAT_ID, "chat_id"),
            (CHAT, "chat"),
            (FROM, "from"),
            (SENDER_CHAT, "sender_chat"),
            (USER, "user"),
            (ID, "id"),
            (DATE, "date"),
            (EDIT_DATE, "edit_date"),
            (TEXT, "text"),
            (CAPTION, "caption"),
            (REPLY_TO_MESSAGE, "reply_to_message"),
            (REPLY_PARAMETERS, "reply_parameters"),
            (PHOTO, "photo"),
            (DOCUMENT, "document"),
            (AUDIO, "audio"),
            (VOICE, "voice"),
            (VIDEO, "video"),
            (VIDEO_NOTE, "video_note"),
            (ANIMATION, "animation"),
            (STICKER, "sticker"),
            (LOCATION, "location"),
            (CONTACT, "contact"),
            (POLL, "poll"),
            (FILE_ID, "file_id"),
            (FILE_NAME, "file_name"),
            (FILE_SIZE, "file_size"),
            (MIME_TYPE, "mime_type"),
            (DURATION, "duration"),
            (WIDTH, "width"),
            (HEIGHT, "height"),
            (EMOJI, "emoji"),
            (LATITUDE, "latitude"),
            (LONGITUDE, "longitude"),
            (NAME, "name"),
            (PHONE_NUMBER, "phone_number"),
            (FIRST_NAME, "first_name"),
            (LAST_NAME, "last_name"),
            (TITLE, "title"),
            (OLD_REACTION, "old_reaction"),
            (NEW_REACTION, "new_reaction"),
            (CUSTOM_EMOJI_ID, "custom_emoji_id"),
            (ENTITIES, "entities"),
            (CAPTION_ENTITIES, "caption_entities"),
            (TYPE, "type"),
            (OFFSET, "offset"),
            (LENGTH, "length"),
            (URL, "url"),
            (LANGUAGE, "language"),
            (ENTITY_MENTION, "mention"),
            (ENTITY_HASHTAG, "hashtag"),
            (ENTITY_CASHTAG, "cashtag"),
            (ENTITY_BOT_COMMAND, "bot_command"),
            (ENTITY_URL, "url"),
            (ENTITY_EMAIL, "email"),
            (ENTITY_PHONE_NUMBER, "phone_number"),
            (ENTITY_BOLD, "bold"),
            (ENTITY_ITALIC, "italic"),
            (ENTITY_UNDERLINE, "underline"),
            (ENTITY_STRIKETHROUGH, "strikethrough"),
            (ENTITY_SPOILER, "spoiler"),
            (ENTITY_BLOCKQUOTE, "blockquote"),
            (ENTITY_EXPANDABLE_BLOCKQUOTE, "expandable_blockquote"),
            (ENTITY_CODE, "code"),
            (ENTITY_PRE, "pre"),
            (ENTITY_TEXT_LINK, "text_link"),
            (ENTITY_TEXT_MENTION, "text_mention"),
            (ENTITY_CUSTOM_EMOJI, "custom_emoji"),
        ];
        for (actual, expected) in fields {
            assert_eq!(actual, expected, "Telegram field name drift");
        }
    }

    /// The update roots the adapter scans, each with its `provider_type`,
    /// and whether it is an edit.
    #[test]
    fn telegram_update_roots_are_pinned() {
        let expected = [
            (UpdateKind::Message, "message", false),
            (UpdateKind::EditedMessage, "edited_message", true),
            (UpdateKind::ChannelPost, "channel_post", false),
            (UpdateKind::EditedChannelPost, "edited_channel_post", true),
            (UpdateKind::MessageReaction, "message_reaction", false),
        ];
        for ((kind, field, is_edit), scanned) in expected.into_iter().zip(UPDATE_KINDS) {
            assert_eq!(kind, scanned, "update scan order drift");
            assert_eq!(kind.field(), field, "update field drift");
            assert_eq!(kind.provider_type(), field, "provider_type drift");
            assert_eq!(kind.is_edit(), is_edit, "edit classification drift");
        }
        assert_eq!(VIDEO_FIELDS, [VIDEO, VIDEO_NOTE, ANIMATION]);
    }
}
