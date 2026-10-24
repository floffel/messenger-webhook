//! The provider-neutral message model.
//!
//! One shape for everything a messenger reports: text, media, replies,
//! threads, reactions, edits and delivery receipts. Fields a provider cannot
//! express stay `None`; fields this crate does not model stay reachable in
//! [`NormalizedMessage::raw`].

use std::fmt;

/// Which messenger a message came from.
///
/// ```
/// use messenger_webhook::Channel;
///
/// assert_eq!(Channel::WhatsApp.as_str(), "whatsapp");
/// assert_eq!(Channel::Other("signal".into()).as_str(), "signal");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    /// `WhatsApp` Cloud API.
    WhatsApp,
    /// Telegram Bot API.
    Telegram,
    /// Matrix client-server or application-service events.
    Matrix,
    /// Anything else, verbatim.
    Other(String),
}

impl Channel {
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
            Self::Other(name) => name,
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of media an attachment is, independent of provider naming.
///
/// ```
/// use messenger_webhook::AttachmentKind;
///
/// assert_eq!(AttachmentKind::Audio.as_str(), "audio");
/// assert_eq!(AttachmentKind::Other("poll".into()).as_str(), "poll");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AttachmentKind {
    /// Still image.
    Image,
    /// Video, including round video notes and animations.
    Video,
    /// Audio file.
    Audio,
    /// Voice note / push-to-talk recording.
    Voice,
    /// Arbitrary file.
    Document,
    /// Sticker.
    Sticker,
    /// Geographic location.
    Location,
    /// Contact card.
    Contact,
    /// Anything the provider sends that this crate does not model.
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
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Voice => "voice",
            Self::Document => "document",
            Self::Sticker => "sticker",
            Self::Location => "location",
            Self::Contact => "contact",
            Self::Other(name) => name,
        }
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
/// let reaction = Reaction { message_id: "wamid.1".into(), emoji: "👍".into(), removed: false };
/// assert!(!reaction.removed);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    /// External id of the message being reacted to.
    pub message_id: String,
    /// The emoji, empty when the reaction was removed.
    pub emoji: String,
    /// Whether this event removes an earlier reaction.
    pub removed: bool,
}

/// Delivery state of an outbound message, as reported back by the provider.
///
/// ```
/// use messenger_webhook::DeliveryStatus;
///
/// assert_eq!(DeliveryStatus::Read.as_str(), "read");
/// assert_eq!(DeliveryStatus::Unknown("queued".into()).as_str(), "queued");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DeliveryStatus {
    /// Accepted by the provider.
    Sent,
    /// Delivered to the recipient's device.
    Delivered,
    /// Read by the recipient.
    Read,
    /// Delivery failed.
    Failed,
    /// Deleted by the sender.
    Deleted,
    /// A state this crate does not model, carried verbatim.
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
        match self {
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::Read => "read",
            Self::Failed => "failed",
            Self::Deleted => "deleted",
            Self::Unknown(name) => name,
        }
    }
}

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
/// let message = NormalizedMessage::text(Channel::Telegram, "-1001", "Hallo");
/// assert_eq!(message.conversation_id, "-1001");
/// assert_eq!(message.text.as_deref(), Some("Hallo"));
/// assert!(message.attachments.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedMessage {
    /// Origin channel.
    pub channel: Channel,
    /// Provider event id — use it as the idempotency key.
    pub external_id: String,
    /// Conversation the event belongs to (chat id, room id, phone number).
    pub conversation_id: String,
    /// Who sent it, in the provider's own id space.
    pub sender_id: String,
    /// Plain text, when the event carries any (captions included).
    pub text: Option<String>,
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
    /// Provider-native event name (`text`, `image`, `message_reaction`,
    /// `m.room.message`, …), verbatim including unknown types.
    pub provider_type: Option<String>,
    /// The provider object this event was normalised from.
    pub raw: Option<serde_json::Value>,
}

