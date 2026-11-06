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
use unicode_general_category::{get_general_category, GeneralCategory};

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
    /// Meta Messenger Platform.
    Messenger,
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
            Self::Messenger => "messenger",
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
            "messenger" => Ok(Self::Messenger),
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
    if matches!(name, "whatsapp" | "messenger" | "telegram" | "matrix") {
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
/// Location and contact cards use the typed fields below; providers with other
/// card shapes still keep their original object in
/// [`NormalizedMessage::raw`].
///
/// ```
/// use messenger_webhook::{Attachment, AttachmentKind};
///
/// let mut photo = Attachment::new(AttachmentKind::Image);
/// photo.mime_type = Some("image/jpeg".into());
/// photo.size_bytes = Some(48_213);
/// assert_eq!(photo.kind, AttachmentKind::Image);
/// assert_eq!(photo.filename, None);
///
/// let mut location = Attachment::new(AttachmentKind::Location);
/// location.latitude = serde_json::Number::from_f64(48.7);
/// location.longitude = serde_json::Number::from_f64(11.4);
/// assert_eq!(location.latitude.as_ref().and_then(serde_json::Number::as_f64), Some(48.7));
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
    /// Latitude of a location attachment, when the provider reports one.
    pub latitude: Option<serde_json::Number>,
    /// Longitude of a location attachment, when the provider reports one.
    pub longitude: Option<serde_json::Number>,
    /// Phone number of a contact attachment, when the provider reports one.
    pub phone_number: Option<String>,
    /// First name of a contact attachment, when the provider reports one.
    pub first_name: Option<String>,
    /// Last name of a contact attachment, when the provider reports one.
    pub last_name: Option<String>,
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
/// The reaction state is one value, not two independent fields: an added
/// reaction always has a non-empty key, and a removal always has no current
/// key. Use [`Reaction::add`] or [`Reaction::remove`] to construct one.
///
/// ```
/// use messenger_webhook::Reaction;
///
/// let reaction = Reaction::add("wamid.1", "👍")?;
/// assert_eq!(reaction.message_id(), "wamid.1");
/// assert_eq!(reaction.emoji(), "👍");
/// assert!(!reaction.removed());
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    message_id: String,
    emoji: String,
    removed: bool,
    added_reactions: Vec<String>,
    removed_reactions: Vec<String>,
}

