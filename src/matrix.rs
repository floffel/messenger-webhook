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
//! let adapter = MatrixAdapter::new("hs-token-hs-token")?;
//! let message = OutboundMessage::text(Channel::Matrix, "!room:example.org", "Hallo");
//! let payload = adapter.encode(&message)?;
//! assert_eq!(payload.method(), "PUT");
//! assert_eq!(payload.json()?["msgtype"], "m.text");
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, EndTag, StartTag, Tag, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};

use crate::error::{Error, Result};
use crate::headers::Headers;
use crate::id;
use crate::json;
use crate::model::{
    Attachment, AttachmentKind, Channel, DeliveryStatus, IdempotencyKey, NormalizedMessage,
    OutboundMessage, ProviderMethod, ProviderPayload, Reaction, RetryPolicy,
};
use crate::rich::{TextFormat, TextSpan};
use crate::secret::Secret;
use crate::{check_body, payload, recipient, Adapter};

/// Process-wide sequence for Matrix adapter transaction ids.
static MATRIX_TXN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Header carrying the application service token.
pub const AUTHORIZATION_HEADER: &str = "authorization";

/// Maximum accepted `formatted_body` size before HTML tokenization.
const MAX_MATRIX_FORMATTED_BODY_BYTES: usize = 64 * 1024;

/// Maximum nesting depth accepted from Matrix `formatted_body`.
const MAX_MATRIX_FORMATTED_BODY_DEPTH: usize = 100;

/// Maximum number of events collected while parsing one body.
const MAX_MATRIX_FORMATTED_BODY_EVENTS: usize = 8192;

/// Maximum number of formatting elements tracked while parsing one body.
const MAX_MATRIX_FORMATTED_BODY_ELEMENTS: usize = 1024;

