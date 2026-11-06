//! The identifiers, limits and small values the outbound client exchanges.
//!
//! Every type here is a newtype over a string or a byte slice because the
//! Matrix grammar is stricter than `String`: a transaction id, an `mxc://` URI
//! and a room id each have a shape, and a value that violates it must fail
//! before a request is built rather than as a `404` from the homeserver.
//!
//! The Matrix identifier grammar is hand-written rather than taken from a
//! maintained crate, because the one crate that owns it (`ruma-common`, via
//! `matrix-sdk`) drags in `as_variant`, whose MPL-2.0 license this project's
//! `deny.toml` does not allow, and no maintained standalone identifier crate
//! exists. The sigils (`!` room, `$` event, `@` user, `#` room alias) are
//! defined once in `IdKind` and every `validate_*_id` shares one code path,
//! so a rule cannot drift per id kind; the spec paragraphs are
//! `appendices.html#room-ids-and-event-ids`, `#user-ids` and `#room-aliases`.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use base64::Engine as _;
use uuid::Uuid;

use crate::matrix_out::error::OutboundError;

/// Longest transaction id the client accepts, in bytes.
///
/// The specification puts no ceiling on a transaction id; this is a local guard
/// so a caller cannot hand us a megabyte of "id" and have it pasted into a URL
/// path. 255 matches the size the specification gives for ids generally.
///
/// ```
/// use messenger_webhook::matrix_out::MAX_TXN_ID_BYTES;
///
/// assert_eq!(MAX_TXN_ID_BYTES, 255);
/// ```
pub const MAX_TXN_ID_BYTES: usize = 255;

/// The serialised `content` ceiling the client checks, in bytes.
///
/// The specification's *Size limits* section: "The complete event MUST NOT be
/// larger than 65536 bytes". The content is a strict part of that event, so a
/// content at or over this bound can never be accepted — the client rejects it
/// before the request exists. The homeserver still enforces the exact bound,
/// because the envelope adds the sender, room, signatures and metadata on top.
///
/// ```
/// use messenger_webhook::matrix_out::MAX_CONTENT_BYTES;
///
/// assert_eq!(MAX_CONTENT_BYTES, 65_536);
/// ```
pub const MAX_CONTENT_BYTES: usize = 65_536;

/// Largest room, user or event id the client accepts, in bytes.
///
/// ```
/// use messenger_webhook::matrix_out::MAX_ID_BYTES;
///
/// assert_eq!(MAX_ID_BYTES, 255);
/// ```
pub const MAX_ID_BYTES: usize = 255;

/// Largest event type the client accepts, in bytes — the specification's rule
/// is "`type` MUST NOT exceed 255 bytes".
///
/// ```
/// use messenger_webhook::matrix_out::MAX_EVENT_TYPE_BYTES;
///
/// assert_eq!(MAX_EVENT_TYPE_BYTES, 255);
/// ```
pub const MAX_EVENT_TYPE_BYTES: usize = 255;

/// Default media ceiling, in bytes.
///
/// The specification has no global upload limit: the homeserver advertises its
/// own as `m.upload.size` in `GET /_matrix/client/v3/capabilities` (and in
/// `GET /_matrix/client/v1/media/config`), and the spec's own example value is
/// `{"m.upload.size": 50000000}`; Synapse's default `max_upload_size` is 50 MB.
/// This constant is that default. It is a *pre-flight* limit — set it to the
/// advertised value with
/// [`ClientConfig::with_max_upload_bytes`](crate::matrix_out::ClientConfig::with_max_upload_bytes)
/// so oversized media fails locally instead of as a `413 M_TOO_LARGE` after the
/// bytes have been read.
///
/// ```
/// use messenger_webhook::matrix_out::DEFAULT_MAX_UPLOAD_BYTES;
///
/// assert_eq!(DEFAULT_MAX_UPLOAD_BYTES, 50_000_000);
/// ```
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 50_000_000;

/// How long one typing notification asks the homeserver to keep the marker up.
///
/// Matrix typing notifications decay on their own; a bot sending a single pulse
/// before composing a reply wants a few seconds, not the protocol maximum.
///
/// ```
/// use messenger_webhook::matrix_out::DEFAULT_TYPING_TIMEOUT_MS;
///
/// assert_eq!(DEFAULT_TYPING_TIMEOUT_MS, 4_000);
/// ```
pub const DEFAULT_TYPING_TIMEOUT_MS: u32 = 4_000;

/// The Megolm v1 messaging algorithm name (`m.megolm.v1.aes-sha2`).
///
/// The Matrix specification's *End-to-End Encryption → Messaging Algorithms*
/// section (v1.19) fixes this exact string as the `algorithm` of a Megolm
/// `m.room.encrypted` room event and of the `m.room.encryption` state event
/// that turns a room encrypted. This crate models that one algorithm, so the
/// name is a constant rather than a free-form string.
///
/// ```
/// use messenger_webhook::matrix_out::MEGOLM_V1_AES_SHA2;
///
/// assert_eq!(MEGOLM_V1_AES_SHA2, "m.megolm.v1.aes-sha2");
/// ```
pub const MEGOLM_V1_AES_SHA2: &str = "m.megolm.v1.aes-sha2";

/// Largest base64 `ciphertext` the client accepts, in bytes.
///
/// The specification puts no ceiling on a single encrypted payload; it caps the
/// *complete event* at 65 536 bytes (`MAX_CONTENT_BYTES`). The other Megolm
/// content fields are bounded by [`MAX_ENCRYPTED_SESSION_ID_BYTES`],
/// [`MAX_ENCRYPTED_DEVICE_ID_BYTES`] and [`MAX_ENCRYPTED_SENDER_KEY_BYTES`], so
/// 60 000 bytes of base64 ciphertext plus that envelope stays under the event
/// ceiling. The value is a local pre-flight guard (the same spirit as
/// [`MAX_TXN_ID_BYTES`]): a caller cannot hand us a megabyte of ciphertext that
/// the homeserver would reject after the write.
///
/// ```
/// use messenger_webhook::matrix_out::MAX_ENCRYPTED_CIPHERTEXT_BYTES;
///
/// assert_eq!(MAX_ENCRYPTED_CIPHERTEXT_BYTES, 60_000);
/// ```
pub const MAX_ENCRYPTED_CIPHERTEXT_BYTES: usize = 60_000;

/// Largest Megolm `session_id` the client accepts, in bytes.
///
/// The specification gives no explicit length for a session id, but the
/// *Appendices → Room IDs and Event IDs* rule reserves 255 bytes for the id
/// fields it does define, and a session id is the same kind of opaque handle.
/// 255 is that general rule applied as a local guard.
///
/// ```
/// use messenger_webhook::matrix_out::MAX_ENCRYPTED_SESSION_ID_BYTES;
///
/// assert_eq!(MAX_ENCRYPTED_SESSION_ID_BYTES, 255);
/// ```
pub const MAX_ENCRYPTED_SESSION_ID_BYTES: usize = 255;

