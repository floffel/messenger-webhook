//! The seam between the typed Matrix client and an HTTP stack.
//!
//! The client never opens a socket. It builds a [`Request`], hands it to a
//! [`Transport`] and interprets the [`Response`] — so the protocol logic (paths,
//! transaction ids, retry classes) is testable without a homeserver, and a
//! consumer can plug in its own HTTP stack, proxy or recorded fixture.
//! [`ReqwestTransport`](crate::matrix_out::ReqwestTransport) is the shipped
//! implementation; [`StubTransport`](crate::matrix_out::StubTransport) is a
//! scripted double for tests.
//!
//! ```text
//! MatrixClient ──Request──▶ Transport ──▶ homeserver
//!              ◀─Response──          ◀──
//! ```
//!
//! A `Transport` implementation owns exactly one job: perform the request and
//! report what came back. It must not retry, must not add headers and must not
//! interpret the status code — the client does all three, because the retry
//! class depends on the operation (see [`crate::matrix_out::Operation`]).

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

/// A boxed, `Send` future — the return type of [`Transport::send`].
///
/// Boxing keeps [`Transport`] object-safe, so a client can hold
/// `Arc<dyn Transport>` and stay a nameable type instead of growing a generic
/// parameter that infects every caller.
///
/// ```
/// use messenger_webhook::matrix_out::transport::BoxFuture;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let future: BoxFuture<'static, u8> = Box::pin(async { 7 });
/// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// assert_eq!(runtime.block_on(future), 7);
/// # Ok(())
/// # }
/// ```
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The HTTP methods the Client-Server API legs use.
///
/// ```
/// use messenger_webhook::matrix_out::transport::Method;
///
/// assert_eq!(Method::Put.as_str(), "PUT");
/// assert_eq!(Method::Get.as_str(), "GET");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Method {
    /// `GET` — reads (`/account/whoami`).
    Get,
    /// `PUT` — the idempotent writes: a send keyed by a transaction id, a
    /// receipt, a typing notification.
    Put,
    /// `POST` — the un-keyed writes: media upload, a read receipt on some
    /// homeserver versions.
    Post,
}

impl Method {
    /// The wire spelling, upper case as HTTP requires.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Method;
    ///
    /// assert_eq!(Method::Post.as_str(), "POST");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Post => "POST",
        }
    }

    /// Whether the method is defined as repeatable by HTTP semantics.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Method;
    ///
    /// assert!(Method::Get.is_safe());
    /// assert!(!Method::Post.is_safe());
    /// ```
    #[must_use]
    pub const fn is_safe(self) -> bool {
        matches!(self, Self::Get)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The body of a [`Request`].
///
/// The variant says how the bytes must be framed, not which header to send: the
/// content type travels in [`Request::headers`] so a transport applies one
/// uniform header list.
///
/// ```
/// use messenger_webhook::matrix_out::transport::Body;
///
/// let body = Body::Bytes(b"png");
/// assert_eq!(body.len(), 3);
/// assert!(Body::Empty.is_empty());
/// ```
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Body<'a> {
    /// No body at all — a `GET`.
    Empty,
    /// A serialised JSON object, already valid UTF-8 JSON.
    Json(Vec<u8>),
    /// Raw bytes that are not JSON (a media upload).
    Bytes(&'a [u8]),
}

impl Body<'_> {
    /// Number of bytes the transport has to write.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Body;
    ///
    /// assert_eq!(Body::Json(b"{}".to_vec()).len(), 2);
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Json(bytes) => bytes.len(),
            Self::Bytes(bytes) => bytes.len(),
        }
    }

    /// Whether there is nothing to write.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Body;
    ///
    /// assert!(Body::Empty.is_empty());
    /// assert!(!Body::Json(b"{}".to_vec()).is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes to write, empty for [`Body::Empty`].
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Body;
    ///
    /// assert_eq!(Body::Bytes(b"hi").as_bytes(), b"hi");
    /// ```
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Json(bytes) => bytes,
            Self::Bytes(bytes) => bytes,
        }
    }
}

/// [`Debug`] reports lengths, never content: a body carries message text and
/// user media, and this type ends up in `tracing` spans.
impl fmt::Debug for Body<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Json(bytes) => write!(f, "Json({} bytes)", bytes.len()),
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
        }
    }
}

