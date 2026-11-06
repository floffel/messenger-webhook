//! Normalise messenger webhooks into one message type — and encode them back.
//!
//! Every provider reports the same facts in a different shape. This crate maps
//! `WhatsApp` Cloud API, Telegram Bot API and Matrix events onto
//! [`NormalizedMessage`], so a consumer has one code path for routing,
//! persistence and threading — and one place where provider quirks live:
//!
//! | provider | inbound | signature | outbound |
//! |----------|---------|-----------|----------|
//! | `WhatsAppAdapter` | `entry[].changes[].value.messages` and `statuses` | HMAC-SHA256 (`X-Hub-Signature-256`) | text, media link |
//! | `TelegramAdapter` | `message`, edits, posts, reactions | `X-Telegram-Bot-Api-Secret-Token` | `sendMessage` / media |
//! | `MatrixAdapter` | `m.room.message`, reactions, receipts, unknown types | `Authorization: Bearer <token>` | `m.room.message` |
//!
//! Each provider adapter lives in its own feature-gated module (`whatsapp`,
//! `telegram`, `matrix`); everything in this module is available regardless of
//! the feature set.
//!
//! One feature is different: `matrix-out` (default **off**) adds the
//! `matrix_out` module, a typed Matrix Client-Server *outbound client* — sends,
//! reactions, redactions, typing, receipts and media upload — on a transport
//! seam so the crate can be tested without a homeserver. It is the only module
//! that performs I/O: with `matrix-out` enabled, it is the only I/O path in the
//! crate, and the caller still supplies the transport it wants.
//!
//! Verification is part of the adapter contract, not an afterthought: an
//! adapter cannot be constructed without a secret, and [`Adapter::verify`]
//! fails closed on an empty secret, a missing header or an oversized body.
//!
//! The size check cannot protect the step that happens before it: the caller
//! has to avoid materialising an unbounded body in the first place. For that
//! step, [`read_body`] reads at most [`MAX_BODY_READ_BYTES`] bytes and returns
//! [`Error::BodyTooLarge`] instead of growing without bound.
//!
//! A `WhatsApp` or Messenger endpoint also receives one request that is not an
//! event at all: Meta's subscription handshake.
//! [`subscription::meta_subscription_challenge`] answers it — the token
//! compared in constant time, the challenge echoed back and nothing else.
//!
//! ```
//! use messenger_webhook::{signature::meta_signature, Headers, Secret};
//!
//! // What a provider-side test harness does, or what a mock server returns.
//! let secret = Secret::new("app-secret-app-secret")?;
//! let body = br#"{"entry":[]}"#;
//! let mut headers = Headers::new();
//! headers.insert("X-Hub-Signature-256", meta_signature(&secret, body)?)?;
//!
//! assert_eq!(headers.get("x-hub-signature-256").map(str::len), Some(71));
//! # Ok::<(), messenger_webhook::Error>(())
//! ```
//!
//! The adapters are constructed with their secret and used through
//! [`Adapter`]: `WhatsAppAdapter::new(app_secret)`, `TelegramAdapter::new(
//! secret_token)`, `MatrixAdapter::new(hs_token)`.
//!
//! `external_id` is the provider's message id and therefore the right
//! idempotency key. An event the provider sent without one is not dropped: it
//! arrives with a stable synthesised id (`SYNTHETIC_ID_PREFIX`, the digest of
//! the raw object), and `NormalizedMessage::has_synthetic_id` says which kind a
//! given event carries. In the default build nothing in this crate performs I/O:
//! the caller owns the HTTP server, retries and rate limits. With `matrix-out`
//! enabled, `matrix_out` is the only I/O path, and the transport it uses is
//! supplied by the caller.
//!
//! ## Deliberate exclusions
//!
//! Documented rather than silently missing — see the README for the full list:
//! no network in the default build, no media download, no Matrix end-to-end
//! encryption (`m.room.encrypted` events are
//! carried verbatim), and Meta's deprecated `X-Hub-Signature` (SHA-1) header is
//! rejected in favour of `X-Hub-Signature-256`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod error;
pub mod headers;
pub mod registry;
pub mod rich;
pub mod secret;
pub mod signature;