/// Largest `device_id` the client accepts, in bytes.
///
/// Device ids are opaque; this is the specification's general 255-byte id rule
/// applied as a local guard, so a caller cannot past a megabyte of "id" into
/// the event.
///
/// ```
/// use messenger_webhook::matrix_out::MAX_ENCRYPTED_DEVICE_ID_BYTES;
///
/// assert_eq!(MAX_ENCRYPTED_DEVICE_ID_BYTES, 255);
/// ```
pub const MAX_ENCRYPTED_DEVICE_ID_BYTES: usize = 255;

/// Largest base64 `sender_key` the client accepts, in bytes.
///
/// A Curve25519 public key is 32 bytes and so 44 base64 characters in practice;
/// 255 is the general 255-byte id rule applied as a local guard.
///
/// ```
/// use messenger_webhook::matrix_out::MAX_ENCRYPTED_SENDER_KEY_BYTES;
///
/// assert_eq!(MAX_ENCRYPTED_SENDER_KEY_BYTES, 255);
/// ```
pub const MAX_ENCRYPTED_SENDER_KEY_BYTES: usize = 255;

/// The idempotency key of one send.
///
/// Matrix de-duplicates `PUT /rooms/{roomId}/send/{eventType}/{txnId}` requests
/// that carry the same transaction id: "the server should return the same HTTP
/// response code and content as the original request". Two attempts with the
/// same [`TxnId`] therefore produce one event, which is what makes a retry after
/// a `5xx` or a timeout safe — and what makes minting a *new* id on a re-drive a
/// duplicate message.
///
/// The type is a validated newtype: non-empty, at most [`MAX_TXN_ID_BYTES`]
/// and restricted to the URL-unreserved characters `A-Z a-z 0-9 - _ . ~`, so a
/// transaction id survives the path segment it is pasted into unchanged.
///
/// ```
/// use messenger_webhook::matrix_out::TxnId;
///
/// let txn = TxnId::new("order-42")?;
/// assert_eq!(txn.as_str(), "order-42");
/// assert_eq!(txn.to_string(), "order-42");
///
/// // A slash or a space is not a transaction id, it is a broken URL.
/// assert!(TxnId::new("order/42").is_err());
/// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TxnId(String);

/// Process-wide sequence for [`TxnId::generate`], so ids generated by one
/// process are ordered by creation and never repeat.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One random value per process, so two processes starting their sequences at
/// zero still mint different ids.
static PROCESS_NONCE: OnceLock<Uuid> = OnceLock::new();

impl TxnId {
    /// Validates and wraps a transaction id.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidTxnId`] when `raw` is empty, longer than
    /// [`MAX_TXN_ID_BYTES`], or carries a byte outside `A-Z a-z 0-9 - _ . ~`.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::TxnId;
    ///
    /// assert!(TxnId::new("").is_err());
    /// assert!(TxnId::new("a".repeat(256)).is_err());
    /// assert!(TxnId::new("abc.DEF_123~-").is_ok());
    /// ```
    pub fn new(raw: impl AsRef<str> + Into<String>) -> Result<Self, OutboundError> {
        let raw_ref = raw.as_ref();
        if raw_ref.is_empty() {
            return Err(OutboundError::InvalidTxnId {
                reason: "the transaction id is empty".to_owned(),
            });
        }
        if raw_ref.len() > MAX_TXN_ID_BYTES {
            return Err(OutboundError::InvalidTxnId {
                reason: format!(
                    "the transaction id is {} bytes, the ceiling is {MAX_TXN_ID_BYTES}",
                    raw_ref.len()
                ),
            });
        }
        if let Some(bad) = raw_ref
            .bytes()
            .find(|byte| !matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~'))
        {
            return Err(OutboundError::InvalidTxnId {
                reason: format!(
                    "byte {bad:#04x} is not one of A-Z a-z 0-9 - _ . ~ (a transaction id is a URL path segment)"
                ),
            });
        }
        Ok(Self(raw.into()))
    }

    /// Mints a fresh transaction id: a monotonic sequence and a per-process
    /// random nonce, e.g. `mw-000000000000002a-9f2c…`.
    ///
    /// Unique within a process (the sequence never repeats) and across
    /// processes (the nonce is 128 random bits), and ordered by creation, which
    /// is what makes two ids comparable in a log.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::TxnId;
    ///
    /// let first = TxnId::generate();
    /// let second = TxnId::generate();
    /// assert_ne!(first, second);
    /// assert!(first < second);
    /// ```
    #[must_use]
    pub fn generate() -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nonce = PROCESS_NONCE.get_or_init(Uuid::new_v4);
        Self(format!("mw-{sequence:016x}-{nonce}"))
    }

    /// The id as it goes into the URL.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::TxnId;
    ///
    /// assert_eq!(TxnId::new("t-1")?.as_str(), "t-1");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TxnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A Matrix content URI (`mxc://server/mediaId`).
///
/// ```
/// use messenger_webhook::matrix_out::MxcUri;
///
/// let uri = MxcUri::parse("mxc://example.org/AQwafuaFswefuhsfAFAgsw")?;
/// assert_eq!(uri.server_name(), "example.org");
/// assert_eq!(uri.media_id(), "AQwafuaFswefuhsfAFAgsw");
///
/// // A plain https URL is not a Matrix content URI.
/// assert!(MxcUri::parse("https://example.org/cat.png").is_err());
/// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MxcUri {
    /// The URI exactly as it arrived, including the `mxc://` scheme.
    full: String,
    /// The validated server name between `mxc://` and the first `/`.
    server: Box<str>,
    /// The validated media id after the first `/`.
    media: Box<str>,
}

/// Longest `mxc://` URI the client accepts, in bytes.
///
/// The specification gives no ceiling; server names and media ids are short in
/// practice, and bounding the value keeps it out of "arbitrary length string in
/// a JSON document" territory.
///
/// ```
/// use messenger_webhook::matrix_out::MAX_MXC_URI_BYTES;
///
/// assert_eq!(MAX_MXC_URI_BYTES, 512);
/// ```
pub const MAX_MXC_URI_BYTES: usize = 512;

