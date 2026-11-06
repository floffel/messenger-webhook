//! The provider-neutral message model.
//!
//! One shape for everything a messenger reports: text, media, replies,
//! threads, reactions, edits and delivery receipts. Fields a provider cannot
//! express stay `None`; fields this crate does not model stay reachable in
//! [`NormalizedMessage::raw`].

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use crate::rich::TextSpan;
use strum::{AsRefStr, Display, EnumString};

/// A validated custom channel name.
///
/// Values are produced by [`Channel::other`], which trims surrounding
/// whitespace, lower-cases ASCII letters, and rejects empty, oversized or
/// malformed names. The inner string is private, so callers cannot bypass those
/// checks by constructing the type directly.
///
/// ```
/// use messenger_webhook::{Channel, ChannelName};
///
/// let channel = Channel::other(" Signal ")?;
/// let name: &ChannelName = match &channel {
///     Channel::Other(name) => name,
///     _ => return Ok(()),
/// };
/// assert_eq!(name.as_str(), "signal");
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelName(String);

impl ChannelName {
    fn new(name: String) -> crate::Result<Self> {
        validate_channel_name(&name)?;
        Ok(Self(name))
    }

    /// Returns the normalised custom channel name.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ChannelName};
    ///
    /// let channel = Channel::other("signal")?;
    /// let name: &ChannelName = match &channel {
    ///     Channel::Other(name) => name,
    ///     _ => return Ok(()),
    /// };
    /// assert_eq!(name.as_str(), "signal");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChannelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which messenger a message came from.
///
/// The enum is `#[non_exhaustive]`, so a messenger this crate learns to speak
/// later arrives as a new variant in a minor release instead of a break.
///
/// Equality, hashing and ordering all compare [`Channel::as_str`], so a custom
/// channel name and a built-in channel cannot disagree about identity. New
/// custom channels must be built with [`Channel::other`].
///
/// ```
/// use messenger_webhook::Channel;
///
/// assert_eq!(Channel::WhatsApp.as_str(), "whatsapp");
/// assert_eq!(Channel::other(" Signal ")?.as_str(), "signal");
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Channel {
    /// `WhatsApp` Cloud API.
    WhatsApp,
    /// Telegram Bot API.
    Telegram,
    /// Matrix client-server or application-service events.
    Matrix,
    /// Anything else, carried as a validated [`ChannelName`].
    ///
    /// Use [`Channel::other`] to build this variant; [`ChannelName`] has no
    /// public constructor.
    Other(ChannelName),
}

impl Channel {
    /// Builds a custom channel from a caller-supplied name.
    ///
    /// The name is trimmed, ASCII lower-cased, and validated. Empty names,
    /// names longer than [`MAX_CHANNEL_NAME_BYTES`], names with a byte outside
    /// `[a-z0-9_-]` after normalisation, and names reserved by a built-in
    /// channel are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidChannelName`] when the trimmed,
    /// lower-cased name is empty, longer than [`MAX_CHANNEL_NAME_BYTES`],
    /// contains a byte outside `[a-z0-9_-]`, or matches a built-in channel name.
    ///
    /// ```
    /// use messenger_webhook::Channel;
    ///
    /// let signal = Channel::other(" Signal ")?;
    /// assert_eq!(signal.as_str(), "signal");
    /// assert!(Channel::other("whatsapp").is_err());
    /// assert!(Channel::other("").is_err());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn other(name: impl Into<String>) -> crate::Result<Self> {
        let raw = name.into();
        let trimmed = raw.trim();
        if trimmed.len() > MAX_CHANNEL_NAME_BYTES {
            return Err(crate::Error::InvalidChannelName {
                reason: format!(
                    "channel name is {} bytes, the ceiling is {MAX_CHANNEL_NAME_BYTES}",
                    trimmed.len()
                ),
            });
        }
        let name = trimmed.to_ascii_lowercase();
        Ok(Self::Other(ChannelName::new(name)?))
    }

    /// Stable lower-case name, used in error messages and registry lookups.
    ///
    /// ```
    /// use messenger_webhook::Channel;
    ///
    /// assert_eq!(Channel::Matrix.as_str(), "matrix");
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::WhatsApp => "whatsapp",
            Self::Telegram => "telegram",
            Self::Matrix => "matrix",
            Self::Other(name) => name.as_str(),
        }
    }
}

impl PartialEq for Channel {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Channel {}

impl Hash for Channel {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl PartialOrd for Channel {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Channel {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Channel {
    type Err = crate::Error;