/// One request for a [`Transport`] to perform, built by the client.
///
/// ```
/// use messenger_webhook::matrix_out::transport::{Body, Method, Request};
///
/// let request = Request::new(
///     Method::Put,
///     "https://matrix.example.org/_matrix/client/v3/account/whoami",
/// )
/// .with_header("authorization", "Bearer token")
/// .with_body(Body::Empty);
///
/// assert_eq!(request.method(), Method::Put);
/// assert_eq!(request.header("authorization"), Some("Bearer token"));
/// assert!(request.body().is_empty());
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Request<'a> {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Body<'a>,
}

impl<'a> Request<'a> {
    /// Builds a request with no headers and no body.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Get, "https://example.org/x");
    /// assert_eq!(request.url(), "https://example.org/x");
    /// assert!(request.headers().is_empty());
    /// ```
    #[must_use]
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    /// Appends a header, keeping insertion order.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Get, "https://example.org/x")
    ///     .with_header("accept", "application/json");
    /// assert_eq!(request.headers().len(), 1);
    /// ```
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Attaches the body.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{Body, Method, Request};
    ///
    /// let request =
    ///     Request::new(Method::Put, "https://example.org/x").with_body(Body::Json(b"{}".to_vec()));
    /// assert_eq!(request.body().len(), 2);
    /// ```
    #[must_use]
    pub fn with_body(mut self, body: Body<'a>) -> Self {
        self.body = body;
        self
    }

    /// The HTTP method.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{Method, Request};
    ///
    /// assert_eq!(Request::new(Method::Post, "u").method(), Method::Post);
    /// ```
    #[must_use]
    pub const fn method(&self) -> Method {
        self.method
    }

    /// The absolute URL, already percent-encoded where it needs to be.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Get, "https://example.org/a%21b");
    /// assert!(request.url().ends_with("a%21b"));
    /// ```
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Every header, in insertion order.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Get, "u").with_header("accept", "*/*");
    /// assert_eq!(request.headers()[0].0, "accept");
    /// ```
    #[must_use]
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// The first value of `name`, compared case-insensitively.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Get, "u").with_header("Accept", "*/*");
    /// assert_eq!(request.header("accept"), Some("*/*"));
    /// ```
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{Method, Request};
    ///
    /// assert!(Request::new(Method::Get, "u").body().is_empty());
    /// ```
    #[must_use]
    pub const fn body(&self) -> &Body<'a> {
        &self.body
    }
}

/// [`Debug`] redacts the credential: the authorisation header carries the
/// access token, and a request type is exactly what ends up in a log line.
impl fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                if name.eq_ignore_ascii_case("authorization") {
                    (name.as_str(), "<redacted>")
                } else {
                    (name.as_str(), value.as_str())
                }
            })
            .collect();
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body", &self.body)
            .finish()
    }
}

/// What the homeserver answered, with the body still unparsed.
///
/// ```
/// use messenger_webhook::matrix_out::transport::Response;
///
/// let response = Response::json(200, r#"{"event_id":"$1"}"#);
/// assert_eq!(response.status(), 200);
/// assert_eq!(response.body(), br#"{"event_id":"$1"}"#);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Response {
    /// The HTTP status code.
    pub status: u16,
    /// Response headers as received.
    pub headers: Vec<(String, String)>,
    /// The raw response body.
    pub body: Vec<u8>,
}