impl NormalizedMessage {
    /// Builds a plain text message — the shape the outbound encoder sends.
    ///
    /// `external_id` and `sender_id` stay empty: providers assign them, an
    /// application sending a message has no id yet.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// let message = NormalizedMessage::text(Channel::WhatsApp, "4915112345678", "Hallo");
    /// assert_eq!(message.channel, Channel::WhatsApp);
    /// assert_eq!(message.external_id, "");
    /// ```
    #[must_use]
    pub fn text(
        channel: Channel,
        conversation_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            channel,
            external_id: String::new(),
            conversation_id: conversation_id.into(),
            sender_id: String::new(),
            text: Some(text.into()),
            timestamp: None,
            attachments: Vec::new(),
            reply_to: None,
            thread_id: None,
            edit_of: None,
            reaction: None,
            status: None,
            provider_type: None,
            raw: None,
        }
    }

    /// The first attachment, if the event carries media.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage};
    ///
    /// assert!(NormalizedMessage::text(Channel::Matrix, "!r:s", "hi").first_attachment().is_none());
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
    /// let mut event = NormalizedMessage::text(Channel::WhatsApp, "49", "");
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
    /// let mut event = NormalizedMessage::text(Channel::Telegram, "1", "fixed typo");
    /// event.edit_of = Some("77".into());
    /// assert!(event.is_edit());
    /// ```
    #[must_use]
    pub fn is_edit(&self) -> bool {
        self.edit_of.is_some()
    }
}

/// An outbound request body for one provider, ready for the caller to send.
///
/// The crate performs no I/O: the payload carries the HTTP method, the content
/// type and the JSON body, and the application decides where to send it.
///
/// ```
/// use messenger_webhook::{Channel, ProviderPayload};
///
/// let payload = ProviderPayload::new(
///     Channel::Telegram,
///     "POST",
///     "application/json",
///     br#"{"chat_id":"1","text":"hi"}"#.to_vec(),
/// );
/// assert_eq!(payload.method(), "POST");
/// assert_eq!(payload.json().expect("json")["text"], "hi");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPayload {
    channel: Channel,
    method: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl ProviderPayload {
    /// Wraps a serialised body.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(Channel::Matrix, "PUT", "application/json", b"{}".to_vec());
    /// assert_eq!(payload.body(), b"{}");
    /// ```
    #[must_use]
    pub fn new(
        channel: Channel,
        method: &'static str,
        content_type: &'static str,
        body: Vec<u8>,
    ) -> Self {
        Self {
            channel,
            method,
            content_type,
            body,
        }
    }

    /// Channel this payload is addressed to.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(Channel::Telegram, "POST", "application/json", vec![]);
    /// assert_eq!(payload.channel(), &Channel::Telegram);
    /// ```
    #[must_use]
    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// HTTP method the provider expects (`POST` for bot APIs, `PUT` for Matrix).
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(Channel::Matrix, "PUT", "application/json", vec![]);
    /// assert_eq!(payload.method(), "PUT");
    /// ```
    #[must_use]
    pub fn method(&self) -> &'static str {
        self.method
    }

    /// Content type of [`ProviderPayload::body`].
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(Channel::Telegram, "POST", "application/json", vec![]);
    /// assert_eq!(payload.content_type(), "application/json");
    /// ```
    #[must_use]
    pub fn content_type(&self) -> &'static str {
        self.content_type
    }

    /// The serialised JSON body.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(Channel::Telegram, "POST", "application/json", b"{}".to_vec());
    /// assert!(!payload.body().is_empty());
    /// ```
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Consumes the payload and returns its body.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(Channel::Telegram, "POST", "application/json", b"{}".to_vec());
    /// assert_eq!(payload.into_body(), b"{}");
    /// ```
    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// Parses the body back into JSON, for inspection or tests.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Payload`] if the body is not valid JSON — which
    /// cannot happen for payloads this crate produces.
    ///
    /// ```
    /// use messenger_webhook::{Channel, ProviderPayload};
    ///
    /// let payload = ProviderPayload::new(Channel::Telegram, "POST", "application/json", b"[1]".to_vec());
    /// assert!(payload.json().is_ok());
    /// ```
    pub fn json(&self) -> crate::Result<serde_json::Value> {
        serde_json::from_slice(&self.body).map_err(|error| crate::Error::Payload(error.to_string()))
    }
}
