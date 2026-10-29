//! Matrix adapter — client-server `m.room.message` events and application
//! service transactions.
//!
//! Matrix is JSON over HTTP with no payload signature of its own: an
//! application service authenticates with its `hs_token` as a bearer token.
//! This module is pure: it never talks to a homeserver, it only normalises the
//! events a homeserver pushes and builds the `content` a caller `PUT`s to
//! `/rooms/{roomId}/send/m.room.message/{txnId}`.
//!
//! ```
//! use messenger_webhook::{Adapter, Channel, MatrixAdapter, NormalizedMessage};
//!
//! let adapter = MatrixAdapter::new("hs-token");
//! let message = NormalizedMessage::text(Channel::Matrix, "!room:example.org", "Hallo");
//! let payload = adapter.encode(&message)?;
//! assert_eq!(payload.method(), "PUT");
//! assert_eq!(payload.json()?["msgtype"], "m.text");
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

use serde_json::Value;

use crate::error::{Error, Result};
use crate::headers::Headers;
use crate::id;
use crate::json;
use crate::model::ProviderPayload;
use crate::model::{
    Attachment, AttachmentKind, Channel, DeliveryStatus, NormalizedMessage, Reaction,
};
use crate::secret::Secret;
use crate::{check_body, json_payload, recipient, Adapter, MAX_BODY_BYTES};

/// Header carrying the application service token.
pub const AUTHORIZATION_HEADER: &str = "authorization";

/// Adapter for Matrix room events.
///
/// Accepts a single event object or an application service transaction
/// (`{"events": [...]}`).
///
/// ```
/// use messenger_webhook::{Adapter, Headers, MatrixAdapter};
///
/// let adapter = MatrixAdapter::new("hs-token");
/// let body = br#"{"type":"m.room.message","event_id":"$1","room_id":"!r:example.org",
///     "sender":"@a:example.org","origin_server_ts":1729600000000,
///     "content":{"msgtype":"m.text","body":"Hallo"}}"#;
///
/// let headers = Headers::from_iter([("authorization", "Bearer hs-token")]);
/// adapter.verify(&headers, body)?;
///
/// let events = adapter.adapt(body)?;
/// assert_eq!(events[0].text.as_deref(), Some("Hallo"));
/// assert_eq!(events[0].timestamp, Some(1_729_600_000));
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct MatrixAdapter {
    token: Secret,
}

impl MatrixAdapter {
    /// Creates an adapter that verifies with the application service
    /// `hs_token`.
    ///
    /// ```
    /// use messenger_webhook::{Adapter, MatrixAdapter};
    ///
    /// assert_eq!(MatrixAdapter::new("t").channel().as_str(), "matrix");
    /// ```
    #[must_use]
    pub fn new(hs_token: impl Into<String>) -> Self {
        Self {
            token: Secret::new(hs_token),
        }
    }
}

impl Adapter for MatrixAdapter {
    fn channel(&self) -> Channel {
        Channel::Matrix
    }