    /// Parses a channel name back into a [`Channel`].
    ///
    /// Built-in names map to their variants; any other valid custom name is
    /// routed through [`Channel::other`], so parsing enforces the same grammar
    /// and reserved-name rules as construction.
    ///
    /// ```
    /// use messenger_webhook::Channel;
    /// use std::str::FromStr;
    ///
    /// assert_eq!(Channel::from_str("matrix")?, Channel::Matrix);
    /// assert_eq!(Channel::from_str("signal")?, Channel::other("signal")?);
    /// assert!(Channel::from_str("").is_err());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    fn from_str(s: &str) -> crate::Result<Self> {
        match s {
            "whatsapp" => Ok(Self::WhatsApp),
            "telegram" => Ok(Self::Telegram),
            "matrix" => Ok(Self::Matrix),
            other => Self::other(other),
        }
    }
}

/// Longest custom channel name this crate accepts, in bytes.
///
/// ```
/// use messenger_webhook::MAX_CHANNEL_NAME_BYTES;
///
/// assert_eq!(MAX_CHANNEL_NAME_BYTES, 255);
/// ```
pub const MAX_CHANNEL_NAME_BYTES: usize = 255;

fn validate_channel_name(name: &str) -> crate::Result<()> {
    if name.is_empty() {
        return Err(crate::Error::InvalidChannelName {
            reason: "channel name is empty".to_owned(),
        });
    }
    if name
        .bytes()
        .any(|byte| !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
    {
        return Err(crate::Error::InvalidChannelName {
            reason: "channel name must match [a-z0-9_-]".to_owned(),
        });
    }
    if matches!(name, "whatsapp" | "telegram" | "matrix") {
        return Err(crate::Error::InvalidChannelName {
            reason: format!("channel name {name:?} is reserved by a built-in channel"),
        });
    }
    Ok(())
}

/// What kind of media an attachment is, independent of provider naming.
///
/// ```
/// use messenger_webhook::AttachmentKind;
///
/// assert_eq!(AttachmentKind::Audio.as_str(), "audio");
/// assert_eq!(AttachmentKind::Other("poll".into()).as_str(), "poll");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, AsRefStr, Display, EnumString)]
#[non_exhaustive]
pub enum AttachmentKind {
    /// Still image.
    #[strum(serialize = "image")]
    Image,
    /// Video, including round video notes and animations.
    #[strum(serialize = "video")]
    Video,
    /// Audio file.
    #[strum(serialize = "audio")]
    Audio,
    /// Voice note / push-to-talk recording.
    #[strum(serialize = "voice")]
    Voice,
    /// Arbitrary file.
    #[strum(serialize = "document")]
    Document,
    /// Sticker.
    #[strum(serialize = "sticker")]
    Sticker,
    /// Geographic location.
    #[strum(serialize = "location")]
    Location,
    /// Contact card.
    #[strum(serialize = "contact")]
    Contact,
    /// Anything the provider sends that this crate does not model.
    #[strum(default, transparent)]
    Other(String),
}

impl AttachmentKind {
    /// Stable lower-case name.
    ///
    /// ```
    /// use messenger_webhook::AttachmentKind;
    ///
    /// assert_eq!(AttachmentKind::Voice.as_str(), "voice");
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.as_ref()
    }
}

/// One piece of media, with the metadata the provider chose to send.
///
/// Providers fill different subsets: `WhatsApp` reports an id and mime type but
/// no size, Telegram reports size and dimensions but no mime type for photos.
///
/// ```
/// use messenger_webhook::{Attachment, AttachmentKind};
///
/// let mut photo = Attachment::new(AttachmentKind::Image);
/// photo.mime_type = Some("image/jpeg".into());
/// photo.size_bytes = Some(48_213);
/// assert_eq!(photo.kind, AttachmentKind::Image);
/// assert_eq!(photo.filename, None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attachment {
    /// What kind of media this is.
    pub kind: AttachmentKind,
    /// Provider media id (`WhatsApp` id, Telegram file id, Matrix `mxc://` url).
    pub id: Option<String>,
    /// Download URL, when the provider sends one.
    pub url: Option<String>,
    /// MIME type, when the provider reports one.
    pub mime_type: Option<String>,
    /// Original file name, when the provider reports one.
    pub filename: Option<String>,
    /// Size in bytes, when the provider reports one.
    pub size_bytes: Option<u64>,
    /// Caption that belongs to this media item.
    pub caption: Option<String>,
    /// Pixel width, when known.
    pub width: Option<u32>,
    /// Pixel height, when known.
    pub height: Option<u32>,
    /// Duration in seconds, for audio and video.
    pub duration_seconds: Option<u32>,
}

impl Attachment {
    /// An attachment of `kind` with every metadata field empty.
    ///
    /// ```
    /// use messenger_webhook::{Attachment, AttachmentKind};
    ///
    /// let file = Attachment::new(AttachmentKind::Document);
    /// assert!(file.id.is_none() && file.size_bytes.is_none());
    /// ```
    #[must_use]
    pub fn new(kind: AttachmentKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }
}

impl Default for AttachmentKind {
    fn default() -> Self {
        Self::Other("unknown".to_owned())
    }
}

/// A reaction to another message.
///
/// ```
/// use messenger_webhook::Reaction;
///
/// let reaction = Reaction {
///     message_id: "wamid.1".into(),
///     emoji: "👍".into(),
///     removed: false,
///     added_reactions: vec!["👍".into()],
///     removed_reactions: Vec::new(),
/// };
/// assert!(!reaction.removed);
/// assert_eq!(reaction.added_reactions, ["👍"]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    /// External id of the message being reacted to.
    pub message_id: String,
    /// The first current reaction key (emoji or custom emoji id), empty when
    /// none remains or none could be parsed.
    pub emoji: String,
    /// Whether this event leaves no reaction on the message.
    pub removed: bool,
    /// Reaction keys (emoji or custom emoji id) present in the update but not
    /// in the prior state.
    pub added_reactions: Vec<String>,
    /// Reaction keys (emoji or custom emoji id) present in the prior state but
    /// not in the update.
    pub removed_reactions: Vec<String>,
}

/// Delivery state of an outbound message, as reported back by the provider.
///
/// ```
/// use messenger_webhook::DeliveryStatus;
///
/// assert_eq!(DeliveryStatus::Read.as_str(), "read");
/// assert_eq!(DeliveryStatus::Unknown("queued".into()).as_str(), "queued");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, AsRefStr, Display, EnumString)]
#[non_exhaustive]
pub enum DeliveryStatus {
    /// Accepted by the provider.
    #[strum(serialize = "sent")]
    Sent,
    /// Delivered to the recipient's device.
    #[strum(serialize = "delivered")]
    Delivered,
    /// Read by the recipient.
    #[strum(serialize = "read")]
    Read,
    /// Delivery failed.
    #[strum(serialize = "failed")]
    Failed,
    /// Deleted by the sender.
    #[strum(serialize = "deleted")]
    Deleted,
    /// A state this crate does not model, carried verbatim.
    #[strum(default, transparent)]
    Unknown(String),
}

impl DeliveryStatus {
    /// Stable lower-case name.
    ///
    /// ```
    /// use messenger_webhook::DeliveryStatus;
    ///
    /// assert_eq!(DeliveryStatus::Delivered.as_str(), "delivered");
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.as_ref()
    }
}