impl Response {
    /// Builds a response from a status and a body.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Response;
    ///
    /// let response = Response::new(404, b"{}".to_vec());
    /// assert!(!response.is_success());
    /// ```
    #[must_use]
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }

    /// Builds a JSON response with the `content-type` header set — the shape a
    /// scripted transport queues.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Response;
    ///
    /// let response = Response::json(429, r#"{"errcode":"M_LIMIT_EXCEEDED"}"#);
    /// assert_eq!(response.header("content-type"), Some("application/json"));
    /// ```
    #[must_use]
    pub fn json(status: u16, body: &str) -> Self {
        Self::new(status, body.as_bytes().to_vec()).with_header("content-type", "application/json")
    }

    /// Appends a header.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Response;
    ///
    /// let response = Response::new(429, Vec::new()).with_header("retry-after", "2");
    /// assert_eq!(response.header("retry-after"), Some("2"));
    /// ```
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The status code.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Response;
    ///
    /// assert_eq!(Response::new(204, Vec::new()).status(), 204);
    /// ```
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// The raw body.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Response;
    ///
    /// assert_eq!(Response::new(200, b"ok".to_vec()).body(), b"ok");
    /// ```
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Whether the status is `2xx`.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Response;
    ///
    /// assert!(Response::new(200, Vec::new()).is_success());
    /// assert!(!Response::new(500, Vec::new()).is_success());
    /// ```
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// The first value of `name`, compared case-insensitively.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Response;
    ///
    /// let response = Response::new(200, Vec::new()).with_header("Retry-After", "1");
    /// assert_eq!(response.header("retry-after"), Some("1"));
    /// ```
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Performs one HTTP request.
///
/// Implementations are `Send + Sync`, hold no per-request state and are shared
/// behind an `Arc` by the client. They must be honest about *when* a request
/// could have reached the homeserver: the retry class of a failure depends on
/// it, so a connection error and a read timeout are different
/// [`TransportErrorKind`]s.
///
/// ```
/// use messenger_webhook::matrix_out::transport::{
///     BoxFuture, Request, Response, Transport, TransportError,
/// };
///
/// /// Answers every request with the same status.
/// #[derive(Debug)]
/// struct Fixed(u16);
///
/// impl Transport for Fixed {
///     fn send<'a>(
///         &'a self,
///         _request: Request<'a>,
///     ) -> BoxFuture<'a, Result<Response, TransportError>> {
///         let status = self.0;
///         Box::pin(async move { Ok(Response::json(status, "{}")) })
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let transport = Fixed(200);
/// let request = Request::new(
///     messenger_webhook::matrix_out::transport::Method::Get,
///     "https://example.org",
/// );
/// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// let response = runtime.block_on(transport.send(request))?;
/// assert_eq!(response.status(), 200);
/// # Ok(())
/// # }
/// ```
pub trait Transport: Send + Sync + fmt::Debug {
    /// Performs `request` and returns the unparsed response.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when no HTTP response was produced at all. A response
    /// with a `4xx`/`5xx` status is **not** an error here: it is a response, and
    /// the client classifies it.
    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Response, TransportError>>;
}

/// Where a request died, which decides whether it may be repeated.
///
/// ```
/// use messenger_webhook::matrix_out::transport::TransportErrorKind;
///
/// assert_eq!(TransportErrorKind::Connect.as_str(), "connect");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransportErrorKind {
    /// The connection was never established: DNS, TCP or TLS failed. Nothing
    /// was written, so even a non-idempotent request is safe to repeat.
    Connect,
    /// The request was written but no response arrived in time. The homeserver
    /// may have applied it.
    Timeout,
    /// The connection broke mid-flight, or the transport could not express the
    /// request. Treat as "may have been applied".
    Other,
}

impl TransportErrorKind {
    /// Stable lower-case name, used in error messages.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::TransportErrorKind;
    ///
    /// assert_eq!(TransportErrorKind::Timeout.as_str(), "timeout");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Other => "other",
        }
    }

    /// Whether the request provably never left the process.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::TransportErrorKind;
    ///
    /// assert!(TransportErrorKind::Connect.is_before_write());
    /// assert!(!TransportErrorKind::Timeout.is_before_write());
    /// ```
    #[must_use]
    pub const fn is_before_write(self) -> bool {
        matches!(self, Self::Connect)
    }
}

/// A transport-level failure: no HTTP response was produced.
///
/// ```
/// use messenger_webhook::matrix_out::transport::TransportError;
///
/// let error = TransportError::connect("dns failure");
/// assert!(error.kind.is_before_write());
/// assert_eq!(error.to_string(), "transport failure (connect): dns failure");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("transport failure ({kind}): {message}", kind = .kind.as_str())]
pub struct TransportError {
    /// Where the request died.
    pub kind: TransportErrorKind,
    /// The transport's own message, verbatim.
    pub message: String,
}