/// Adapter for Matrix room events.
///
/// Accepts a single event object or an application service transaction
/// (`{"events": [...]}`).
///
/// ```
/// use messenger_webhook::{Adapter, Headers, MatrixAdapter};
///
/// let adapter = MatrixAdapter::new("hs-token-hs-token")?;
/// let body = br#"{"type":"m.room.message","event_id":"$1","room_id":"!r:example.org",
///     "sender":"@a:example.org","origin_server_ts":1729600000000,
///     "content":{"msgtype":"m.text","body":"Hallo"}}"#;
///
/// let headers = Headers::from_iter([("authorization", "Bearer hs-token-hs-token")]);
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
    /// # Errors
    ///
    /// [`Error::MissingSecret`] when `hs_token` is empty, and
    /// [`Error::WeakSecret`] when it is shorter than [`crate::secret::MIN_SECRET_BYTES`].
    ///
    /// ```
    /// use messenger_webhook::{Adapter, MatrixAdapter};
    ///
    /// assert_eq!(
    ///     MatrixAdapter::new("hs-token-hs-token")?
    ///         .channel()
    ///         .as_str(),
    ///     "matrix"
    /// );
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn new(hs_token: impl Into<String>) -> Result<Self> {
        let hs_token = hs_token.into();
        if hs_token.is_empty() {
            return Err(Error::MissingSecret {
                channel: Channel::Matrix,
            });
        }
        Ok(Self {
            token: Secret::new(hs_token)?,
        })
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
        let root: Value = serde_json::from_slice(payload)?;
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
        events.into_iter().filter(|entry| is_event(entry)).try_fold(
            Vec::new(),
            |mut messages, event| {
                messages.extend(normalize_event(event)?);
                Ok(messages)
            },
        )
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
fn normalize_event(event: &Value) -> Result<Vec<NormalizedMessage>> {
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
                normalized.reaction = Some(Reaction::add(message_id, emoji)?);
            }
        }
        "m.room.redaction" => {
            if let Some(redacts) = json::id(&content, "redacts") {
                normalized.external_id = redacts;
                normalized.status = Some(DeliveryStatus::Deleted);
            }
        }
        "m.receipt" => return Ok(apply_receipts(&normalized, &content)),
        // Encrypted events, typing notifications, membership and every future
        // event type are carried with their provider name and full payload.
        _ => {}
    }

    Ok(vec![normalized])
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

    if json::string(content, "format") == Some("org.matrix.custom.html") {
        let body = normalized.text().unwrap_or_default();
        if let Some(formatted_body) = json::string(content, "formatted_body") {
            normalized.set_spans(matrix_html_spans(body, formatted_body));
        }
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

/// Extracts formatting spans from Matrix's `org.matrix.custom.html` body.
///
/// The Matrix spec recommends HTML for rich text, but the neutral model keeps
/// plain `body` as the source of offsets. This function tokenizes the bounded
/// `formatted_body` with `html5ever` and only emits spans whose HTML text run
/// can be found verbatim in that plain body. If the two drift apart, the body
/// still normalizes and only the formatting is dropped.
fn matrix_html_spans(body: &str, formatted_body: &str) -> Vec<TextSpan> {
    if formatted_body.len() > MAX_MATRIX_FORMATTED_BODY_BYTES {
        return Vec::new();
    }

    let input = BufferQueue::default();
    input.push_back(StrTendril::from(formatted_body));
    let tokenizer = Tokenizer::new(MatrixHtmlSink::new(), TokenizerOpts::default());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    matrix_spans_from_events(body, tokenizer.sink.finish())
}

/// One start/end tag or text run collected from Matrix HTML.
#[derive(Debug)]
enum MatrixHtmlEvent {
    /// A start tag that may contribute a formatting span.
    Start {
        /// Tag name, lower-cased by the HTML tokenizer.
        tag: String,
        /// Link target for `a[href]`, when present and allowed.
        href: Option<String>,
    },
    /// Text emitted by the HTML tokenizer, with entities already decoded.
    Text(String),
    /// An end tag, matched against the open element stack by name.
    End(String),
}

/// Token sink that records supported Matrix formatting tags.
#[derive(Debug)]
struct MatrixHtmlSink {
    /// Events collected up to the configured event limit.
    events: std::cell::RefCell<Vec<MatrixHtmlEvent>>,
    /// Open element names, used to validate closing tags and depth limits.
    open: std::cell::RefCell<Vec<String>>,
    /// Number of formatting elements accepted so far.
    elements: std::cell::Cell<usize>,
    /// Stack depth whose text content is being skipped.
    skip_depth: std::cell::Cell<Option<usize>>,
    /// Set when any bound is exceeded; the caller then emits no spans.
    too_complex: std::cell::Cell<bool>,
}

impl MatrixHtmlSink {
    /// Creates an empty sink for one HTML body.
    fn new() -> Self {
        Self {
            events: std::cell::RefCell::new(Vec::new()),
            open: std::cell::RefCell::new(Vec::new()),
            elements: std::cell::Cell::new(0),
            skip_depth: std::cell::Cell::new(None),
            too_complex: std::cell::Cell::new(false),
        }
    }

    /// Records one event unless a bound has already been exceeded.
    fn push(&self, event: MatrixHtmlEvent) {
        let mut events = self.events.borrow_mut();
        if events.len() >= MAX_MATRIX_FORMATTED_BODY_EVENTS {
            self.too_complex.set(true);
            return;
        }
        events.push(event);
    }

    /// Records a decoded text token.
    fn record_text(&self, text: &str) {
        if self.skip_depth.get().is_none() && !text.is_empty() {
            self.push(MatrixHtmlEvent::Text(text.to_owned()));
        }
    }

    /// Records one start or end tag token.
    fn record_tag(&self, tag: &Tag) {
        let name = tag.name.to_string().to_ascii_lowercase();
        match tag.kind {
            StartTag => self.record_start_tag(&name, tag),
            EndTag => self.record_end_tag(&name),
        }
    }

    /// Records one start tag token.
    fn record_start_tag(&self, name: &str, tag: &Tag) {
        if self.skip_depth.get().is_some() {
            return;
        }
        if is_matrix_skipped_tag(name) {
            let open = self.open.borrow();
            self.skip_depth.set(Some(open.len()));
            return;
        }
        if self.open.borrow().len() >= MAX_MATRIX_FORMATTED_BODY_DEPTH {
            self.too_complex.set(true);
            return;
        }
        let elements = self.elements.get();
        if elements >= MAX_MATRIX_FORMATTED_BODY_ELEMENTS {
            self.too_complex.set(true);
            return;
        }
        self.elements.set(elements + 1);
        self.open.borrow_mut().push(name.to_owned());
        self.push(MatrixHtmlEvent::Start {
            tag: name.to_owned(),
            href: matrix_href(tag),
        });
    }

    /// Records one end tag token and any elements it implicitly closes.
    fn record_end_tag(&self, name: &str) {
        if let Some(depth) = self.skip_depth.get() {
            if is_matrix_skipped_tag(name) && self.open.borrow().len() == depth {
                self.skip_depth.set(None);
            }
            return;
        }

        let mut open = self.open.borrow_mut();
        let Some(index) = open.iter().rposition(|open| open == name) else {
            return;
        };
        let closed = open.split_off(index);
        drop(open);
        for tag in closed.into_iter().rev() {
            self.push(MatrixHtmlEvent::End(tag));
        }
    }

    /// Returns the collected events, or an empty vector when a bound was hit.
    fn finish(self) -> Vec<MatrixHtmlEvent> {
        if self.too_complex.get() {
            Vec::new()
        } else {
            self.events.into_inner()
        }
    }
}

impl TokenSink for MatrixHtmlSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        match token {
            Token::CharacterTokens(chars) => self.record_text(chars.as_ref()),
            Token::NullCharacterToken => self.record_text("\0"),
            Token::TagToken(tag) => self.record_tag(&tag),
            Token::EOFToken
            | Token::ParseError(_)
            | Token::CommentToken(_)
            | Token::DoctypeToken(_) => {}
        }
        TokenSinkResult::Continue
    }
}