/// Prefix of an [`NormalizedMessage::external_id`] this crate synthesised.
///
/// A provider-assigned id never carries it, so `external_id.starts_with(…)` —
/// or [`NormalizedMessage::has_synthetic_id`] — tells the two apart.
///
/// ```
/// use messenger_webhook::SYNTHETIC_ID_PREFIX;
///
/// assert_eq!(SYNTHETIC_ID_PREFIX, "synthetic:");
/// ```
pub const SYNTHETIC_ID_PREFIX: &str = "synthetic:";

/// A caller-visible retry contract for one outbound payload.
///
/// This crate performs no I/O, so the application owns HTTP retries. This enum
/// is how an adapter tells the caller whether a retry can duplicate an already
/// accepted send. A caller should retry only when [`ProviderPayload::retry`]
/// says it is safe, or when the caller supplies the same
/// [`ProviderPayload::idempotency_key`] again.
///
/// ```
/// use messenger_webhook::RetryPolicy;
///
/// assert_eq!(RetryPolicy::Safe.as_str(), "safe");
/// assert!(RetryPolicy::UnsafeUnlessKeyPresent.is_unsafe_without_key());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RetryPolicy {
    /// Repeating the exact request is safe: it is a read, state-setting write or
    /// provider-idempotent write.
    Safe,
    /// Repeating the request can create a duplicate unless it carries the same
    /// [`ProviderPayload::idempotency_key`] as the original attempt.
    UnsafeUnlessKeyPresent,
}