#[cfg(feature = "whatsapp")]
#[cfg_attr(docsrs, doc(cfg(feature = "whatsapp")))]
pub mod subscription;

#[cfg(feature = "matrix")]
#[cfg_attr(docsrs, doc(cfg(feature = "matrix")))]
pub mod matrix;
#[cfg(feature = "matrix-out")]
#[cfg_attr(docsrs, doc(cfg(feature = "matrix-out")))]
pub mod matrix_out;
#[cfg(feature = "telegram")]
#[cfg_attr(docsrs, doc(cfg(feature = "telegram")))]
pub mod telegram;
#[cfg(feature = "whatsapp")]
#[cfg_attr(docsrs, doc(cfg(feature = "whatsapp")))]
pub mod whatsapp;

#[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
mod id;
#[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
mod json;
mod model;

#[cfg(feature = "matrix")]
#[cfg_attr(docsrs, doc(cfg(feature = "matrix")))]
pub use matrix::MatrixAdapter;
#[cfg(feature = "telegram")]
#[cfg_attr(docsrs, doc(cfg(feature = "telegram")))]
pub use telegram::TelegramAdapter;
#[cfg(feature = "whatsapp")]
#[cfg_attr(docsrs, doc(cfg(feature = "whatsapp")))]
pub use whatsapp::WhatsAppAdapter;

pub use error::{Error, LimitUnit, Result};
pub use headers::Headers;
pub use model::SYNTHETIC_ID_PREFIX;
pub use model::{
    Attachment, AttachmentKind, Channel, ChannelName, DeliveryStatus, IdempotencyKey,
    NormalizedMessage, OutboundMessage, ProviderMethod, ProviderPayload, Reaction, RetryPolicy,
    MAX_CHANNEL_NAME_BYTES, MAX_IDEMPOTENCY_KEY_BYTES,
};
pub use registry::Registry;
pub use rich::{SpanError, TextFormat, TextSpan};
pub use secret::Secret;

#[cfg(feature = "whatsapp")]
#[cfg_attr(docsrs, doc(cfg(feature = "whatsapp")))]
pub use subscription::meta_subscription_challenge;

/// Hard ceiling on a request body, enforced before any hashing or JSON parsing.
///
/// Providers cap webhook bodies well below this; the point is to bound the work
/// an unauthenticated caller can make the process do. The crate's adapters and
/// [`Registry`] reject a body above this ceiling before dispatch, hashing or
/// JSON parsing.
///
/// ```
/// use messenger_webhook::MAX_BODY_BYTES;
///
/// assert_eq!(MAX_BODY_BYTES, 1024 * 1024);
/// ```
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Hard ceiling on an outbound recipient id, in bytes.
///
/// Every provider accepts conversation ids far shorter than this. The cap
/// bounds the id an outbound message can be addressed to, so a caller cannot
/// turn a `1 MiB` string into a provider request body.
///
/// ```
/// use messenger_webhook::MAX_RECIPIENT_BYTES;
///
/// assert_eq!(MAX_RECIPIENT_BYTES, 256);
/// ```
pub const MAX_RECIPIENT_BYTES: usize = 256;

/// The largest body read [`read_body`] will buffer before failing.
///
/// This is [`MAX_BODY_BYTES`] plus one byte: the extra byte is enough to prove
/// that a stream exceeded the real ceiling without reading the whole stream.
///
/// ```
/// use messenger_webhook::{MAX_BODY_BYTES, MAX_BODY_READ_BYTES};
///
/// assert_eq!(MAX_BODY_READ_BYTES, MAX_BODY_BYTES + 1);
/// ```
pub const MAX_BODY_READ_BYTES: usize = MAX_BODY_BYTES + 1;