/// Tags whose entire content must be skipped.
fn is_matrix_skipped_tag(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "mx-reply")
}

/// Extracts a Matrix-permitted `href` from an `a` tag.
fn matrix_href(tag: &Tag) -> Option<String> {
    tag.attrs
        .iter()
        .find(|attr| &*attr.name.local == "href")
        .map(|attr| attr.value.to_string())
        .filter(|href| is_matrix_permitted_href(href))
}

/// Whether `href` uses a Matrix-approved absolute URL scheme.
fn is_matrix_permitted_href(href: &str) -> bool {
    let Some((scheme, rest)) = href.split_once(':') else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ftp" | "mailto" | "magnet"
    )
}

/// Maps a Matrix HTML tag to a neutral span format.
fn matrix_format_for(tag: &str, href: Option<&str>) -> Option<TextFormat> {
    match tag {
        "b" | "strong" => Some(TextFormat::Bold),
        "i" | "em" => Some(TextFormat::Italic),
        "u" => Some(TextFormat::Underline),
        "s" | "del" => Some(TextFormat::Strikethrough),
        "code" => Some(TextFormat::Code),
        "pre" => Some(TextFormat::Pre { language: None }),
        "blockquote" => Some(TextFormat::Blockquote),
        "a" => href.map(|url| TextFormat::Link {
            url: url.to_owned(),
        }),
        other => Some(TextFormat::Other(other.to_owned())),
    }
}