impl RetryPolicy {
    /// Stable lower-case name.
    ///
    /// ```
    /// use messenger_webhook::RetryPolicy;
    ///
    /// assert_eq!(RetryPolicy::UnsafeUnlessKeyPresent.as_str(), "unsafe-unless-key-present");
    /// ```
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::UnsafeUnlessKeyPresent => "unsafe-unless-key-present",
        }
    }

    /// Whether a retry is unsafe until the caller proves the same idempotency
    /// key will be reused.
    ///
    /// ```
    /// use messenger_webhook::RetryPolicy;
    ///
    /// assert!(!RetryPolicy::Safe.is_unsafe_without_key());
    /// assert!(RetryPolicy::UnsafeUnlessKeyPresent.is_unsafe_without_key());
    /// ```
    #[must_use]
    pub const fn is_unsafe_without_key(&self) -> bool {
        matches!(self, Self::UnsafeUnlessKeyPresent)
    }
}

/// The idempotency key that makes a retry repeat instead of duplicate.
///
/// This is separate from [`NormalizedMessage::external_id`]: inbound events use
/// provider ids for deduplication, while outbound retries use the key a
/// provider documents for the send endpoint. Matrix transaction ids are the
/// canonical example.
///
/// ```
/// use messenger_webhook::IdempotencyKey;
///
/// let key = IdempotencyKey::new("order-42")?;
/// assert_eq!(key.as_str(), "order-42");
///
/// assert!(IdempotencyKey::new("").is_err());
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Wraps an already validated key without re-running validation.
    pub(crate) fn from_validated(raw: String) -> Self {
        Self(raw)
    }

    /// Wraps a non-empty, bounded key.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidIdempotencyKey`] when `raw` is empty or
    /// longer than [`MAX_IDEMPOTENCY_KEY_BYTES`].
    ///
    /// ```
    /// use messenger_webhook::IdempotencyKey;
    ///
    /// assert!(IdempotencyKey::new("txn-1").is_ok());
    /// assert!(IdempotencyKey::new("").is_err());
    /// ```
    pub fn new(raw: impl Into<String>) -> crate::Result<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(crate::Error::InvalidIdempotencyKey {
                reason: "idempotency key is empty".to_owned(),
            });
        }
        if raw.len() > MAX_IDEMPOTENCY_KEY_BYTES {
            return Err(crate::Error::InvalidIdempotencyKey {
                reason: format!(
                    "idempotency key is {} bytes, the ceiling is {MAX_IDEMPOTENCY_KEY_BYTES}",
                    raw.len()
                ),
            });
        }
        Ok(Self::from_validated(raw))
    }

    /// The key as the provider expects it in the request.
    ///
    /// ```
    /// use messenger_webhook::IdempotencyKey;
    ///
    /// assert_eq!(IdempotencyKey::new("txn-1")?.as_str(), "txn-1");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Longest idempotency key this crate will carry in a payload.
///
/// Provider limits differ; this is a local guard matching the Matrix
/// transaction-id ceiling so a caller cannot paste an unbounded string into a
/// request path.
///
/// ```
/// assert_eq!(messenger_webhook::MAX_IDEMPOTENCY_KEY_BYTES, 255);
/// ```
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 255;

/// One inbound event, provider independent.
///
/// A message, a reaction, an edit and a delivery receipt are all one of these:
/// they differ in which optional fields are set. `raw` keeps the provider's
/// original object, so nothing is lost even for event types this crate does not
/// model.
///
/// ```
/// use messenger_webhook::{Channel, NormalizedMessage};
///
/// let message = NormalizedMessage::inbound_text(Channel::Telegram, "update-1", "-1001", "Hallo");
/// assert_eq!(message.external_id, "update-1");
/// assert_eq!(message.conversation_id, "-1001");
/// assert_eq!(message.text(), Some("Hallo"));
/// assert!(message.attachments.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct NormalizedMessage {
    /// Origin channel.
    pub channel: Channel,
    /// Provider event id — use it as the idempotency key.
    ///
    /// When the provider sent none, this crate synthesises a stable one (see
    /// [`SYNTHETIC_ID_PREFIX`]) instead of dropping the event, so a replay
    /// dedupes and nothing disappears. [`Self::has_synthetic_id`] tells the
    /// two apart. A provider-assigned id that starts with
    /// [`SYNTHETIC_ID_PREFIX`] is retagged before it reaches this field, so
    /// the prefix stays a reliable provenance signal.
    pub external_id: String,
    /// Conversation the event belongs to (chat id, room id, phone number).
    pub conversation_id: String,
    /// Who sent it, in the provider's own id space.
    pub sender_id: String,
    /// Plain text, when the event carries any (captions included).
    text: Option<String>,
    /// Unix timestamp in seconds, when the provider sent one.
    pub timestamp: Option<i64>,
    /// Media carried by the event, in provider order.
    pub attachments: Vec<Attachment>,
    /// External id of the message this one replies to.
    pub reply_to: Option<String>,
    /// Thread or topic id, when the provider threads messages.
    pub thread_id: Option<String>,
    /// When this event is an edit: external id of the message it replaces.
    pub edit_of: Option<String>,
    /// Reaction carried by this event.
    pub reaction: Option<Reaction>,
    /// Delivery state carried by this event.
    pub status: Option<DeliveryStatus>,
    /// Formatted runs inside [`Self::text`], in provider order.
    ///
    /// Offsets are UTF-8 byte offsets into `text`, so `text[start..end]` is the
    /// run the span describes. A provider entity whose offsets cannot be
    /// expressed that way is dropped rather than repaired — see
    /// [`TextSpan::new`](crate::TextSpan::new).
    spans: Vec<TextSpan>,
    /// Provider-native event name (`text`, `image`, `message_reaction`,
    /// `m.room.message`, …), verbatim including unknown types.
    pub provider_type: Option<String>,
    /// The provider object this event was normalised from.
    pub raw: Option<serde_json::Value>,
}