/// Reads a request body, failing before an unbounded stream is buffered.
///
/// Webhook signatures are computed over the raw body, so the bytes have to
/// survive intact. This helper keeps that requirement while bounding an
/// unauthenticated caller's memory and read work: it stops after
/// [`MAX_BODY_READ_BYTES`] bytes and returns [`Error::BodyTooLarge`] rather than
/// reading further.
///
/// The caller still owns transport-level limits such as rejecting a
/// `Content-Length` header that is already over [`MAX_BODY_BYTES`]; this helper
/// protects the read itself when a header is missing or inaccurate.
///
/// # Errors
///
/// - [`Error::BodyTooLarge`] when the stream supplies more than
///   [`MAX_BODY_BYTES`] bytes. At most [`MAX_BODY_READ_BYTES`] bytes are
///   buffered before the failure.
/// - [`Error::Io`] when the underlying reader returns an error other than an
///   interrupted read.
///
/// ```
/// use messenger_webhook::{read_body, Error, MAX_BODY_BYTES};
///
/// let body = read_body(&b"{}"[..])?;
/// assert_eq!(body, b"{}");
///
/// let oversized = vec![b' '; MAX_BODY_BYTES + 1];
/// assert!(matches!(
///     read_body(&oversized[..]),
///     Err(Error::BodyTooLarge { .. })
/// ));
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
pub fn read_body(mut reader: impl std::io::Read) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let remaining = MAX_BODY_READ_BYTES.saturating_sub(body.len());
        if remaining == 0 {
            return Err(Error::BodyTooLarge {
                limit: MAX_BODY_BYTES,
                actual: body.len(),
            });
        }

        let read_len = remaining.min(buffer.len());
        match reader.read(&mut buffer[..read_len]) {
            Ok(0) => return Ok(body),
            Ok(read) => {
                body.extend_from_slice(&buffer[..read]);
                if body.len() > MAX_BODY_BYTES {
                    return Err(Error::BodyTooLarge {
                        limit: MAX_BODY_BYTES,
                        actual: body.len(),
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(Error::Io(error)),
        }
    }
}

/// Turns provider payloads into normalised messages.
///
/// Implementations are `Send + Sync + Debug` (with redacted secrets), hold no
/// interior mutability and are safe to share across tasks by reference.
///
/// ```
/// use messenger_webhook::{
///     Adapter, Channel, Error, Headers, NormalizedMessage, OutboundMessage, ProviderMethod,
///     ProviderPayload, Result,
/// };
///
/// /// An adapter for a provider this crate does not ship.
/// #[derive(Debug)]
/// struct Echo;
///
/// impl Adapter for Echo {
///     fn channel(&self) -> Channel {
///         Channel::other("echo").expect("echo is a valid channel")
///     }
///
///     fn verify(&self, _headers: &Headers, body: &[u8]) -> Result<()> {
///         if body.is_empty() {
///             return Err(Error::Payload("empty body".into()));
///         }
///         Ok(())
///     }
///
///     fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
///         let text = String::from_utf8_lossy(payload).into_owned();
///         Ok(vec![NormalizedMessage::inbound_text(
///             Channel::other("echo").expect("echo is a valid channel"),
///             "event-1",
///             "self",
///             text,
///         )])
///     }
///
///     fn encode(&self, message: &OutboundMessage) -> Result<ProviderPayload> {
///         Ok(ProviderPayload::new(
///             Channel::other("echo").expect("echo is a valid channel"),
///             ProviderMethod::Post,
///             "application/json",
///             serde_json::json!({ "text": message.text.clone().unwrap_or_default() })
///                 .to_string()
///                 .into_bytes(),
///         )?)
///     }
/// }
///
/// let events = Echo.adapt(b"ping")?;
/// assert_eq!(events[0].text(), Some("ping"));
///
/// let payload = Echo.encode(&OutboundMessage::text(
///     Channel::other("echo").expect("echo is a valid channel"),
///     "self",
///     "ping",
/// ))?;
/// assert_eq!(payload.json()?["text"], "ping");
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
pub trait Adapter: Send + Sync + std::fmt::Debug {
    /// Which channel this adapter speaks.
    fn channel(&self) -> Channel;

    /// Verifies that `body` really came from the provider.
    ///
    /// # Errors
    ///
    /// - [`Error::BodyTooLarge`] if `body` exceeds [`MAX_BODY_BYTES`], checked
    ///   before any hashing.
    /// - [`Error::MissingSecret`] if the adapter was built without a secret.
    /// - [`Error::MissingVerificationHeader`] if the provider header is absent.
    /// - [`Error::MalformedVerificationHeader`] if the header does not have the
    ///   provider's documented shape.
    /// - [`Error::BadSignature`] if the signature does not match. The
    ///   comparison is constant time.
    ///
    /// The size check happens here, after the body has already been read. A
    /// server should cap the read itself with [`read_body`] (or an equivalent
    /// `Read::take(MAX_BODY_READ_BYTES)` guard) before calling this method.
    fn verify(&self, headers: &Headers, body: &[u8]) -> Result<()>;

    /// Parses `body` and returns every event it contains.
    ///
    /// Unknown fields and unknown event types are carried, not rejected. An
    /// event the provider sent without an id is carried too, with a stable
    /// synthesised `external_id` ([`SYNTHETIC_ID_PREFIX`]) — the same body
    /// always yields the same key, so a provider retry still dedupes. Only an
    /// entry that carries none of the provider's own fields — a body forwarded
    /// from another provider — is skipped rather than turned into an invented
    /// event.
    ///
    /// The [`MAX_BODY_BYTES`] ceiling is unconditional and is checked before
    /// JSON parsing. [`Registry`] enforces it before dispatching to a custom
    /// adapter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`] when the body is not valid JSON,
    /// [`Error::Payload`] when it is valid JSON but not the provider's shape,
    /// and [`Error::BodyTooLarge`] for oversized bodies.
    fn adapt(&self, payload: &[u8]) -> Result<Vec<NormalizedMessage>>;

    /// Encodes `message` into the provider's outbound schema.
    ///
    /// Provider limits are enforced here, so an invalid payload is never
    /// produced: `WhatsApp` text is capped at 4096 bytes, Telegram text at 4096
    /// UTF-16 units and Telegram captions at 1024 UTF-16 units. Matrix has no
    /// documented body limit.
    ///
    /// The returned [`ProviderPayload`] also states whether the caller may
    /// retry the request after a `5xx` or timeout. A payload marked
    /// [`RetryPolicy::UnsafeUnlessKeyPresent`] must not be repeated blindly:
    /// doing so can duplicate a message at the provider. When the provider
    /// supports idempotency keys, [`ProviderPayload::idempotency_key`] carries
    /// the key that makes a retry repeat rather than duplicate.
    ///
    /// # Errors
    ///
    /// - [`Error::MessageTooLong`] when the text exceeds the provider limit.
    /// - [`Error::InvalidMessage`] when the provider cannot express the message
    ///   (empty recipient, no text and no attachment, unsupported media kind,
    ///   non-numeric Telegram id, non-`mxc://` Matrix url).
    ///
    /// ```
    /// use messenger_webhook::{
    ///     Adapter, Channel, Error, Headers, NormalizedMessage, OutboundMessage, ProviderMethod,
    ///     ProviderPayload, Result,
    /// };
    ///
    /// #[derive(Debug)]
    /// struct Upper;
    ///
    /// impl Adapter for Upper {
    ///     fn channel(&self) -> Channel { Channel::other("upper").expect("upper is valid") }
    ///     fn verify(&self, _: &Headers, _: &[u8]) -> Result<()> { Ok(()) }
    ///     fn adapt(&self, _: &[u8]) -> Result<Vec<NormalizedMessage>> { Ok(Vec::new()) }
    ///     fn encode(&self, message: &OutboundMessage) -> Result<ProviderPayload> {
    ///         let text = message.text.clone().unwrap_or_default().to_uppercase();
    ///         ProviderPayload::new(
    ///             self.channel(),
    ///             ProviderMethod::Post,
    ///             "application/json",
    ///             serde_json::json!({ "text": text }).to_string().into_bytes(),
    ///         )
    ///     }
    /// }
    ///
    /// let message = OutboundMessage::text(
    ///     Channel::other("upper").expect("upper is valid"),
    ///     "1",
    ///     "hi",
    /// );
    /// assert_eq!(Upper.encode(&message)?.json()?["text"], "HI");
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    fn encode(&self, message: &OutboundMessage) -> Result<ProviderPayload>;
}

/// Length of `text` in UTF-16 code units — the unit Telegram counts.
#[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Serialises `body` into a [`ProviderPayload`].
#[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
pub(crate) fn json_payload(
    channel: Channel,
    method: ProviderMethod,
    body: &serde_json::Value,
) -> Result<ProviderPayload> {
    payload(
        channel,
        method,
        body,
        RetryPolicy::UnsafeUnlessKeyPresent,
        None,
    )
}

/// Serialises `body` into a [`ProviderPayload`] with a retry contract.
#[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
pub(crate) fn payload(
    channel: Channel,
    method: ProviderMethod,
    body: &serde_json::Value,
    retry: RetryPolicy,
    idempotency_key: Option<IdempotencyKey>,
) -> Result<ProviderPayload> {
    let encoded = serde_json::to_vec(body)?;
    ProviderPayload::with_retry(
        channel,
        method,
        "application/json",
        encoded,
        retry,
        idempotency_key,
    )
}

/// The recipient a message is addressed to, validated per channel.
///
/// The rules are shared so every adapter inherits them:
///
/// - the trimmed id must not be empty, longer than [`MAX_RECIPIENT_BYTES`]
///   bytes, or carry a character that is not printable;
/// - `WhatsApp` takes E.164 digits only, Telegram a signed integer chat id and
///   Matrix a room id that starts with `!`.
#[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
pub(crate) fn recipient(channel: Channel, message: &crate::OutboundMessage) -> Result<&str> {
    let id = message.conversation_id.trim();
    if id.is_empty() {
        return Err(invalid_recipient(channel, "conversation_id is empty"));
    }
    if id.len() > MAX_RECIPIENT_BYTES {
        return Err(invalid_recipient(
            channel,
            format!(
                "conversation_id is {} bytes, the ceiling is {MAX_RECIPIENT_BYTES}",
                id.len()
            ),
        ));
    }
    if id.chars().any(|c| !is_graphic(c)) {
        return Err(invalid_recipient(
            channel,
            "conversation_id contains a non-printable character",
        ));
    }
    match channel {
        Channel::WhatsApp => {
            if !id.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid_recipient(
                    channel,
                    format!("WhatsApp recipients are E.164 digits, got {id:?}"),
                ));
            }
        }
        Channel::Telegram => {
            if id.parse::<i64>().is_err() {
                return Err(invalid_recipient(
                    channel,
                    format!("Telegram chat ids are integers, got {id:?}"),
                ));
            }
        }
        Channel::Matrix => {
            if !id.starts_with('!') {
                return Err(invalid_recipient(
                    channel,
                    format!("Matrix room ids start with '!', got {id:?}"),
                ));
            }
        }
        Channel::Messenger | Channel::Other(_) => {}
    }
    Ok(id)
}