impl TransportError {
    /// A failure before anything was written.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::TransportError;
    ///
    /// assert_eq!(TransportError::connect("dns").kind.as_str(), "connect");
    /// ```
    #[must_use]
    pub fn connect(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Connect,
            message: message.into(),
        }
    }

    /// A read timeout after the request was written.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::TransportError;
    ///
    /// assert!(!TransportError::timeout("30s").kind.is_before_write());
    /// ```
    #[must_use]
    pub fn timeout(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Timeout,
            message: message.into(),
        }
    }

    /// Anything else — assumed to be "may have been applied".
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::TransportError;
    ///
    /// assert_eq!(TransportError::other("reset").kind.as_str(), "other");
    /// ```
    #[must_use]
    pub fn other(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Other,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_round_trip_their_wire_spelling() {
        for (method, text, safe) in [
            (Method::Get, "GET", true),
            (Method::Put, "PUT", false),
            (Method::Post, "POST", false),
        ] {
            assert_eq!(method.as_str(), text);
            assert_eq!(method.to_string(), text);
            assert_eq!(method.is_safe(), safe);
        }
    }

    #[test]
    fn body_lengths_and_emptiness_agree() {
        assert_eq!(Body::Empty.len(), 0);
        assert!(Body::Empty.is_empty());
        assert_eq!(Body::Json(vec![1, 2, 3]).len(), 3);
        assert_eq!(Body::Bytes(b"abcd").len(), 4);
        assert_eq!(Body::Bytes(b"abcd").as_bytes(), b"abcd");
    }

    #[test]
    fn debug_redacts_the_authorization_header_and_the_body() {
        let request = Request::new(Method::Put, "https://example.org/send")
            .with_header("authorization", "Bearer syt_secret")
            .with_header("content-type", "application/json")
            .with_body(Body::Json(br#"{"body":"hallo"}"#.to_vec()));
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("syt_secret"), "{rendered}");
        assert!(
            rendered.contains("(\"authorization\", \"<redacted>\")"),
            "{rendered}"
        );
        assert!(!rendered.contains("hallo"), "{rendered}");
        assert!(rendered.contains("Json(16 bytes)"), "{rendered}");
        // Non-credential headers stay readable, so a debug log is still useful.
        assert!(rendered.contains("application/json"), "{rendered}");
    }

    #[test]
    fn body_debug_names_the_variant_without_content() {
        assert_eq!(format!("{:?}", Body::Empty), "Empty");
        assert_eq!(format!("{:?}", Body::Bytes(b"secret")), "Bytes(6 bytes)");
    }

    #[test]
    fn header_lookup_is_case_insensitive_and_takes_the_first() {
        let request = Request::new(Method::Get, "u")
            .with_header("Accept", "application/json")
            .with_header("accept", "text/plain");
        assert_eq!(request.header("accept"), Some("application/json"));
        assert_eq!(request.header("ACCEPT"), Some("application/json"));
        assert_eq!(request.header("missing"), None);
    }

    #[test]
    fn response_accessors_and_success_window() {
        let response = Response::json(204, "").with_header("retry-after", "3");
        assert!(response.is_success());
        assert_eq!(response.status(), 204);
        assert_eq!(response.body().len(), 0);
        assert_eq!(response.header("Retry-After"), Some("3"));
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert!(!Response::new(199, Vec::new()).is_success());
        assert!(!Response::new(300, Vec::new()).is_success());
        assert_eq!(Response::default().status(), 0);
    }

    #[test]
    fn transport_error_kinds_classify_the_write_boundary() {
        for (error, kind, before) in [
            (TransportError::connect("c"), "connect", true),
            (TransportError::timeout("t"), "timeout", false),
            (TransportError::other("o"), "other", false),
        ] {
            assert_eq!(error.kind.as_str(), kind);
            assert_eq!(error.kind.is_before_write(), before);
            assert_eq!(error.kind, error.kind.clone());
            assert!(error.to_string().contains(kind));
            assert!(error.to_string().contains(&error.message));
        }
    }

    #[test]
    fn boxed_futures_are_send() {
        fn assert_send<T: Send>(_: T) {}
        let future: BoxFuture<'static, u8> = Box::pin(async { 1 });
        assert_send(future);
    }
}