impl NormalizedMessage {
    /// Builds an event with empty text and no formatting spans.
    pub(crate) fn carrier(
        channel: Channel,
        external_id: impl Into<String>,
        conversation_id: impl Into<String>,
        sender_id: impl Into<String>,
    ) -> Self {
        Self {
            channel,
            external_id: external_id.into(),
            conversation_id: conversation_id.into(),
            sender_id: sender_id.into(),
            text: None,
            timestamp: None,
            attachments: Vec::new(),
            reply_to: None,
            thread_id: None,
            edit_of: None,
            reaction: None,
            status: None,
            spans: Vec::new(),
            provider_type: None,
            raw: None,
        }
    }

    /// Builds a plain inbound text event with the provider's event id.
    ///
    /// This constructor is for tests, examples and event forwarding. Real
    /// adapters fill the same fields from the provider's own payload.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// let message = NormalizedMessage::inbound_text(
    ///     Channel::WhatsApp,
    ///     "wamid.1",
    ///     "4915112345678",
    ///     "Hallo",
    /// );
    /// assert_eq!(message.channel, Channel::WhatsApp);
    /// assert_eq!(message.external_id, "wamid.1");
    /// ```
    #[must_use]
    pub fn inbound_text(
        channel: Channel,
        external_id: impl Into<String>,
        conversation_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let mut message = Self::carrier(channel, external_id, conversation_id, String::new());
        message.set_text(Some(text));
        message
    }