impl MxcUri {
    /// Validates and wraps an `mxc://` URI.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidMxcUri`] when the value is not `mxc://`, has an
    /// empty server name or media id, exceeds [`MAX_MXC_URI_BYTES`], or contains
    /// whitespace or a control character.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MxcUri;
    ///
    /// assert!(MxcUri::parse("mxc://").is_err());
    /// assert!(MxcUri::parse("mxc://example.org/").is_err());
    /// assert!(MxcUri::parse("mxc:///media").is_err());
    /// assert!(MxcUri::parse("mxc://example.org/a b").is_err());
    /// assert!(MxcUri::parse("mxc://example.org/ok").is_ok());
    /// ```
    pub fn parse(raw: impl AsRef<str> + Into<String>) -> Result<Self, OutboundError> {
        let raw_ref = raw.as_ref();
        let invalid = |reason: &str| OutboundError::InvalidMxcUri {
            reason: reason.to_owned(),
        };
        let Some(rest) = raw_ref.strip_prefix("mxc://") else {
            return Err(invalid("a Matrix content URI starts with `mxc://`"));
        };
        if raw_ref.len() > MAX_MXC_URI_BYTES {
            return Err(invalid("the URI is longer than the 512 byte ceiling"));
        }
        if raw_ref.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(invalid(
                "the URI contains whitespace or a control character",
            ));
        }
        let Some((server, media)) = rest.split_once('/') else {
            return Err(invalid(
                "the URI has no `/` between server name and media id",
            ));
        };
        if server.is_empty() {
            return Err(invalid("the server name is empty"));
        }
        if media.is_empty() {
            return Err(invalid("the media id is empty"));
        }
        let server = server.to_owned().into_boxed_str();
        let media = media.to_owned().into_boxed_str();
        Ok(Self {
            full: raw.into(),
            server,
            media,
        })
    }

    /// The URI as the homeserver spells it.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MxcUri;
    ///
    /// assert_eq!(MxcUri::parse("mxc://a/b")?.as_str(), "mxc://a/b");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.full
    }

    /// The server name between `mxc://` and the media id.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MxcUri;
    ///
    /// assert_eq!(MxcUri::parse("mxc://a/b")?.server_name(), "a");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server
    }

    /// The media id after the server name.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MxcUri;
    ///
    /// assert_eq!(MxcUri::parse("mxc://a/b")?.media_id(), "b");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn media_id(&self) -> &str {
        &self.media
    }
}

impl fmt::Display for MxcUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.full)
    }
}

/// Which `m.room.message` msgtype a media upload is sent as.
///
/// The upload endpoint only stores bytes; the *kind* decides the message
/// content (`m.image`, `m.video`, `m.audio`, `m.file`) that references them.
///
/// ```
/// use messenger_webhook::matrix_out::MediaKind;
///
/// assert_eq!(MediaKind::Image.msgtype(), "m.image");
/// assert_eq!(MediaKind::File.msgtype(), "m.file");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum MediaKind {
    /// `m.image`.
    Image,
    /// `m.video`.
    Video,
    /// `m.audio`.
    Audio,
    /// `m.file` — everything that is not one of the above, and the default.
    #[default]
    File,
}

impl MediaKind {
    /// The `msgtype` the referencing message event carries.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MediaKind;
    ///
    /// assert_eq!(MediaKind::Audio.msgtype(), "m.audio");
    /// ```
    #[must_use]
    pub const fn msgtype(self) -> &'static str {
        match self {
            Self::Image => "m.image",
            Self::Video => "m.video",
            Self::Audio => "m.audio",
            Self::File => "m.file",
        }
    }
}

/// One media item to upload, borrowed from the caller.
///
/// ```
/// use messenger_webhook::matrix_out::{MediaKind, MediaUpload};
///
/// let upload = MediaUpload::new(b"\x89PNG", "image/png")
///     .with_filename("cat.png")
///     .with_kind(MediaKind::Image);
///
/// assert_eq!(upload.data.len(), 4);
/// assert_eq!(upload.kind, MediaKind::Image);
/// assert_eq!(upload.filename, Some("cat.png"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaUpload<'a> {
    /// The bytes to upload. Never copied by the client.
    pub data: &'a [u8],
    /// The `Content-Type` the homeserver stores with the media.
    pub content_type: &'a str,
    /// The file name, sent as the `filename` query parameter.
    pub filename: Option<&'a str>,
    /// Which message type references the upload.
    pub kind: MediaKind,
}

impl<'a> MediaUpload<'a> {
    /// A `m.file` upload with no file name.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MediaUpload;
    ///
    /// let upload = MediaUpload::new(b"pdf", "application/pdf");
    /// assert_eq!(upload.filename, None);
    /// ```
    #[must_use]
    pub const fn new(data: &'a [u8], content_type: &'a str) -> Self {
        Self {
            data,
            content_type,
            filename: None,
            kind: MediaKind::File,
        }
    }

    /// Sets the file name.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MediaUpload;
    ///
    /// assert_eq!(MediaUpload::new(b"x", "text/plain").with_filename("a.txt").filename, Some("a.txt"));
    /// ```
    #[must_use]
    pub const fn with_filename(mut self, filename: &'a str) -> Self {
        self.filename = Some(filename);
        self
    }

    /// Sets the message type the upload is sent as.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{MediaKind, MediaUpload};
    ///
    /// assert_eq!(MediaUpload::new(b"x", "image/png").with_kind(MediaKind::Image).kind, MediaKind::Image);
    /// ```
    #[must_use]
    pub const fn with_kind(mut self, kind: MediaKind) -> Self {
        self.kind = kind;
        self
    }
}

/// What a send produced.
///
/// ```
/// use messenger_webhook::matrix_out::{SendOutcome, TxnId};
///
/// let outcome = SendOutcome {
///     event_id: "$abc".into(),
///     txn_id: TxnId::new("t1")?,
///     attempts: 2,
/// };
/// assert_eq!(outcome.event_id, "$abc");
/// assert_eq!(outcome.attempts, 2);
/// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendOutcome {
    /// The event id the homeserver assigned (`$…`).
    pub event_id: String,
    /// The transaction id the send was keyed with. Persist it if the send may
    /// have to be repeated: re-sending with the same id is a no-op, re-sending
    /// with a new one is a second message.
    pub txn_id: TxnId,
    /// How many HTTP attempts the client made, `1` when the first one worked.
    pub attempts: u32,
}

/// What a media send produced once both legs have succeeded.
///
/// The upload URI is returned alongside the event outcome so a later retry can
/// address the send leg directly with
/// [`MatrixClient::send_uploaded_media_with_txn`](crate::matrix_out::MatrixClient::send_uploaded_media_with_txn)
/// instead of uploading the bytes a second time.
///
/// ```
/// use messenger_webhook::matrix_out::{MediaSendOutcome, MxcUri, SendOutcome, TxnId};
///
/// let outcome = MediaSendOutcome {
///     content_uri: MxcUri::parse("mxc://m.example/abc")?,
///     send: SendOutcome {
///         event_id: "$img".into(),
///         txn_id: TxnId::new("media-1")?,
///         attempts: 2,
///     },
/// };
/// assert_eq!(outcome.content_uri.as_str(), "mxc://m.example/abc");
/// assert_eq!(outcome.send.txn_id.as_str(), "media-1");
/// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSendOutcome {
    /// The `mxc://` URI of the uploaded bytes.
    pub content_uri: MxcUri,
    /// The event send that references the uploaded bytes.
    pub send: SendOutcome,
}

