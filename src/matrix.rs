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
//! use messenger_webhook::{Adapter, Channel, MatrixAdapter, OutboundMessage};
//!
//! let adapter = MatrixAdapter::new("hs-token");
//! let message = OutboundMessage::text(Channel::Matrix, "!room:example.org", "Hallo");
//! let payload = adapter.encode(&message)?;
//! assert_eq!(payload.method(), "PUT");
//! assert_eq!(payload.json()?["msgtype"], "m.text");
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};
use crate::headers::Headers;
use crate::id;
use crate::json;
use crate::model::{
    Attachment, AttachmentKind, Channel, DeliveryStatus, IdempotencyKey, NormalizedMessage,
    OutboundMessage, ProviderMethod, ProviderPayload, Reaction, RetryPolicy,
};
use crate::secret::Secret;
use crate::{check_body, payload, recipient, Adapter};

/// Process-wide sequence for Matrix adapter transaction ids.
static MATRIX_TXN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
/// assert_eq!(events[0].text(), Some("Hallo"));
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
        check_body(body.len())?;
        if self.token.is_empty() {
            return Err(Error::MissingSecret {
                channel: Channel::Matrix,
            });
        }
        let header = headers.exactly_one(Channel::Matrix, AUTHORIZATION_HEADER)?;
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
                reason: "authorization scheme is not Bearer".to_owned(),
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
        check_body(payload.len())?;
        let root: Value =
            serde_json::from_slice(payload).map_err(|error| Error::Payload(error.to_string()))?;
        if !root.is_object() {
            return Err(Error::Payload("expected a JSON object".to_owned()));
        }

        let events: Vec<&Value> = match root.get("events") {
            Some(events) => {
                if is_event(&root) {
                    return Err(Error::Payload(
                        "expected either a Matrix event or a transaction, not both".to_owned(),
                    ));
                }
                let Some(transaction) = events.as_array() else {
                    return Err(Error::Payload(
                        "expected matrix transaction `events` to be an array".to_owned(),
                    ));
                };
                transaction.iter().collect()
            }
            None => vec![&root],
        };
        events
            .into_iter()
            .filter(|entry| is_event(entry))
            .map(normalize_event)
            .collect()
    }

    fn encode(&self, message: &OutboundMessage) -> Result<ProviderPayload> {
        // The room id lives in the URL, not the content, but it is still the
        // recipient, so it goes through the shared validator before anything is
        // built.
        recipient(Channel::Matrix, message)?;

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

        apply_relations(&mut content, message)?;
        payload(
            Channel::Matrix,
            ProviderMethod::Put,
            &Value::Object(content),
            RetryPolicy::Safe,
            Some(matrix_transaction_key()),
        )
    }
}

/// Builds the transaction key Matrix needs to de-duplicate a send.
fn matrix_transaction_key() -> IdempotencyKey {
    let sequence = MATRIX_TXN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    IdempotencyKey::from_validated(format!("mw-{sequence:016x}"))
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
fn apply_relations(
    content: &mut serde_json::Map<String, Value>,
    message: &OutboundMessage,
) -> Result<()> {
    if let Some(target) = &message.edit_of {
        let mut replacement = content.clone();
        replacement.remove("m.relates_to");
        replacement.remove("m.new_content");
        if is_media_replacement_without_url(&replacement) {
            return Err(Error::InvalidMessage {
                channel: Channel::Matrix,
                reason: "Matrix media edits require a url in m.new_content".to_owned(),
            });
        }
        if let Some(body) = replacement.get("body").and_then(Value::as_str) {
            content.insert("body".to_owned(), Value::String(format!("* {body}")));
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
    Ok(())
}

/// Whether an edit replacement describes media but has no source url.
fn is_media_replacement_without_url(replacement: &serde_json::Map<String, Value>) -> bool {
    let media = matches!(
        replacement.get("msgtype").and_then(Value::as_str),
        Some("m.image" | "m.video" | "m.audio" | "m.file" | "m.sticker")
    );
    media && replacement.get("url").is_none()
}

/// Maps one Matrix event of any type.
fn normalize_event(event: &Value) -> Result<NormalizedMessage> {
    let event_type = json::string(event, "type").unwrap_or("unknown");
    let external_id = id::identify(event, "event_id", &Channel::Matrix, "event")?;
    let conversation_id = json::id(event, "room_id").unwrap_or_default();
    let sender_id = json::id(event, "sender").unwrap_or_default();
    let content = json::child(event, "content")
        .cloned()
        .unwrap_or(Value::Null);

    let mut normalized =
        NormalizedMessage::carrier(Channel::Matrix, external_id, conversation_id, sender_id);
    normalized.timestamp = json::millis_to_seconds(json::integer(event, "origin_server_ts"));
    normalized.provider_type = Some(event_type.to_owned());
    normalized.raw = Some(event.clone());

    match event_type {
        "m.room.message" | "m.sticker" => apply_content(&mut normalized, event_type, &content),
        "m.reaction" => {
            let relates = json::child(&content, "m.relates_to");
            let target = relates.and_then(|relates| json::id(relates, "event_id"));
            let emoji = relates.and_then(|relates| json::string(relates, "key"));
            if let (Some(message_id), Some(emoji)) = (target, emoji.filter(|key| !key.is_empty())) {
                normalized.reaction = Some(Reaction {
                    message_id,
                    removed: false,
                    emoji: emoji.to_owned(),
                    added_reactions: vec![emoji.to_owned()],
                    removed_reactions: Vec::new(),
                });
            }
        }
        "m.room.redaction" => {
            if let Some(redacts) = json::id(&content, "redacts") {
                normalized.external_id = redacts;
                normalized.status = Some(DeliveryStatus::Deleted);
            }
        }
        "m.receipt" => apply_receipts(&mut normalized, &content),
        // Encrypted events, typing notifications, membership and every future
        // event type are carried with their provider name and full payload.
        _ => {}
    }

    Ok(normalized)
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
                    normalized.set_text(json::string(replacement, "body").map(str::to_owned));
                }
            }
            Some("m.thread") => normalized.thread_id = json::id(relates, "event_id"),
            _ => {}
        }
    }

    if normalized.text().is_none() {
        normalized.set_text(json::string(content, "body").map(str::to_owned));
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
        normalized.set_text(None::<String>);
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