    /// Plain text, when the event carries any (captions included).
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// let message = NormalizedMessage::inbound_text(Channel::Telegram, "update-1", "-1001", "Hallo");
    /// assert_eq!(message.text(), Some("Hallo"));
    /// ```
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Formatted runs inside [`Self::text`], in provider order.
    ///
    /// Every span returned here slices [`Self::text`] cleanly. Providers whose
    /// entity offsets cannot be expressed as UTF-8 byte offsets are dropped
    /// before they reach this list.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage, TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// let message = NormalizedMessage::inbound_text(Channel::Telegram, "update-1", "-1001", "Hallo Welt")
    ///     .with_spans([span]);
    /// assert_eq!(message.spans().len(), 1);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn spans(&self) -> &[TextSpan] {
        &self.spans
    }

    /// The text covered by the span at `index`, or `None` when no such span exists.
    ///
    /// This is the checked way to read formatted text: it delegates to
    /// [`TextSpan::slice`](crate::TextSpan::slice) and never indexes the text
    /// directly.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage, TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// let message = NormalizedMessage::inbound_text(Channel::Telegram, "update-1", "-1001", "Hallo Welt")
    ///     .with_spans([span]);
    /// assert_eq!(message.span_text(0), Some("Welt"));
    /// assert_eq!(message.span_text(1), None);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn span_text(&self, index: usize) -> Option<&str> {
        let text = self.text.as_deref()?;
        self.spans.get(index)?.slice(text)
    }

    /// Returns this event with `text` replaced and incompatible spans dropped.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// let message = NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "hi")
    ///     .with_text("servus");
    /// assert_eq!(message.text(), Some("servus"));
    /// ```
    #[must_use]
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.set_text(Some(text));
        self
    }

    /// Returns this event with `text` cleared and all formatting spans removed.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// let message = NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "hi")
    ///     .without_text();
    /// assert_eq!(message.text(), None);
    /// assert!(message.spans().is_empty());
    /// ```
    #[must_use]
    pub fn without_text(mut self) -> Self {
        self.set_text(None::<String>);
        self
    }

    /// Returns this event with formatting spans set from `spans`.
    ///
    /// Spans that do not slice [`Self::text`] cleanly are dropped.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage, TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// let message = NormalizedMessage::inbound_text(Channel::Telegram, "update-1", "-1001", "Hallo Welt")
    ///     .with_spans([span]);
    /// assert_eq!(message.span_text(0), Some("Welt"));
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn with_spans(mut self, spans: impl IntoIterator<Item = TextSpan>) -> Self {
        self.set_spans(spans);
        self
    }

    /// Returns this event with `text` and `spans` set together.
    ///
    /// Spans that do not slice the final `text` cleanly are dropped.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage, TextFormat, TextSpan};
    ///
    /// let valid = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// let message = NormalizedMessage::inbound_text(Channel::Telegram, "update-1", "-1001", "")
    ///     .with_text_and_spans("Hallo Welt", [valid]);
    /// assert_eq!(message.text(), Some("Hallo Welt"));
    /// assert_eq!(message.span_text(0), Some("Welt"));
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn with_text_and_spans(
        mut self,
        text: impl Into<String>,
        spans: impl IntoIterator<Item = TextSpan>,
    ) -> Self {
        self.text = Some(text.into());
        self.set_spans(spans);
        self
    }

    pub(crate) fn set_text(&mut self, text: Option<impl Into<String>>) {
        self.text = text.map(Into::into);
        self.retain_valid_spans();
    }

    pub(crate) fn set_spans(&mut self, spans: impl IntoIterator<Item = TextSpan>) {
        let Some(text) = self.text.as_deref() else {
            self.spans.clear();
            return;
        };
        self.spans = spans
            .into_iter()
            .filter(|span| span.slice(text).is_some())
            .collect();
    }

    fn retain_valid_spans(&mut self) {
        let Some(text) = self.text.as_deref() else {
            self.spans.clear();
            return;
        };
        self.spans.retain(|span| span.slice(text).is_some());
    }

    /// Whether [`Self::external_id`] was invented by this crate.
    ///
    /// Providers occasionally send an event without an id. Instead of dropping
    /// it, [`Adapter::adapt`](crate::Adapter::adapt) surfaces it with a stable
    /// synthetic id — the digest of the raw event, so a replay still dedupes.
    /// Applications that need the provider's own key (to call back into the
    /// provider, for example) check this first.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// let mut event = NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "hi");
    /// assert!(!event.has_synthetic_id());
    /// event.external_id = "synthetic:00ff".into();
    /// assert!(event.has_synthetic_id());
    /// ```
    #[must_use]
    pub fn has_synthetic_id(&self) -> bool {
        self.external_id.starts_with(SYNTHETIC_ID_PREFIX)
    }

    /// The first attachment, if the event carries media.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// assert!(NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "hi")
    ///     .first_attachment()
    ///     .is_none());
    /// ```
    #[must_use]
    pub fn first_attachment(&self) -> Option<&Attachment> {
        self.attachments.first()
    }

    /// Whether this event reports a delivery state rather than new content.
    ///
    /// ```
    /// use messenger_webhook::{Channel, DeliveryStatus, NormalizedMessage};
    ///
    /// let mut event = NormalizedMessage::inbound_text(Channel::WhatsApp, "wamid.1", "49", "");
    /// assert!(!event.is_status_update());
    /// event.status = Some(DeliveryStatus::Read);
    /// assert!(event.is_status_update());
    /// ```
    #[must_use]
    pub fn is_status_update(&self) -> bool {
        self.status.is_some()
    }

    /// Whether this event replaces the content of an earlier message.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// let mut event = NormalizedMessage::inbound_text(Channel::Telegram, "2", "1", "fixed typo");
    /// event.edit_of = Some("77".into());
    /// assert!(event.is_edit());
    /// ```
    #[must_use]
    pub fn is_edit(&self) -> bool {
        self.edit_of.is_some()
    }

    /// Whether the event carries formatting, i.e. [`Self::spans`] is not empty.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// assert!(!NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "plain").is_formatted());
    /// ```
    #[must_use]
    pub fn is_formatted(&self) -> bool {
        !self.spans.is_empty()
    }
}