/// The content of a Megolm `m.room.encrypted` room event, typed.
///
/// This is the **metadata-only** surface: the client carries the fields a
/// consumer needs to address the event — the algorithm, the opaque ciphertext
/// and the Megolm session identity — and never derives a key or decrypts. The
/// fields and their meaning are the Matrix specification's *End-to-End
/// Encryption → `m.room.encrypted`* section (v1.19): a Megolm room event's
/// content is `{"algorithm", "ciphertext", "device_id", "sender_key",
/// "session_id"}`. The `algorithm` is fixed to [`MEGOLM_V1_AES_SHA2`], the one
/// algorithm this constructor models, so it is an accessor rather than a stored
/// field.
///
/// The type is a validated newtype: the base64 `ciphertext`, `sender_key` and
/// the ids are checked before the value can reach a request, so a malformed
/// payload is a local [`OutboundError::InvalidRequest`] instead of a homeserver
/// `M_BAD_JSON` after the write.
///
/// ```
/// use messenger_webhook::matrix_out::MegolmEncryptedContent;
///
/// let content = MegolmEncryptedContent::new(
///     "AwgAEnB1cnBvc2U",          // ciphertext, base64 (unpadded is fine)
///     "3C5BFWi2Y8MaVvjM8M22DBmh24PmgR0n", // sender_key, base64
///     "JLAFKJWSCS",                // device_id
///     "lEuiRJBit0IG6nUf5pUzWTUEsRVVe/HJToXO1DfK7Jg", // session_id
/// )?;
/// assert_eq!(content.algorithm(), "m.megolm.v1.aes-sha2");
/// assert_eq!(content.device_id(), "JLAFKJWSCS");
/// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MegolmEncryptedContent {
    ciphertext: String,
    sender_key: String,
    device_id: String,
    session_id: String,
}

impl MegolmEncryptedContent {
    /// Validates and wraps the four caller-supplied Megolm fields.
    ///
    /// `algorithm` is not a parameter: this type is the Megolm content, whose
    /// algorithm is [`MEGOLM_V1_AES_SHA2`] by definition. Model a different
    /// algorithm with [`EncryptedEventContent::Other`].
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidRequest`] when `ciphertext` or `sender_key` is
    /// not valid base64, when any field is empty, or when a field is longer than
    /// its ceiling ([`MAX_ENCRYPTED_CIPHERTEXT_BYTES`],
    /// [`MAX_ENCRYPTED_SESSION_ID_BYTES`], [`MAX_ENCRYPTED_DEVICE_ID_BYTES`],
    /// [`MAX_ENCRYPTED_SENDER_KEY_BYTES`]).
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MegolmEncryptedContent;
    ///
    /// assert!(MegolmEncryptedContent::new("AAAA", "AAAA", "DEV", "SESS").is_ok());
    /// // The ciphertext and the sender key are base64, so `!` is not one.
    /// assert!(MegolmEncryptedContent::new("!!!!", "AAAA", "DEV", "SESS").is_err());
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    pub fn new(
        ciphertext: impl AsRef<str> + Into<String>,
        sender_key: impl AsRef<str> + Into<String>,
        device_id: impl AsRef<str> + Into<String>,
        session_id: impl AsRef<str> + Into<String>,
    ) -> Result<Self, OutboundError> {
        validate_base64_field(
            "ciphertext",
            ciphertext.as_ref(),
            MAX_ENCRYPTED_CIPHERTEXT_BYTES,
        )?;
        validate_base64_field(
            "sender_key",
            sender_key.as_ref(),
            MAX_ENCRYPTED_SENDER_KEY_BYTES,
        )?;
        validate_opaque_field(
            "device_id",
            device_id.as_ref(),
            MAX_ENCRYPTED_DEVICE_ID_BYTES,
        )?;
        validate_opaque_field(
            "session_id",
            session_id.as_ref(),
            MAX_ENCRYPTED_SESSION_ID_BYTES,
        )?;
        Ok(Self {
            ciphertext: ciphertext.into(),
            sender_key: sender_key.into(),
            device_id: device_id.into(),
            session_id: session_id.into(),
        })
    }

    /// The algorithm that always accompanies this content.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{MegolmEncryptedContent, MEGOLM_V1_AES_SHA2};
    ///
    /// let content = MegolmEncryptedContent::new("AAAA", "AAAA", "DEV", "SESS")?;
    /// assert_eq!(content.algorithm(), MEGOLM_V1_AES_SHA2);
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub const fn algorithm(&self) -> &'static str {
        MEGOLM_V1_AES_SHA2
    }

    /// The base64 ciphertext, verbatim.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MegolmEncryptedContent;
    ///
    /// let content = MegolmEncryptedContent::new("AwgA", "AAAA", "DEV", "SESS")?;
    /// assert_eq!(content.ciphertext(), "AwgA");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn ciphertext(&self) -> &str {
        &self.ciphertext
    }

    /// The base64 Curve25519 sender key, verbatim.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MegolmEncryptedContent;
    ///
    /// let content = MegolmEncryptedContent::new("AAAA", "3C5B", "DEV", "SESS")?;
    /// assert_eq!(content.sender_key(), "3C5B");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn sender_key(&self) -> &str {
        &self.sender_key
    }

    /// The device id that produced the message.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MegolmEncryptedContent;
    ///
    /// let content = MegolmEncryptedContent::new("AAAA", "AAAA", "JLAFKJWSCS", "SESS")?;
    /// assert_eq!(content.device_id(), "JLAFKJWSCS");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The Megolm session id the sender used.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MegolmEncryptedContent;
    ///
    /// let content = MegolmEncryptedContent::new("AAAA", "AAAA", "DEV", "lEuiRJBit0IG")?;
    /// assert_eq!(content.session_id(), "lEuiRJBit0IG");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The JSON content of an `m.room.encrypted` event carrying these fields.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::MegolmEncryptedContent;
    ///
    /// let content = MegolmEncryptedContent::new("AAAA", "AAAA", "DEV", "SESS")?;
    /// assert_eq!(content.to_value()["algorithm"], "m.megolm.v1.aes-sha2");
    /// assert_eq!(content.to_value()["session_id"], "SESS");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "algorithm": MEGOLM_V1_AES_SHA2,
            "ciphertext": self.ciphertext,
            "sender_key": self.sender_key,
            "device_id": self.device_id,
            "session_id": self.session_id,
        })
    }
}

/// The `content` of an `m.room.encrypted` room event this client can send.
///
/// The Matrix specification's *End-to-End Encryption* section says an
/// `m.room.encrypted` event carries an `algorithm` that names how the payload
/// was encrypted and its algorithm-specific `content`. This crate models the
/// one Megolm shape as typed data ([`MegolmEncryptedContent`]) and keeps the
/// door open for any other algorithm as [`EncryptedEventContent::Other`], so an
/// event this crate does not model is still carried, tagged with its algorithm,
/// instead of being dropped. No variant decrypts; the client carries the
/// provider metadata a consumer needs.
///
/// ```
/// use messenger_webhook::matrix_out::{EncryptedEventContent, MegolmEncryptedContent};
///
/// let content =
///     EncryptedEventContent::Megolm(MegolmEncryptedContent::new("AAAA", "AAAA", "DEV", "SESS")?);
/// assert_eq!(content.to_value()?["algorithm"], "m.megolm.v1.aes-sha2");
/// assert_eq!(content.to_value()?["session_id"], "SESS");
///
/// let custom = EncryptedEventContent::Other {
///     algorithm: "org.example.cipher".to_owned(),
///     fields: serde_json::json!({ "blob": "…" }),
/// };
/// assert_eq!(custom.to_value()?["algorithm"], "org.example.cipher");
/// assert_eq!(custom.to_value()?["blob"], "…");
/// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EncryptedEventContent {
    /// A Megolm (`m.megolm.v1.aes-sha2`) room event.
    Megolm(MegolmEncryptedContent),
    /// Any other algorithm: the algorithm name plus the algorithm-specific
    /// fields, carried verbatim. The event `content` is flat — the spec puts
    /// `algorithm` beside the algorithm's own keys, not around them — so
    /// `fields` must be a JSON object and is merged next to `algorithm`.
    /// `algorithm` must be a non-empty, control-free name no longer than
    /// [`MAX_EVENT_TYPE_BYTES`]; both are validated by
    /// [`to_value`](Self::to_value) before the wire.
    Other {
        /// The `algorithm` field, e.g. `org.example.cipher`.
        algorithm: String,
        /// The algorithm-specific content fields, merged beside `algorithm`.
        fields: serde_json::Value,
    },
}