impl Reaction {
    /// Builds an added reaction with a non-empty reaction key.
    ///
    /// The key is the provider's emoji or custom-emoji id. Empty, whitespace
    /// and non-graphic-only keys are rejected because they cannot identify a
    /// reaction.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidReactionKey`] when `emoji` does not carry
    /// at least one graphic character.
    ///
    /// ```
    /// use messenger_webhook::Reaction;
    ///
    /// let reaction = Reaction::add("wamid.1", "👍")?;
    /// assert_eq!(reaction.emoji(), "👍");
    /// assert_eq!(reaction.added_reactions(), ["👍"]);
    /// assert!(Reaction::add("wamid.1", "").is_err());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn add(message_id: impl Into<String>, emoji: impl Into<String>) -> crate::Result<Self> {
        let emoji = emoji.into();
        if !has_graphic(&emoji) {
            return Err(crate::Error::InvalidReactionKey {
                reason: "an added reaction key must contain a visible character".to_owned(),
            });
        }
        let added_reactions = vec![emoji.clone()];
        Self::from_parts(message_id, emoji, false, added_reactions, Vec::new())
    }

    /// Builds a reaction removal.
    ///
    /// ```
    /// use messenger_webhook::Reaction;
    ///
    /// let reaction = Reaction::remove("wamid.1");
    /// assert_eq!(reaction.message_id(), "wamid.1");
    /// assert_eq!(reaction.emoji(), "");
    /// assert!(reaction.removed());
    /// ```
    #[must_use]
    pub fn remove(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            emoji: String::new(),
            removed: true,
            added_reactions: Vec::new(),
            removed_reactions: Vec::new(),
        }
    }

    /// Builds a reaction change from adapter-provided before/after state.
    pub(crate) fn from_parts(
        message_id: impl Into<String>,
        emoji: impl Into<String>,
        removed: bool,
        added_reactions: Vec<String>,
        removed_reactions: Vec<String>,
    ) -> crate::Result<Self> {
        let emoji = emoji.into();
        if removed {
            if !emoji.is_empty() {
                return Err(crate::Error::InvalidReactionKey {
                    reason: "a removed reaction cannot carry a current key".to_owned(),
                });
            }
        } else if !emoji.is_empty() && !has_graphic(&emoji) {
            return Err(crate::Error::InvalidReactionKey {
                reason: "an added reaction key must contain a visible character".to_owned(),
            });
        }
        Ok(Self {
            message_id: message_id.into(),
            emoji,
            removed,
            added_reactions,
            removed_reactions,
        })
    }

    /// External id of the message being reacted to.
    ///
    /// ```
    /// use messenger_webhook::Reaction;
    ///
    /// assert_eq!(Reaction::remove("wamid.1").message_id(), "wamid.1");
    /// ```
    #[must_use]
    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    /// The first current reaction key, empty when none remains.
    ///
    /// ```
    /// use messenger_webhook::Reaction;
    ///
    /// assert_eq!(Reaction::add("wamid.1", "👍")?.emoji(), "👍");
    /// assert_eq!(Reaction::remove("wamid.1").emoji(), "");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn emoji(&self) -> &str {
        &self.emoji
    }

    /// Whether this event leaves no reaction on the message.
    ///
    /// ```
    /// use messenger_webhook::Reaction;
    ///
    /// assert!(!Reaction::add("wamid.1", "👍")?.removed());
    /// assert!(Reaction::remove("wamid.1").removed());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn removed(&self) -> bool {
        self.removed
    }

    /// Reaction keys present in the update but not in the prior state.
    ///
    /// ```
    /// use messenger_webhook::Reaction;
    ///
    /// assert_eq!(Reaction::add("wamid.1", "👍")?.added_reactions(), ["👍"]);
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn added_reactions(&self) -> &[String] {
        &self.added_reactions
    }

    /// Reaction keys present in the prior state but not in the update.
    ///
    /// ```
    /// use messenger_webhook::Reaction;
    ///
    /// assert!(Reaction::remove("wamid.1").removed_reactions().is_empty());
    /// ```
    #[must_use]
    pub fn removed_reactions(&self) -> &[String] {
        &self.removed_reactions
    }
}

/// A chosen option from an interactive message.
///
/// When a reader taps a reply button or list row, the provider reports the
/// option's `id` and its human-readable `title`. The id is the value a bot has
/// to echo back to act on the choice, so it is carried alongside the title
/// instead of being flattened into [`NormalizedMessage::text`], which only ever
/// holds the title.
///
/// ```
/// use messenger_webhook::InteractiveReply;
///
/// let reply = InteractiveReply::new("yes", "Yes, please")?;
/// assert_eq!(reply.id(), "yes");
/// assert_eq!(reply.title(), "Yes, please");
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveReply {
    id: String,
    title: String,
}

impl InteractiveReply {
    /// Wraps a chosen option, validating what the provider rejects on the wire.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidInteractiveReply`] when `id` carries no
    /// graphic character (so a tap could not be answered), when `id` is longer
    /// than [`MAX_INTERACTIVE_ID_BYTES`], or when `title` is empty or longer
    /// than [`MAX_INTERACTIVE_TITLE_CHARS`].
    ///
    /// ```
    /// use messenger_webhook::InteractiveReply;
    ///
    /// let reply = InteractiveReply::new("confirm", "Confirm")?;
    /// assert_eq!(reply.id(), "confirm");
    /// assert!(InteractiveReply::new("", "Confirm").is_err());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> crate::Result<Self> {
        let id = id.into();
        if !has_graphic(&id) {
            return Err(crate::Error::InvalidInteractiveReply {
                reason: "option id must contain a visible character".to_owned(),
            });
        }
        if id.len() > MAX_INTERACTIVE_ID_BYTES {
            return Err(crate::Error::InvalidInteractiveReply {
                reason: format!(
                    "option id is {} bytes, the ceiling is {MAX_INTERACTIVE_ID_BYTES}",
                    id.len()
                ),
            });
        }
        let title = title.into();
        let title_len = title.chars().count();
        if title.is_empty() {
            return Err(crate::Error::InvalidInteractiveReply {
                reason: "option title is empty".to_owned(),
            });
        }
        if title_len > MAX_INTERACTIVE_TITLE_CHARS {
            return Err(crate::Error::InvalidInteractiveReply {
                reason: format!(
                    "option title is {title_len} characters, the ceiling is \
                     {MAX_INTERACTIVE_TITLE_CHARS}"
                ),
            });
        }
        Ok(Self { id, title })
    }

    /// The option id the provider expects a bot to echo back.
    ///
    /// ```
    /// use messenger_webhook::InteractiveReply;
    ///
    /// assert_eq!(InteractiveReply::new("yes", "Yes")?.id(), "yes");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The option title that was shown to the reader.
    ///
    /// ```
    /// use messenger_webhook::InteractiveReply;
    ///
    /// assert_eq!(InteractiveReply::new("yes", "Yes")?.title(), "Yes");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
}