/// One outbound request, provider independent.
///
/// This is the shape [`Adapter::encode`](crate::Adapter::encode) accepts. It
/// carries only fields an application can know before the provider assigns an
/// event id; provider-assigned ids belong to [`NormalizedMessage`] on the
/// inbound side and [`ProviderPayload::idempotency_key`] on the outbound side.
///
/// ```
/// use messenger_webhook::{Channel, OutboundMessage};
///
/// let message = OutboundMessage::text(Channel::Telegram, "-1001", "Hallo");
/// assert_eq!(message.conversation_id, "-1001");
/// assert_eq!(message.text.as_deref(), Some("Hallo"));
/// assert!(message.attachments.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct OutboundMessage {
    /// Destination channel.
    pub channel: Channel,
    /// Conversation the request is addressed to (chat id, room id, phone
    /// number).
    pub conversation_id: String,
    /// Plain text to send.
    pub text: Option<String>,
    /// Media carried by the request, in provider order.
    pub attachments: Vec<Attachment>,
    /// External id of the message this one replies to.
    pub reply_to: Option<String>,
    /// Thread or topic id, when the provider threads messages.
    pub thread_id: Option<String>,
    /// When this request is an edit: external id of the message it replaces.
    pub edit_of: Option<String>,
    /// Reaction carried by this request.
    ///
    /// Providers that expose reactions through a separate endpoint reject this
    /// field in `encode`, just as they do for edits.
    pub reaction: Option<Reaction>,
}

impl OutboundMessage {
    /// Builds a plain text outbound request.
    ///
    /// The idempotency key for an outbound retry is provider-specific and
    /// travels on [`ProviderPayload`], not here.
    ///
    /// ```
    /// use messenger_webhook::{Channel, OutboundMessage};
    ///
    /// let message = OutboundMessage::text(Channel::WhatsApp, "4915112345678", "Hallo");
    /// assert_eq!(message.channel, Channel::WhatsApp);
    /// assert_eq!(message.text.as_deref(), Some("Hallo"));
    /// ```
    #[must_use]
    pub fn text(
        channel: Channel,
        conversation_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            channel,
            conversation_id: conversation_id.into(),
            text: Some(text.into()),
            attachments: Vec::new(),
            reply_to: None,
            thread_id: None,
            edit_of: None,
            reaction: None,
        }
    }

    /// The first attachment, if the request carries media.
    ///
    /// ```
    /// use messenger_webhook::{Channel, OutboundMessage};
    ///
    /// assert!(OutboundMessage::text(Channel::Matrix, "!r:s", "hi")
    ///     .first_attachment()
    ///     .is_none());
    /// ```
    #[must_use]
    pub fn first_attachment(&self) -> Option<&Attachment> {
        self.attachments.first()
    }
}

/// HTTP method a provider payload uses.
///
/// The methods are typed so a payload cannot be built with a method the crate
/// does not support.
///
/// ```
/// use messenger_webhook::ProviderMethod;
///
/// assert_eq!(ProviderMethod::Post.as_str(), "POST");
/// assert_eq!(ProviderMethod::Put.as_str(), "PUT");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProviderMethod {
    /// `POST`, used by the `WhatsApp` and Telegram bot APIs.
    Post,
    /// `PUT`, used by Matrix client-server room sends.
    Put,
}

impl ProviderMethod {
    /// The wire spelling of this method.
    ///
    /// ```
    /// use messenger_webhook::ProviderMethod;
    ///
    /// assert_eq!(ProviderMethod::Post.as_str(), "POST");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Post => "POST",
            Self::Put => "PUT",
        }
    }
}

/// An outbound request body for one provider, ready for the caller to send.
///
/// The crate performs no I/O: the payload carries the HTTP method, the content
/// type and a JSON object body, and the application decides where to send it.
///
/// ```
/// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload};
///
/// let payload = ProviderPayload::new(
///     Channel::Telegram,
///     ProviderMethod::Post,
///     "application/json",
///     br#"{"chat_id":"1","text":"hi"}"#.to_vec(),
/// )?;
/// assert_eq!(payload.method(), "POST");
/// assert_eq!(payload.json().expect("json")["text"], "hi");
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPayload {
    channel: Channel,
    method: ProviderMethod,
    content_type: &'static str,
    body: Vec<u8>,
    idempotency_key: Option<IdempotencyKey>,
    retry: RetryPolicy,
}