impl From<MegolmEncryptedContent> for EncryptedEventContent {
    /// Wraps Megolm content as the [`EncryptedEventContent::Megolm`] arm, so a
    /// Megolm send reads `send_encrypted(room, content.into())`.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{EncryptedEventContent, MegolmEncryptedContent};
    ///
    /// let content = MegolmEncryptedContent::new("AAAA", "AAAA", "DEV", "SESS")?;
    /// let event: EncryptedEventContent = content.into();
    /// assert_eq!(event.to_value()?["session_id"], "SESS");
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    fn from(content: MegolmEncryptedContent) -> Self {
        Self::Megolm(content)
    }
}

impl EncryptedEventContent {
    /// The outbound `content` object for an `m.room.encrypted` event.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidRequest`] when an [`EncryptedEventContent::Other`]
    /// carries an empty or over-long algorithm name, or a `content` that is not
    /// a JSON object. [`EncryptedEventContent::Megolm`] was already validated
    /// when it was built.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::EncryptedEventContent;
    ///
    /// let content = EncryptedEventContent::Other {
    ///     algorithm: "org.example.cipher".to_owned(),
    ///     fields: serde_json::json!({ "blob": "x" }),
    /// };
    /// assert_eq!(content.to_value()?["algorithm"], "org.example.cipher");
    /// assert_eq!(content.to_value()?["blob"], "x");
    ///
    /// let not_an_object = EncryptedEventContent::Other {
    ///     algorithm: "org.example.cipher".to_owned(),
    ///     fields: serde_json::json!("just a string"),
    /// };
    /// assert!(not_an_object.to_value().is_err());
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    pub fn to_value(&self) -> Result<serde_json::Value, OutboundError> {
        match self {
            Self::Megolm(content) => Ok(content.to_value()),
            Self::Other { algorithm, fields } => {
                if algorithm.is_empty() {
                    return Err(OutboundError::InvalidRequest {
                        reason: "the m.room.encrypted algorithm is empty".to_owned(),
                    });
                }
                if algorithm.len() > MAX_EVENT_TYPE_BYTES {
                    return Err(OutboundError::InvalidRequest {
                        reason: format!(
                            "the m.room.encrypted algorithm is {} bytes, the ceiling is {MAX_EVENT_TYPE_BYTES}",
                            algorithm.len()
                        ),
                    });
                }
                if algorithm.chars().any(char::is_control) {
                    return Err(OutboundError::InvalidRequest {
                        reason: "the m.room.encrypted algorithm contains a control character"
                            .to_owned(),
                    });
                }
                let Some(object) = fields.as_object() else {
                    return Err(OutboundError::InvalidRequest {
                        reason: "the m.room.encrypted content fields are not a JSON object"
                            .to_owned(),
                    });
                };
                // Flat per the spec: `algorithm` sits beside the algorithm's
                // own keys, and the caller's `algorithm` key (if any) cannot
                // override the validated one.
                let mut merged = object.clone();
                merged.insert(
                    "algorithm".to_owned(),
                    serde_json::Value::String(algorithm.clone()),
                );
                Ok(serde_json::Value::Object(merged))
            }
        }
    }
}

/// The sigil that introduces each Matrix id this client addresses.
///
/// Matrix gives every identifier a leading sigil so a room, user and event id
/// are distinguishable from one another and from an id of the wrong kind. The
/// spec (`appendices.html#room-ids-and-event-ids`, `#user-ids`,
/// `#room-aliases`) fixes them as: `!` room id, `$` event id, `@` user id and
/// `#` room alias.
#[derive(Clone, Copy)]
enum IdKind {
    Room,
    User,
    Event,
}

impl IdKind {
    /// The sigil the identifier MUST start with, and the noun used in errors.
    const fn sigil(self) -> char {
        match self {
            Self::Room => '!',
            Self::User => '@',
            Self::Event => '$',
        }
    }

    const fn noun(self) -> &'static str {
        match self {
            Self::Room => "room id",
            Self::User => "user id",
            Self::Event => "event id",
        }
    }
}