/// Longest interactive option id this crate will carry, in bytes.
///
/// Provider limits differ; this is a local guard so an unbounded string cannot
/// travel through a normalised message or an outbound payload.
///
/// ```
/// assert_eq!(messenger_webhook::MAX_INTERACTIVE_ID_BYTES, 256);
/// ```
pub const MAX_INTERACTIVE_ID_BYTES: usize = 256;

/// Longest interactive option title this crate will carry, in characters.
///
/// `WhatsApp` reply buttons cap the visible title at 20 characters, which is
/// the tightest ceiling this crate sends against.
///
/// ```
/// assert_eq!(messenger_webhook::MAX_INTERACTIVE_TITLE_CHARS, 20);
/// ```
pub const MAX_INTERACTIVE_TITLE_CHARS: usize = 20;

/// Whether `text` holds at least one character a reader would see.
///
/// A provider id of `" "` or `"\u{200b}"` names nothing, so treating it as a
/// real id collapses every such event onto one key and the ledger drops all but
/// the first. Whitespace is not enough to decide this - a zero-width space is a
/// `Cf` format character, not whitespace - so the general category is used:
/// a control, format, surrogate, private-use, unassigned or separator code
/// point is padding, anything else is a real id.
pub(crate) fn has_graphic(text: &str) -> bool {
    text.chars().any(|c| {
        !matches!(
            get_general_category(c),
            GeneralCategory::Control
                | GeneralCategory::Format
                | GeneralCategory::Surrogate
                | GeneralCategory::PrivateUse
                | GeneralCategory::Unassigned
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
                | GeneralCategory::SpaceSeparator
        )
    })
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
    /// [`SYNTHETIC_ID_PREFIX`]) instead of dropping the event. The digest
    /// includes the object's position in the webhook envelope, so a replay
    /// dedupes while byte-identical objects at different positions remain
    /// distinct. [`Self::has_synthetic_id`] tells the two apart. A
    /// provider-assigned id that starts with [`SYNTHETIC_ID_PREFIX`] is retagged
    /// before it reaches this field, so the prefix stays a reliable provenance
    /// signal.
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
    /// External id of the message [`Self::status`] refers to, when it is not
    /// the same as [`Self::external_id`].
    ///
    /// Matrix `m.receipt` events carry the receipt event's own `event_id` in
    /// [`Self::external_id`] and name the message that was read here.
    pub status_for: Option<String>,
    /// Chosen option carried by an interactive reply, when the provider sends
    /// one. The option id is the value a bot must echo back; it is kept
    /// separate from [`Self::text`], which only holds the title.
    pub interactive_reply: Option<InteractiveReply>,
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
            status_for: None,
            interactive_reply: None,
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
    /// synthetic id — the digest of the raw event and its envelope position, so
    /// a replay still dedupes. Applications that need the provider's own key (to
    /// call back into the provider, for example) check this first.
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

    /// The chosen option carried by an interactive reply, when there is one.
    ///
    /// ```
    /// use messenger_webhook::{Channel, InteractiveReply, NormalizedMessage};
    ///
    /// let reply = InteractiveReply::new("yes", "Yes, please")?;
    /// let mut message = NormalizedMessage::inbound_text(Channel::WhatsApp, "wamid.1", "49", "Yes, please");
    /// message.interactive_reply = Some(reply);
    /// assert_eq!(message.interactive_reply().map(InteractiveReply::id), Some("yes"));
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn interactive_reply(&self) -> Option<&InteractiveReply> {
        self.interactive_reply.as_ref()
    }

    /// Returns this event with the chosen option set.
    ///
    /// ```
    /// use messenger_webhook::{Channel, InteractiveReply, NormalizedMessage};
    ///
    /// let reply = InteractiveReply::new("no", "No thanks")?;
    /// let message = NormalizedMessage::inbound_text(Channel::WhatsApp, "wamid.1", "49", "No thanks")
    ///     .with_interactive_reply(reply);
    /// assert_eq!(message.interactive_reply().map(InteractiveReply::title), Some("No thanks"));
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn with_interactive_reply(mut self, reply: InteractiveReply) -> Self {
        self.interactive_reply = Some(reply);
        self
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
    /// [`Adapter::encode`](crate::Adapter::encode) turns this into a provider
    /// reaction request, and the returned [`ProviderPayload`] reports
    /// [`ProviderOperation::Reaction`] so the caller can route it to the
    /// provider's reaction endpoint. A removal ([`Reaction::remove`]) is sent
    /// when the provider documents one; Matrix documents removal through
    /// `MatrixClient::redact`, so a Matrix removal is rejected here instead of
    /// being sent as an annotation.
    pub reaction: Option<Reaction>,
    /// Interactive reply button to send, when the request asks for one.
    ///
    /// Providers that support it render a reply button the reader can tap; the
    /// option id travels back through [`NormalizedMessage::interactive_reply`].
    pub interactive_reply: Option<InteractiveReply>,
    /// Formatted runs inside [`Self::text`], in caller order.
    ///
    /// Offsets are UTF-8 byte offsets into `text`, matching
    /// [`NormalizedMessage::spans`](crate::NormalizedMessage::spans). A span
    /// that does not slice the outbound text is rejected by `encode` with
    /// [`Error::InvalidMessage`](crate::Error::InvalidMessage) instead of
    /// being silently dropped.
    pub spans: Vec<TextSpan>,
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
            interactive_reply: None,
            spans: Vec::new(),
        }
    }

    /// The formatted runs this request carries.
    ///
    /// ```
    /// use messenger_webhook::{Channel, OutboundMessage, TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// let message = OutboundMessage::text(Channel::Telegram, "-1001", "Hallo Welt")
    ///     .with_spans([span]);
    /// assert_eq!(message.spans().len(), 1);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn spans(&self) -> &[TextSpan] {
        &self.spans
    }

    /// Returns this request with formatted runs set from `spans`.
    ///
    /// The setter stores the spans as given. [`Adapter::encode`](crate::Adapter::encode)
    /// validates them against the final text before a provider payload is
    /// built, so a mismatched span becomes [`Error::InvalidMessage`](crate::Error::InvalidMessage)
    /// rather than disappearing.
    ///
    /// ```
    /// use messenger_webhook::{Channel, OutboundMessage, TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 0, 5, TextFormat::Italic)?;
    /// let message = OutboundMessage::text(Channel::Telegram, "-1001", "Hallo Welt")
    ///     .with_spans([span]);
    /// assert_eq!(message.spans()[0].format, TextFormat::Italic);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn with_spans(mut self, spans: impl IntoIterator<Item = TextSpan>) -> Self {
        self.spans = spans.into_iter().collect();
        self
    }

    /// Returns this request with `text` and formatted runs set together.
    ///
    /// ```
    /// use messenger_webhook::{Channel, OutboundMessage, TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// let message = OutboundMessage::text(Channel::Telegram, "-1001", "")
    ///     .with_text_and_spans("Hallo Welt", [span]);
    /// assert_eq!(message.text.as_deref(), Some("Hallo Welt"));
    /// assert_eq!(message.spans()[0].end, 10);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn with_text_and_spans(
        mut self,
        text: impl Into<String>,
        spans: impl IntoIterator<Item = TextSpan>,
    ) -> Self {
        self.text = Some(text.into());
        self.spans = spans.into_iter().collect();
        self
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

/// The outbound operation a [`ProviderPayload`] performs.
///
/// A payload's HTTP method is the same for a send, an edit and a reaction on
/// most providers, so the caller cannot tell them apart from
/// [`ProviderPayload::method`] alone. This enum says which endpoint or event
/// type the body belongs to, so a caller routes it to the provider's documented
/// send, edit or reaction endpoint instead of guessing.
///
/// ```
/// use messenger_webhook::ProviderOperation;
///
/// assert_eq!(ProviderOperation::Message.as_str(), "send");
/// assert_eq!(ProviderOperation::Edit.as_str(), "edit");
/// assert_eq!(ProviderOperation::Reaction.as_str(), "reaction");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProviderOperation {
    /// A new message (text, media or an interactive reply).
    Message,
    /// An edit of an earlier message, addressed by `edit_of`.
    Edit,
    /// A reaction to an earlier message, added or removed.
    Reaction,
}