impl ProviderPayload {
    /// Wraps a serialised JSON object body.
    ///
    /// The content type must be `application/json`, the body must be a JSON
    /// object, and it must fit within [`crate::MAX_BODY_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Payload`] when `content_type` is not
    /// `application/json` or `body` is not a JSON object, and
    /// [`crate::Error::BodyTooLarge`] when `body` exceeds
    /// [`crate::MAX_BODY_BYTES`].
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Matrix,
    ///     ProviderMethod::Put,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert_eq!(payload.body(), b"{}");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn new(
        channel: Channel,
        method: ProviderMethod,
        content_type: &'static str,
        body: Vec<u8>,
    ) -> crate::Result<Self> {
        Self::with_retry(
            channel,
            method,
            content_type,
            body,
            RetryPolicy::UnsafeUnlessKeyPresent,
            None,
        )
    }

    /// Wraps a serialised JSON object body together with its retry contract.
    ///
    /// Adapters use this constructor when they know whether repeating the
    /// request is safe. The plain [`ProviderPayload::new`] constructor keeps the
    /// conservative default: a payload is unsafe to retry unless it carries an
    /// idempotency key.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Payload`] when `content_type` is not
    /// `application/json` or `body` is not a JSON object, and
    /// [`crate::Error::BodyTooLarge`] when `body` exceeds
    /// [`crate::MAX_BODY_BYTES`].
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload, RetryPolicy};
    ///
    /// let payload = ProviderPayload::with_retry(
    ///     Channel::other("echo").expect("echo is a valid channel"),
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     b"{}".to_vec(),
    ///     RetryPolicy::Safe,
    ///     None,
    /// )?;
    /// assert_eq!(payload.retry(), RetryPolicy::Safe);
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn with_retry(
        channel: Channel,
        method: ProviderMethod,
        content_type: &'static str,
        body: Vec<u8>,
        retry: RetryPolicy,
        idempotency_key: Option<IdempotencyKey>,
    ) -> crate::Result<Self> {
        if content_type != "application/json" {
            return Err(crate::Error::Payload(
                "outbound payload content type must be application/json".to_owned(),
            ));
        }
        crate::check_body(body.len())?;
        let value: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|error| crate::Error::Payload(error.to_string()))?;
        if !value.is_object() {
            return Err(crate::Error::Payload(
                "outbound payload body must be a JSON object".to_owned(),
            ));
        }
        Ok(Self {
            channel,
            method,
            content_type,
            body,
            idempotency_key,
            retry,
        })
    }

    /// The retry contract for this payload.
    ///
    /// A caller must not repeat a payload blindly when this is
    /// [`RetryPolicy::UnsafeUnlessKeyPresent`]. Matrix payloads use
    /// [`RetryPolicy::Safe`] because their request path includes
    /// [`ProviderPayload::idempotency_key`]. A caller that retries that send
    /// must reuse the payload, or at least the same key, rather than calling
    /// [`crate::Adapter::encode`] again.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload, RetryPolicy};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Telegram,
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert_eq!(payload.retry(), RetryPolicy::UnsafeUnlessKeyPresent);
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub const fn retry(&self) -> RetryPolicy {
        self.retry
    }

    /// The provider idempotency key for this payload, if any.
    ///
    /// Matrix encodes this as the transaction id in `PUT /rooms/.../{txnId}`.
    /// Providers with no such endpoint leave it `None`. A caller that retries a
    /// Matrix send must reuse this key for every attempt.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload, RetryPolicy};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::WhatsApp,
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert!(payload.idempotency_key().is_none());
    /// assert!(payload.retry().is_unsafe_without_key());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn idempotency_key(&self) -> Option<&IdempotencyKey> {
        self.idempotency_key.as_ref()
    }

    /// Channel this payload is addressed to.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Telegram,
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert_eq!(payload.channel(), &Channel::Telegram);
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// HTTP method the provider expects (`POST` for bot APIs, `PUT` for Matrix).
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Matrix,
    ///     ProviderMethod::Put,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert_eq!(payload.method(), "PUT");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn method(&self) -> &'static str {
        self.method.as_str()
    }

    /// Content type of [`ProviderPayload::body`].
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Telegram,
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert_eq!(payload.content_type(), "application/json");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn content_type(&self) -> &'static str {
        self.content_type
    }

    /// The serialised JSON body.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Telegram,
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert!(!payload.body().is_empty());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Consumes the payload and returns its body.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Telegram,
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert_eq!(payload.into_body(), b"{}");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// Parses the body back into JSON, for inspection or tests.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Payload`] if the body is not valid JSON. The
    /// constructors reject malformed and non-object bodies, so this cannot
    /// fail for payloads this crate produces.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Telegram,
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     br#"{"ok":true}"#.to_vec(),
    /// )?;
    /// assert!(payload.json().is_ok());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn json(&self) -> crate::Result<serde_json::Value> {
        serde_json::from_slice(&self.body).map_err(|error| crate::Error::Payload(error.to_string()))
    }
}