/// Converts collected token events into spans over the plain body.
fn matrix_spans_from_events(body: &str, events: Vec<MatrixHtmlEvent>) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let mut active: Vec<(String, Option<String>, usize)> = Vec::new();
    let mut html = String::new();
    let mut runs = Vec::new();
    let mut body_cursor = 0_usize;
    for event in events {
        match event {
            MatrixHtmlEvent::Start { tag, href } => active.push((tag, href, html.len())),
            MatrixHtmlEvent::Text(text) => {
                let start = html.len();
                html.push_str(&text);
                runs.push((start, html.len()));
            }
            MatrixHtmlEvent::End(tag) => {
                while let Some((open_tag, href, start)) = active.pop() {
                    if open_tag == tag {
                        let end = html.len();
                        if let Some((body_start, body_end)) =
                            align_html_run(body, &html, &runs, start, end, &mut body_cursor)
                        {
                            if let Some(format) = matrix_format_for(&open_tag, href.as_deref()) {
                                if let Ok(span) = TextSpan::new(body, body_start, body_end, format)
                                {
                                    spans.push(span);
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }
    }
    spans
}

/// Finds a body byte range matching the HTML text before `end` and from `start`.
fn align_html_run(
    body: &str,
    html: &str,
    runs: &[(usize, usize)],
    start: usize,
    end: usize,
    body_cursor: &mut usize,
) -> Option<(usize, usize)> {
    let run = html.get(start..end)?;
    if run.is_empty() {
        return None;
    }
    let prefix = html.get(..start)?;
    let before = body.get(..*body_cursor)?;
    if !html_prefix_matches(before, prefix) {
        return None;
    }
    let tail = body.get(*body_cursor..)?;
    let offset = tail.find(run)?;
    let body_start = *body_cursor + offset;
    let body_end = body_start + run.len();
    if !html_suffix_matches(body, body_end, html.get(end..)?) {
        return None;
    }
    *body_cursor = body_start;
    let _ = runs;
    Some((body_start, body_end))
}

/// Whether the plain prefix can correspond to the consumed HTML prefix.
fn html_prefix_matches(body: &str, html: &str) -> bool {
    body.ends_with(html) || html.ends_with(body)
}

/// Whether the plain suffix can correspond to the pending HTML suffix.
fn html_suffix_matches(body: &str, offset: usize, html: &str) -> bool {
    body.get(offset..)
        .is_some_and(|tail| tail.starts_with(html) || html.starts_with(tail))
}

/// Maps a receipt event: one read marker per referenced event id and reader.
fn apply_receipts(base: &NormalizedMessage, content: &Value) -> Vec<NormalizedMessage> {
    let Some(receipts) = content.as_object() else {
        return vec![base.clone()];
    };
    let mut messages = Vec::new();
    let mut receipt_entries = receipts.iter().collect::<Vec<_>>();
    receipt_entries.sort_unstable_by_key(|(event_id, _)| *event_id);
    for (event_id, receipt) in receipt_entries {
        for message in apply_receipt(base, event_id, receipt) {
            messages.push(message);
        }
    }
    if messages.is_empty() {
        messages.push(base.clone());
    }
    messages
}

/// Maps one `content[event_id]` receipt value into one status event per reader.
fn apply_receipt(
    base: &NormalizedMessage,
    event_id: &str,
    receipt: &Value,
) -> Vec<NormalizedMessage> {
    let Some(kinds) = receipt.as_object() else {
        return vec![receipt_status(
            base,
            event_id,
            DeliveryStatus::Unknown("unknown".to_owned()),
            None,
        )];
    };
    if kinds.is_empty() {
        return vec![receipt_status(
            base,
            event_id,
            DeliveryStatus::Unknown("unknown".to_owned()),
            None,
        )];
    }

    let mut messages = Vec::new();
    for (kind, users) in kinds {
        let status = if kind == "m.read" {
            DeliveryStatus::Read
        } else {
            DeliveryStatus::Unknown(kind.clone())
        };
        let users = users.as_object();
        let mut readers = users
            .into_iter()
            .flat_map(|users| users.keys())
            .map(String::as_str)
            .collect::<Vec<_>>();
        readers.sort_unstable();
        if readers.is_empty() {
            messages.push(receipt_status(base, event_id, status, None));
        } else {
            for reader in readers {
                messages.push(receipt_status(base, event_id, status.clone(), Some(reader)));
            }
        }
    }
    messages
}

/// Builds one receipt status event.
fn receipt_status(
    base: &NormalizedMessage,
    event_id: &str,
    status: DeliveryStatus,
    reader: Option<&str>,
) -> NormalizedMessage {
    let mut message = base.clone();
    message.status = Some(status);
    message.status_for = Some(event_id.to_owned());
    if let Some(reader) = reader.filter(|reader| !reader.is_empty()) {
        reader.clone_into(&mut message.sender_id);
    }
    message
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