    fn verify(&self, headers: &Headers, body: &[u8]) -> Result<()> {
        check_body(MAX_BODY_BYTES, body.len())?;
        if self.token.is_empty() {
            return Err(Error::MissingSecret {
                channel: Channel::Matrix,
            });
        }
        let header = headers
            .get(AUTHORIZATION_HEADER)
            .ok_or(Error::MissingVerificationHeader {
                channel: Channel::Matrix,
                header: AUTHORIZATION_HEADER,
            })?;
        let (scheme, token) =
            header
                .split_once(' ')
                .ok_or_else(|| Error::MalformedVerificationHeader {
                    channel: Channel::Matrix,
                    reason: "expected `Bearer <token>`".to_owned(),
                })?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(Error::MalformedVerificationHeader {
                channel: Channel::Matrix,
                reason: format!("unsupported authorization scheme {scheme:?}"),
            });
        }
        if self.token.ct_eq(token.as_bytes()) {
            Ok(())
        } else {
            Err(Error::BadSignature {
                channel: Channel::Matrix,
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

        let events: Vec<&Value> = match json::child(&root, "events").and_then(Value::as_array) {
            Some(transaction) => transaction.iter().collect(),
            None => vec![&root],
        };
        Ok(events
            .into_iter()
            .filter(|entry| is_event(entry))
            .map(normalize_event)
            .collect())
    }

    fn encode(&self, message: &NormalizedMessage) -> Result<ProviderPayload> {
        let room = recipient(Channel::Matrix, message)?;
        if !room.starts_with('!') {
            return Err(Error::InvalidMessage {
                channel: Channel::Matrix,
                reason: format!("Matrix room ids start with '!', got {room:?}"),
            });
        }

        let text = message.text.as_deref().unwrap_or_default();
        let mut content = if let Some(attachment) = message.first_attachment() {
            media_content(attachment, text)?
        } else {
            if text.is_empty() {
                return Err(Error::InvalidMessage {
                    channel: Channel::Matrix,
                    reason: "message has neither text nor an attachment".to_owned(),
                });
            }
            let mut content = serde_json::Map::new();
            content.insert("msgtype".to_owned(), Value::String("m.text".to_owned()));
            content.insert("body".to_owned(), Value::String(text.to_owned()));
            content
        };

        apply_relations(&mut content, message);
        json_payload(Channel::Matrix, "PUT", &Value::Object(content))
    }
}

/// Builds the `content` of an `m.image`/`m.video`/`m.audio`/`m.file`/
/// `m.sticker` event.
fn media_content(attachment: &Attachment, text: &str) -> Result<serde_json::Map<String, Value>> {
    let msgtype = match attachment.kind {
        AttachmentKind::Image => "m.image",
        AttachmentKind::Video => "m.video",
        AttachmentKind::Audio | AttachmentKind::Voice => "m.audio",
        AttachmentKind::Document => "m.file",
        AttachmentKind::Sticker => "m.sticker",
        ref other => {
            return Err(Error::InvalidMessage {
                channel: Channel::Matrix,
                reason: format!(
                    "Matrix cannot send {} attachments as m.room.message",
                    other.as_str()
                ),
            });
        }
    };
    let url = attachment
        .url
        .as_ref()
        .or(attachment.id.as_ref())
        .ok_or_else(|| Error::InvalidMessage {
            channel: Channel::Matrix,
            reason: "attachment has neither a url nor an id".to_owned(),
        })?;
    if !url.starts_with("mxc://") {
        return Err(Error::InvalidMessage {
            channel: Channel::Matrix,
            reason: format!("Matrix media urls must be mxc://, got {url:?}"),
        });
    }

    let body_text = attachment
        .filename
        .clone()
        .or_else(|| attachment.caption.clone())
        .unwrap_or_else(|| {
            if text.is_empty() {
                attachment.kind.as_str().to_owned()
            } else {
                text.to_owned()
            }
        });

    let mut content = serde_json::Map::new();
    content.insert("msgtype".to_owned(), Value::String(msgtype.to_owned()));
    content.insert("body".to_owned(), Value::String(body_text));
    content.insert("url".to_owned(), Value::String(url.clone()));

    let mut info = serde_json::Map::new();
    if let Some(mime) = &attachment.mime_type {
        info.insert("mimetype".to_owned(), Value::String(mime.clone()));
    }
    if let Some(size) = attachment.size_bytes {
        info.insert("size".to_owned(), Value::from(size));
    }
    if let Some(width) = attachment.width {
        info.insert("w".to_owned(), Value::from(width));
    }
    if let Some(height) = attachment.height {
        info.insert("h".to_owned(), Value::from(height));
    }
    if let Some(duration) = attachment.duration_seconds {
        info.insert("duration".to_owned(), Value::from(duration));
    }
    if !info.is_empty() {
        content.insert("info".to_owned(), Value::Object(info));
    }
    Ok(content)
}

/// Adds `m.relates_to` (reply, thread or edit) to an outbound `content`.
fn apply_relations(content: &mut serde_json::Map<String, Value>, message: &NormalizedMessage) {
    if let Some(target) = &message.edit_of {
        let mut replacement = serde_json::Map::new();
        if let Some(msgtype) = content.get("msgtype") {
            replacement.insert("msgtype".to_owned(), msgtype.clone());
        }
        if let Some(body) = content.get("body") {
            replacement.insert("body".to_owned(), body.clone());
        }
        content.insert("m.new_content".to_owned(), Value::Object(replacement));
        content.insert(
            "m.relates_to".to_owned(),
            serde_json::json!({ "rel_type": "m.replace", "event_id": target }),
        );
    } else if let Some(thread) = &message.thread_id {
        let mut relates = serde_json::Map::new();
        relates.insert("rel_type".to_owned(), Value::String("m.thread".to_owned()));
        relates.insert("event_id".to_owned(), Value::String(thread.clone()));
        if let Some(reply) = &message.reply_to {
            relates.insert(
                "m.in_reply_to".to_owned(),
                serde_json::json!({ "event_id": reply }),
            );
        }
        content.insert("m.relates_to".to_owned(), Value::Object(relates));
    } else if let Some(reply) = &message.reply_to {
        content.insert(
            "m.relates_to".to_owned(),
            serde_json::json!({ "m.in_reply_to": { "event_id": reply } }),
        );
    }
}

/// Maps one Matrix event of any type.
fn normalize_event(event: &Value) -> NormalizedMessage {
    let event_type = json::string(event, "type").unwrap_or("unknown");
    let external_id = id::identify(event, "event_id", &Channel::Matrix, "event");
    let conversation_id = json::id(event, "room_id").unwrap_or_default();
    let sender_id = json::id(event, "sender").unwrap_or_default();
    let content = json::child(event, "content")
        .cloned()
        .unwrap_or(Value::Null);

    let mut normalized = NormalizedMessage {
        channel: Channel::Matrix,
        external_id,
        conversation_id,
        sender_id,
        text: None,
        timestamp: json::millis_to_seconds(json::integer(event, "origin_server_ts")),
        attachments: Vec::new(),
        reply_to: None,
        thread_id: None,
        edit_of: None,
        reaction: None,
        status: None,
        spans: Vec::new(),
        provider_type: Some(event_type.to_owned()),
        raw: Some(event.clone()),
    };

    match event_type {
        "m.room.message" | "m.sticker" => apply_content(&mut normalized, event_type, &content),
        "m.reaction" => {
            let relates = json::child(&content, "m.relates_to");
            let target = relates.and_then(|relates| json::id(relates, "event_id"));
            if let Some(message_id) = target {
                let emoji = relates
                    .and_then(|relates| json::string(relates, "key"))
                    .unwrap_or_default()
                    .to_owned();
                normalized.reaction = Some(Reaction {
                    message_id,
                    removed: emoji.is_empty(),
                    emoji,
                });
            }
        }
        "m.receipt" => apply_receipts(&mut normalized, &content),
        // Encrypted events, typing notifications, membership and every future
        // event type are carried with their provider name and full payload.
        _ => {}
    }

    normalized
}

/// Maps the `content` of an `m.room.message` (or `m.sticker`) event.
fn apply_content(normalized: &mut NormalizedMessage, event_type: &str, content: &Value) {
    let relates = json::child(content, "m.relates_to");
    normalized.reply_to = relates
        .and_then(|relates| json::child(relates, "m.in_reply_to"))
        .and_then(|reply| json::id(reply, "event_id"));

    if let Some(relates) = relates {
        match json::string(relates, "rel_type") {
            Some("m.replace") => {
                normalized.edit_of = json::id(relates, "event_id");
                // A replacement carries the new content in `m.new_content`.
                if let Some(replacement) = json::child(content, "m.new_content") {
                    normalized.text = json::string(replacement, "body").map(str::to_owned);
                }
            }
            Some("m.thread") => normalized.thread_id = json::id(relates, "event_id"),
            _ => {}
        }
    }

    if normalized.text.is_none() {
        normalized.text = json::string(content, "body").map(str::to_owned);
    }

    let msgtype = json::string(content, "msgtype").unwrap_or("m.text");
    let kind = match (event_type, msgtype) {
        ("m.sticker", _) => Some(AttachmentKind::Sticker),
        (_, "m.image") => Some(AttachmentKind::Image),
        (_, "m.video") => Some(AttachmentKind::Video),
        (_, "m.audio") => Some(AttachmentKind::Audio),
        (_, "m.file") => Some(AttachmentKind::Document),
        (_, "m.location") => Some(AttachmentKind::Location),
        (_, "m.text" | "m.notice" | "m.emote") => None,
        (_, other) => Some(AttachmentKind::Other(other.to_owned())),
    };

    if let Some(kind) = kind {
        let is_location = kind == AttachmentKind::Location;
        let info = json::child(content, "info");
        let mut attachment = Attachment::new(kind);
        attachment.id = json::id(content, "url")
            .or_else(|| json::child(content, "file").and_then(|file| json::id(file, "url")));
        attachment.url.clone_from(&attachment.id);
        attachment.mime_type = info
            .and_then(|info| json::string(info, "mimetype"))
            .map(str::to_owned);
        attachment.size_bytes = info.and_then(|info| json::unsigned(info, "size"));
        attachment.width = info.and_then(|info| json::narrow(json::unsigned(info, "w")));
        attachment.height = info.and_then(|info| json::narrow(json::unsigned(info, "h")));
        attachment.duration_seconds =
            info.and_then(|info| json::narrow(json::unsigned(info, "duration")));

        let body = json::string(content, "body");
        attachment.filename = json::string(content, "filename")
            .or(body)
            .map(str::to_owned);
        if is_location {
            attachment.filename = None;
            attachment.caption = json::string(content, "geo_uri").map(str::to_owned);
        }
        // For media the body is the filename, not the message text.
        normalized.text = None;
        normalized.attachments.push(attachment);
    }
}

/// Maps a receipt event: one read marker per referenced event id.
fn apply_receipts(normalized: &mut NormalizedMessage, content: &Value) {
    let Some(receipts) = content.as_object() else {
        return;
    };
    let Some((event_id, receipt)) = receipts.iter().next() else {
        return;
    };
    {
        let (status, reader) = match receipt.as_object() {
            Some(kinds) => {
                let status = if kinds.contains_key("m.read") {
                    DeliveryStatus::Read
                } else if let Some(kind) = kinds.keys().next() {
                    DeliveryStatus::Unknown(kind.clone())
                } else {
                    DeliveryStatus::Unknown("unknown".to_owned())
                };
                let reader = kinds
                    .values()
                    .find_map(|users| users.as_object().and_then(|users| users.keys().next()))
                    .cloned()
                    .unwrap_or_default();
                (status, reader)
            }
            None => (DeliveryStatus::Unknown("unknown".to_owned()), String::new()),
        };
        normalized.external_id.clone_from(event_id);
        normalized.status = Some(status);
        if !reader.is_empty() {
            normalized.sender_id = reader;
        }
    }
}

/// Whether `entry` carries a field a Matrix room event has.
///
/// A body from a different provider has none of them, and inventing an event
/// (and an id) out of foreign JSON would be worse than skipping it. Together
/// with a non-object entry this is the only case in which one is dropped.
fn is_event(entry: &Value) -> bool {
    ["event_id", "room_id", "type"]
        .iter()
        .any(|key| json::child(entry, key).is_some())
}