/// The single place that decides whether a Matrix id has the right shape.
///
/// Every `validate_*_id` funnels through here, so the sigil, the byte ceiling
/// and the whitespace/control guard live in exactly one place and cannot drift
/// apart per id kind.
fn validate_id(kind: IdKind, value: &str) -> Result<(), OutboundError> {
    let noun = kind.noun();
    if value.is_empty() {
        return Err(OutboundError::InvalidRequest {
            reason: format!("the {noun} is empty"),
        });
    }
    let sigil = kind.sigil();
    if !value.starts_with(sigil) {
        let hint = if sigil == '!' {
            " (resolve a `#alias` first)"
        } else {
            ""
        };
        return Err(OutboundError::InvalidRequest {
            reason: format!("Matrix {noun}s start with `{sigil}`, got {value:?}{hint}"),
        });
    }
    if value.len() > MAX_ID_BYTES {
        return Err(OutboundError::InvalidRequest {
            reason: format!(
                "the {noun} is {} bytes, the ceiling is {MAX_ID_BYTES}",
                value.len()
            ),
        });
    }
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(OutboundError::InvalidRequest {
            reason: format!("the {noun} contains whitespace or a control character"),
        });
    }
    Ok(())
}

/// Rejects a room id the send endpoints cannot address.
pub(crate) fn validate_room_id(room_id: &str) -> Result<(), OutboundError> {
    validate_id(IdKind::Room, room_id)
}

/// Rejects a user id the typing endpoint cannot address.
pub(crate) fn validate_user_id(user_id: &str) -> Result<(), OutboundError> {
    validate_id(IdKind::User, user_id)
}

/// Rejects an event id used as a relation target.
pub(crate) fn validate_event_id(event_id: &str) -> Result<(), OutboundError> {
    validate_id(IdKind::Event, event_id)
}

/// Rejects an event type the send endpoint cannot address.
pub(crate) fn validate_event_type(event_type: &str) -> Result<(), OutboundError> {
    if event_type.is_empty() {
        return Err(OutboundError::InvalidRequest {
            reason: "the event type is empty".to_owned(),
        });
    }
    if event_type.len() > MAX_EVENT_TYPE_BYTES {
        return Err(OutboundError::InvalidRequest {
            reason: format!(
                "the event type is {} bytes, the ceiling is {MAX_EVENT_TYPE_BYTES}",
                event_type.len()
            ),
        });
    }
    if event_type.contains('/') {
        return Err(OutboundError::InvalidRequest {
            reason: format!(
                "the event type {event_type:?} contains `/`, which is a path separator"
            ),
        });
    }
    if event_type.chars().any(char::is_control) {
        return Err(OutboundError::InvalidRequest {
            reason: "the event type contains a control character".to_owned(),
        });
    }
    Ok(())
}

/// Validates one base64 field of a Megolm `m.room.encrypted` content.
///
/// The Matrix specification's *End-to-End Encryption → `m.room.encrypted`*
/// section: the `ciphertext` is "base64-encoded", and so is the Curve25519
/// `sender_key`. This accepts the canonical padded form and the unpadded form
/// the spec also shows in its (unpadded) `sender_key` example, and rejects
/// anything else before the value reaches a request.
fn validate_base64_field(name: &str, value: &str, ceiling: usize) -> Result<(), OutboundError> {
    let invalid = |reason: String| OutboundError::InvalidRequest {
        reason: format!("the m.room.encrypted {name} {reason}"),
    };
    if value.is_empty() {
        return Err(invalid("is empty".to_owned()));
    }
    if value.len() > ceiling {
        return Err(invalid(format!(
            "is {} bytes, the ceiling is {ceiling}",
            value.len()
        )));
    }
    // The canonical and the unpadded alphabet are the same ASCII set; padding
    // mode is what differs, and `STANDARD_PAD_INDIFFERENT` accepts both.
    let engine = &base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;
    if engine.decode(value.as_bytes()).is_err() {
        return Err(invalid("is not base64".to_owned()));
    }
    Ok(())
}

/// Validates one opaque, non-base64 id field of a Megolm `m.room.encrypted`
/// content (`device_id`, `session_id`).
fn validate_opaque_field(name: &str, value: &str, ceiling: usize) -> Result<(), OutboundError> {
    let invalid = |reason: String| OutboundError::InvalidRequest {
        reason: format!("the m.room.encrypted {name} {reason}"),
    };
    if value.is_empty() {
        return Err(invalid("is empty".to_owned()));
    }
    if value.len() > ceiling {
        return Err(invalid(format!(
            "is {} bytes, the ceiling is {ceiling}",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid("contains a control character".to_owned()));
    }
    Ok(())
}

/// Rejects a media upload the client will not be able to send.
pub(crate) fn validate_media(media: &MediaUpload<'_>, limit: u64) -> Result<(), OutboundError> {
    let size = u64::try_from(media.data.len()).unwrap_or(u64::MAX);
    if size > limit {
        return Err(OutboundError::MediaTooLarge {
            limit,
            actual: size,
        });
    }
    if media.data.is_empty() {
        return Err(OutboundError::InvalidRequest {
            reason: "the upload is empty".to_owned(),
        });
    }
    validate_content_type(media.content_type)?;
    if let Some(filename) = media.filename {
        validate_filename(filename)?;
    }
    Ok(())
}

/// A MIME type goes into a header: no CR/LF (header injection), no spaces.
fn validate_content_type(content_type: &str) -> Result<(), OutboundError> {
    let invalid = |reason: &str| OutboundError::InvalidRequest {
        reason: format!("the upload content type is unusable: {reason}"),
    };
    if content_type.is_empty() {
        return Err(invalid("empty"));
    }
    if content_type.len() > 255 {
        return Err(invalid("longer than 255 bytes"));
    }
    if content_type
        .chars()
        .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(invalid("it contains whitespace or a control character"));
    }
    let Some((ty, subtype)) = content_type.split_once('/') else {
        return Err(invalid("not a `type/subtype` pair"));
    };
    if ty.is_empty() || subtype.is_empty() || subtype.contains('/') {
        return Err(invalid("not a `type/subtype` pair"));
    }
    if !is_http_token(ty) || !is_http_token(subtype) {
        return Err(invalid("not a `type/subtype` pair"));
    }
    Ok(())
}

/// RFC 7230's `token` production.
fn is_http_token(value: &str) -> bool {
    value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
            )
    })
}