impl ProviderOperation {
    /// The stable, lower-case spelling of this operation.
    ///
    /// ```
    /// use messenger_webhook::ProviderOperation;
    ///
    /// assert_eq!(ProviderOperation::Message.as_str(), "send");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "send",
            Self::Edit => "edit",
            Self::Reaction => "reaction",
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
    operation: ProviderOperation,
    event_type: Option<&'static str>,
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
    /// `application/json` or `body` is valid JSON but not an object, and
    /// [`crate::Error::Json`] when `body` is not valid JSON. Returns
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
    /// `application/json` or `body` is valid JSON but not an object, and
    /// [`crate::Error::Json`] when `body` is not valid JSON. Returns
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
        crate::check_body(body.len(), crate::MAX_BODY_BYTES)?;
        let value: serde_json::Value = serde_json::from_slice(&body)?;
        if !value.is_object() {
            return Err(crate::Error::Payload(
                "outbound payload body must be a JSON object".to_owned(),
            ));
        }
        Ok(Self {
            channel,
            method,
            operation: ProviderOperation::Message,
            event_type: None,
            content_type,
            body,
            idempotency_key,
            retry,
        })
    }

    /// Records the provider operation and Matrix event type for this payload.
    ///
    /// Adapters call this after building a body so a caller can route an edit
    /// or reaction to the provider's documented endpoint. The Matrix
    /// client-server API carries both in the request path, so `event_type` is
    /// the event type the send leg belongs to (`m.room.message` or
    /// `m.reaction`); providers that keep the event type out of their REST
    /// surface leave it `None`.
    #[must_use]
    pub(crate) fn with_operation(
        mut self,
        operation: ProviderOperation,
        event_type: Option<&'static str>,
    ) -> Self {
        self.operation = operation;
        self.event_type = event_type;
        self
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

    /// Which outbound operation this payload performs.
    ///
    /// The method alone does not separate a send, an edit and a reaction: on
    /// Telegram all three are `POST`s, and the adapter encodes an edit or
    /// reaction into the body of the payload it returns. This tells the caller
    /// which provider endpoint the body belongs to.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderMethod, ProviderOperation, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(
    ///     Channel::Telegram,
    ///     ProviderMethod::Post,
    ///     "application/json",
    ///     b"{}".to_vec(),
    /// )?;
    /// assert_eq!(payload.operation(), ProviderOperation::Message);
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub const fn operation(&self) -> ProviderOperation {
        self.operation
    }

    /// The Matrix event type this payload belongs to, when the provider carries
    /// one in the request path.
    ///
    /// Matrix builds `PUT /rooms/{room}/send/{type}/{txn}`, so the event type is
    /// part of the route: `m.room.message` for a send or edit and `m.reaction`
    /// for a reaction. Providers whose REST surface keeps the event type inside
    /// the body (the `WhatsApp` and Telegram bot APIs) return `None`.
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
    /// assert_eq!(payload.event_type(), None);
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub const fn event_type(&self) -> Option<&'static str> {
        self.event_type
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
    /// Returns [`crate::Error::Json`] if the body is not valid JSON. The
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
        Ok(serde_json::from_slice(&self.body)?)
    }
}