/// Whether `character` is one a reader would see, as opposed to a control,
/// format, surrogate, private-use, unassigned or separator code point.
///
/// Separators and controls cover whitespace, so a space, tab or newline is not
/// printable by this definition.
#[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
fn is_graphic(character: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory};

    !matches!(
        get_general_category(character),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::SpaceSeparator
    )
}

/// Builds the shared [`Error::InvalidMessage`] for a rejected recipient.
#[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
fn invalid_recipient(channel: Channel, reason: impl Into<String>) -> Error {
    Error::InvalidMessage {
        channel,
        reason: reason.into(),
    }
}

/// Rejects bodies above the crate-wide ceiling.
pub(crate) fn check_body(actual: usize) -> Result<()> {
    if actual > MAX_BODY_BYTES {
        Err(Error::BodyTooLarge {
            limit: MAX_BODY_BYTES,
            actual,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{check_body, Error, MAX_BODY_BYTES};

    #[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
    use super::{is_graphic, recipient, utf16_len, Channel, OutboundMessage, MAX_RECIPIENT_BYTES};

    #[test]
    fn check_body_accepts_at_limit_and_rejects_above_it() {
        assert!(check_body(0).is_ok());
        assert!(check_body(MAX_BODY_BYTES - 1).is_ok());
        assert!(check_body(MAX_BODY_BYTES).is_ok());
        assert!(matches!(
            check_body(MAX_BODY_BYTES + 1),
            Err(Error::BodyTooLarge {
                limit,
                actual,
            }) if limit == MAX_BODY_BYTES && actual == MAX_BODY_BYTES + 1
        ));
    }

    proptest::proptest! {
        #[test]
        fn check_body_accepts_exactly_values_within_the_ceiling(actual in 0usize..=(MAX_BODY_BYTES + 2)) {
            proptest::prop_assert_eq!(check_body(actual).is_ok(), actual <= MAX_BODY_BYTES);
        }
    }

    #[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
    #[test]
    fn utf16_len_counts_ascii_bmp_and_astral_units() {
        assert_eq!(utf16_len(""), 0);
        assert_eq!(utf16_len("ascii"), 5);
        assert_eq!(utf16_len("a\u{e4}\u{20ac}"), 3);
        assert_eq!(utf16_len("a\u{1f600}b"), 4);
    }

    #[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
    #[test]
    fn recipient_trims_echoes_and_enforces_the_byte_ceiling() {
        let channel = Channel::other("custom").expect("custom channel name");
        let message = OutboundMessage::text(channel.clone(), "  abc  ", "hi");
        assert_eq!(recipient(channel.clone(), &message).ok(), Some("abc"));

        let message = OutboundMessage::text(channel.clone(), " \t\n ", "hi");
        assert!(recipient(channel.clone(), &message).is_err());

        let just_inside = "a".repeat(MAX_RECIPIENT_BYTES - 1);
        let at_limit = "a".repeat(MAX_RECIPIENT_BYTES);
        let over_limit = "a".repeat(MAX_RECIPIENT_BYTES + 1);

        let message = OutboundMessage::text(channel.clone(), just_inside.as_str(), "hi");
        assert_eq!(
            recipient(channel.clone(), &message).ok(),
            Some(just_inside.as_str())
        );

        let message = OutboundMessage::text(channel.clone(), at_limit.as_str(), "hi");
        assert_eq!(
            recipient(channel.clone(), &message).ok(),
            Some(at_limit.as_str())
        );

        let message = OutboundMessage::text(channel.clone(), over_limit.as_str(), "hi");
        assert!(recipient(channel, &message).is_err());
    }

    #[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
    proptest::proptest! {
        #[test]
        fn utf16_len_matches_rust_utf16_encoding(text in proptest::prelude::any::<String>()) {
            proptest::prop_assert_eq!(utf16_len(&text), text.encode_utf16().count());
        }

        #[test]
        fn recipient_accepts_exactly_non_empty_graphic_trimmed_ids(
            id in proptest::prelude::any::<String>(),
        ) {
            let channel = Channel::other("custom").expect("custom channel name");
            let message = OutboundMessage::text(channel.clone(), id.as_str(), "hi");
            let trimmed = id.trim();
            let expected = if trimmed.is_empty()
                || trimmed.len() > MAX_RECIPIENT_BYTES
                || trimmed.chars().any(|character| !is_graphic(character))
            {
                None
            } else {
                Some(trimmed)
            };

            match recipient(channel, &message) {
                Ok(actual) => proptest::prop_assert_eq!(expected, Some(actual)),
                Err(Error::InvalidMessage { .. }) => proptest::prop_assert_eq!(expected, None),
                Err(other) => proptest::prop_assert!(false, "unexpected error: {other:?}"),
            }
        }
    }
}