/// A file name goes into the query string: separators and controls are out.
fn validate_filename(filename: &str) -> Result<(), OutboundError> {
    let invalid = |reason: &str| OutboundError::InvalidRequest {
        reason: format!("the upload file name is unusable: {reason}"),
    };
    if filename.is_empty() {
        return Err(invalid("empty"));
    }
    if filename.len() > 255 {
        return Err(invalid("longer than 255 bytes"));
    }
    if filename
        .chars()
        .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(invalid(
            "it contains a path separator or a control character",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use proptest::prelude::*;

    use super::*;

    struct TrackedInput {
        value: String,
        conversions: Rc<Cell<usize>>,
    }

    impl TrackedInput {
        fn new(value: String, conversions: Rc<Cell<usize>>) -> Self {
            Self { value, conversions }
        }
    }

    impl AsRef<str> for TrackedInput {
        fn as_ref(&self) -> &str {
            &self.value
        }
    }

    impl From<TrackedInput> for String {
        fn from(input: TrackedInput) -> Self {
            input.conversions.set(input.conversions.get() + 1);
            input.value
        }
    }

    #[test]
    fn txn_ids_accept_the_unreserved_alphabet_only() {
        for accepted in ["a", "T-1", "mw.000_111~x", &"z".repeat(MAX_TXN_ID_BYTES)] {
            assert!(TxnId::new(accepted).is_ok(), "{accepted:?} should be valid");
        }
        for rejected in ["", " ", "a b", "a/b", "a%20b", "ok\n", "ü"] {
            assert!(
                TxnId::new(rejected).is_err(),
                "{rejected:?} should be rejected"
            );
        }
        let too_long = "x".repeat(MAX_TXN_ID_BYTES + 1);
        let error = TxnId::new(too_long).expect_err("over the ceiling");
        assert!(error.to_string().contains("255"), "{error}");
    }

    #[test]
    fn txn_id_rejection_names_the_offending_byte() {
        let error = TxnId::new("a/b").expect_err("slash");
        assert!(error.to_string().contains("0x2f"), "{error}");
    }

    #[test]
    fn ceiling_rejections_do_not_materialise_the_caller_string() {
        let txn_conversions = Rc::new(Cell::new(0));
        let over_txn = TrackedInput::new(
            "x".repeat(MAX_TXN_ID_BYTES + 1),
            Rc::clone(&txn_conversions),
        );
        assert!(TxnId::new(over_txn).is_err());
        assert_eq!(txn_conversions.get(), 0);

        let uri_conversions = Rc::new(Cell::new(0));
        let over_uri = TrackedInput::new(
            format!("mxc://example.org/{}", "a".repeat(MAX_MXC_URI_BYTES)),
            Rc::clone(&uri_conversions),
        );
        assert!(MxcUri::parse(over_uri).is_err());
        assert_eq!(uri_conversions.get(), 0);
    }

    #[test]
    fn accepted_ids_are_owned_after_validation() {
        let txn_conversions = Rc::new(Cell::new(0));
        let exact_txn =
            TrackedInput::new("t".repeat(MAX_TXN_ID_BYTES), Rc::clone(&txn_conversions));
        let txn = TxnId::new(exact_txn).expect("exactly at the ceiling");
        assert_eq!(txn.as_str().len(), MAX_TXN_ID_BYTES);
        assert_eq!(txn_conversions.get(), 1);

        let uri_conversions = Rc::new(Cell::new(0));
        let prefix = "mxc://a/";
        let exact_uri = TrackedInput::new(
            format!("{prefix}{}", "a".repeat(MAX_MXC_URI_BYTES - prefix.len())),
            Rc::clone(&uri_conversions),
        );
        let uri = MxcUri::parse(exact_uri).expect("exactly at the ceiling");
        assert_eq!(uri.as_str().len(), MAX_MXC_URI_BYTES);
        assert_eq!(uri_conversions.get(), 1);
    }

    #[test]
    fn generated_txn_ids_are_unique_ordered_and_valid() {
        let ids: Vec<TxnId> = (0..500).map(|_| TxnId::generate()).collect();
        for id in &ids {
            assert!(TxnId::new(id.as_str()).is_ok(), "{id} is not a valid id");
            assert!(id.as_str().starts_with("mw-"));
        }
        for pair in ids.windows(2) {
            assert!(pair[0] < pair[1], "{} !< {}", pair[0], pair[1]);
        }
        let unique: std::collections::BTreeSet<&str> = ids.iter().map(TxnId::as_str).collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn mxc_uris_split_into_server_and_media_id() {
        let uri = MxcUri::parse("mxc://matrix.org/AbC123").expect("valid");
        assert_eq!(uri.server_name(), "matrix.org");
        assert_eq!(uri.media_id(), "AbC123");
        assert_eq!(uri.to_string(), "mxc://matrix.org/AbC123");
        // A server name with a port and a media id with `_` stay intact.
        let uri = MxcUri::parse("mxc://localhost:8008/a_b").expect("valid");
        assert_eq!(uri.server_name(), "localhost:8008");
        assert_eq!(uri.media_id(), "a_b");

        // A prefix-like server name must not make the stored split disagree
        // with the accessors: the server is `mxc:` and the media id is `/x`.
        let uri = MxcUri::parse("mxc://mxc://x").expect("valid");
        assert_eq!(uri.server_name(), "mxc:");
        assert_eq!(uri.media_id(), "/x");
    }

    #[test]
    fn mxc_uri_rejections_all_name_a_reason() {
        for bad in [
            "",
            "mxc://",
            "mxc://example.org",
            "mxc://example.org/",
            "mxc:///abc",
            "mxc://exa mple/abc",
            "mxc://example.org/a\tb",
            "https://example.org/abc",
        ] {
            let error = MxcUri::parse(bad).expect_err(bad);
            assert!(
                error.to_string().contains("Matrix content URI")
                    || error.to_string().contains("URI"),
                "{error}"
            );
        }
        let long = format!("mxc://example.org/{}", "a".repeat(MAX_MXC_URI_BYTES));
        assert!(MxcUri::parse(long).is_err());
    }

    #[test]
    fn id_validators_reject_the_four_broad_shapes() {
        assert!(validate_room_id("!r:example.org").is_ok());
        assert!(validate_room_id("").is_err());
        assert!(validate_room_id("#alias:example.org").is_err());
        assert!(validate_room_id("!r with space:example.org").is_err());
        assert!(validate_room_id(&format!("!{}:x", "a".repeat(MAX_ID_BYTES))).is_err());

        assert!(validate_user_id("@bot:example.org").is_ok());
        assert!(validate_user_id("bot:example.org").is_err());
        assert!(validate_user_id("").is_err());

        assert!(validate_event_id("$abc").is_ok());
        assert!(validate_event_id("").is_err());
        assert!(validate_event_id("!room:example.org").is_err());
        assert!(validate_event_id("https://example.org/x").is_err());
        assert!(validate_event_id("$a\nb").is_err());
        let max_event_id = format!("${}", "a".repeat(MAX_ID_BYTES - 1));
        assert!(validate_event_id(&max_event_id).is_ok());
        assert!(validate_event_id(&format!("{max_event_id}a")).is_err());

        assert!(validate_event_type("m.room.message").is_ok());
        assert!(validate_event_type("com.example.custom").is_ok());
        assert!(validate_event_type("").is_err());
        assert!(validate_event_type("m.room/message").is_err());
        assert!(validate_event_type("bad\n").is_err());
        assert!(validate_event_type(&"t".repeat(MAX_EVENT_TYPE_BYTES + 1)).is_err());
    }

    #[test]
    fn every_id_kind_rejects_the_other_two_sigils() {
        // The regression that motivated the shared helper: `validate_event_id`
        // used to accept a `!`-prefixed room id because the `$` sigil was
        // missing. Pin that each kind rejects the wrong sigil, so no future
        // divergence can let one id kind masquerade as another.
        let wrong_for_room = ["@bot:example.org", "$abc", "#alias:example.org"];
        for value in wrong_for_room {
            let error = validate_room_id(value).expect_err(value);
            assert!(error.to_string().contains("start with `!`"), "{error}");
        }
        assert!(validate_room_id("#alias:example.org")
            .expect_err("an alias")
            .to_string()
            .contains("resolve a `#alias`"));

        let wrong_for_user = ["!room:example.org", "$abc", "bot:example.org"];
        for value in wrong_for_user {
            let error = validate_user_id(value).expect_err(value);
            assert!(error.to_string().contains("start with `@`"), "{error}");
        }

        let wrong_for_event = ["!room:example.org", "@bot:example.org", "https://x"];
        for value in wrong_for_event {
            let error = validate_event_id(value).expect_err(value);
            assert!(error.to_string().contains("start with `$`"), "{error}");
        }
    }

    #[test]
    fn media_validation_covers_size_type_and_name() {
        let bytes = [0u8; 8];
        assert!(validate_media(&MediaUpload::new(&bytes, "image/png"), 8).is_ok());
        let too_large = validate_media(&MediaUpload::new(&bytes, "image/png"), 7)
            .expect_err("8 bytes over a 7 byte limit");
        assert!(matches!(
            too_large,
            OutboundError::MediaTooLarge {
                limit: 7,
                actual: 8
            }
        ));

        assert!(validate_media(&MediaUpload::new(&[], "image/png"), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, ""), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, "png"), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, "/"), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, "a/"), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, "/b"), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, "a/b/c"), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, "image//png"), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, "image/p?ng"), 8).is_err());
        assert!(validate_media(&MediaUpload::new(&bytes, "image/png\r\nX: y"), 8).is_err());

        let content_type_at_limit = format!("{}/{}", "a".repeat(127), "b".repeat(127));
        assert_eq!(content_type_at_limit.len(), 255);
        assert!(validate_media(&MediaUpload::new(&bytes, &content_type_at_limit), 8).is_ok());

        let content_type_over_limit = format!("{}/{}", "a".repeat(128), "b".repeat(127));
        assert_eq!(content_type_over_limit.len(), 256);
        assert!(validate_media(&MediaUpload::new(&bytes, &content_type_over_limit), 8).is_err());
        assert!(
            validate_media(&MediaUpload::new(&bytes, "image/png").with_filename(""), 8).is_err()
        );
        assert!(validate_media(
            &MediaUpload::new(&bytes, "image/png").with_filename("a/b"),
            8
        )
        .is_err());
        assert!(validate_media(
            &MediaUpload::new(&bytes, "image/png").with_filename("a\\b"),
            8
        )
        .is_err());
        assert!(validate_media(
            &MediaUpload::new(&bytes, "image/png").with_filename("a\u{7}b"),
            8
        )
        .is_err());
        assert!(validate_media(
            &MediaUpload::new(&bytes, "image/png").with_filename("Bild München.png"),
            8
        )
        .is_ok());
    }

    proptest! {
        #[test]
        fn content_type_validation_matches_the_type_subtype_token_rule(raw in "\\PC{0,300}") {
            let expected = !raw.is_empty()
                && raw.len() <= 255
                && !raw.chars().any(|c| c.is_control() || c.is_whitespace())
                && raw.split_once('/').is_some_and(|(ty, subtype)| {
                    !ty.is_empty()
                        && !subtype.is_empty()
                        && !subtype.contains('/')
                        && is_http_token(ty)
                        && is_http_token(subtype)
                });

            prop_assert_eq!(
                validate_content_type(raw.as_str()).is_ok(),
                expected,
                "for {:?}",
                raw
            );
        }
    }

    #[test]
    fn media_kind_defaults_to_file_and_maps_every_msgtype() {
        assert_eq!(MediaKind::default(), MediaKind::File);
        assert_eq!(MediaKind::Image.msgtype(), "m.image");
        assert_eq!(MediaKind::Video.msgtype(), "m.video");
        assert_eq!(MediaKind::Audio.msgtype(), "m.audio");
        assert_eq!(MediaKind::File.msgtype(), "m.file");
    }

    #[test]
    fn megolm_content_exposes_every_field_and_marshals_the_spec_shape() {
        let content =
            MegolmEncryptedContent::new("AwgA", "3C5B", "DEV", "SESS").expect("valid content");
        assert_eq!(content.algorithm(), MEGOLM_V1_AES_SHA2);
        assert_eq!(content.ciphertext(), "AwgA");
        assert_eq!(content.sender_key(), "3C5B");
        assert_eq!(content.device_id(), "DEV");
        assert_eq!(content.session_id(), "SESS");

        let value = content.to_value();
        assert_eq!(
            value,
            serde_json::json!({
                "algorithm": "m.megolm.v1.aes-sha2",
                "ciphertext": "AwgA",
                "sender_key": "3C5B",
                "device_id": "DEV",
                "session_id": "SESS",
            })
        );

        let event: EncryptedEventContent = content.into();
        assert_eq!(event.to_value().expect("object"), value);
    }

    #[test]
    fn megolm_content_accepts_padded_and_unpadded_base64() {
        // The spec's own `ciphertext` and `sender_key` examples are unpadded,
        // but the canonical alphabet is padded; both must pass.
        assert!(MegolmEncryptedContent::new("AwgQ", "AAAA", "DEV", "SESS").is_ok());
        assert!(MegolmEncryptedContent::new("Awg=", "AAAA", "DEV", "SESS").is_ok());
        assert!(MegolmEncryptedContent::new("Awg", "AAAA", "DEV", "SESS").is_ok());
    }

    #[test]
    fn opaque_encrypted_fields_reject_control_characters() {
        assert!(MegolmEncryptedContent::new("AAAA", "AAAA", "DE\u{0}V", "SESS").is_err());
        assert!(MegolmEncryptedContent::new("AAAA", "AAAA", "DEV", "SE\nSS").is_err());
    }

    #[test]
    fn other_encrypted_algorithm_requires_an_object_and_bounds_the_name() {
        let too_long = EncryptedEventContent::Other {
            algorithm: "a".repeat(MAX_EVENT_TYPE_BYTES + 1),
            fields: serde_json::json!({}),
        };
        assert!(too_long.to_value().is_err());

        let exact = EncryptedEventContent::Other {
            algorithm: "a".repeat(MAX_EVENT_TYPE_BYTES),
            fields: serde_json::json!({}),
        };
        assert!(exact.to_value().is_ok());
    }
}
