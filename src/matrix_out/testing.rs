//! Test doubles: a scripted transport and a sleeper that records instead of
//! waiting.
//!
//! Both are public API because a consumer testing its own code against this
//! client needs exactly the same seam the crate's tests use — asserting the URL,
//! the transaction id and the number of attempts without a homeserver and
//! without a wall clock.
//!
//! ```
//! use std::sync::Arc;
//! use messenger_webhook::matrix_out::transport::Transport;
//! use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Script a 429 with a 2 s hint, then a success: one send, two attempts.
//! let transport = Arc::new(StubTransport::new());
//! transport
//!     .push_json(429, r#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":2000}"#)
//!     .push_json(200, r#"{"event_id":"$1"}"#);
//!
//! let client = MatrixClient::new(
//!     transport.clone(),
//!     ClientConfig::new("https://matrix.example.org")?,
//!     "syt_token",
//! )?;
//!
//! let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
//! let outcome = runtime.block_on(client.send_text("!room:example.org", "Hallo"))?;
//! assert_eq!(outcome.attempts, 2);
//! // The retry reused the transaction id, so the homeserver sees one event.
//! assert_eq!(transport.requests()[0].url, transport.requests()[1].url);
//! # Ok(())
//! # }
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::matrix_out::transport::{
    Body, BoxFuture, Method, Request, Response, Transport, TransportError,
};
use crate::matrix_out::Sleeper;

/// One request the stub received.
///
/// [`Debug`] redacts the authorisation header, so a failing assertion can print
/// the recorded calls without printing the access token.
///
/// ```
/// use messenger_webhook::matrix_out::testing::RecordedRequest;
///
/// let request = RecordedRequest {
///     method: messenger_webhook::matrix_out::transport::Method::Put,
///     url: "https://example.org/x".into(),
///     headers: vec![("authorization".into(), "Bearer secret".into())],
///     body: Vec::new(),
/// };
/// assert!(!format!("{request:?}").contains("secret"));
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// The HTTP method the client used.
    pub method: Method,
    /// The absolute URL, after percent-encoding.
    pub url: String,
    /// Every header, in the order the client set them.
    pub headers: Vec<(String, String)>,
    /// The body bytes as they were handed to the transport.
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// The first value of `name`, compared case-insensitively.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::testing::RecordedRequest;
    ///
    /// let request = RecordedRequest {
    ///     method: messenger_webhook::matrix_out::transport::Method::Get,
    ///     url: "u".into(),
    ///     headers: vec![("accept".into(), "application/json".into())],
    ///     body: Vec::new(),
    /// };
    /// assert_eq!(request.header("Accept"), Some("application/json"));
    /// ```
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body parsed as JSON.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the body is empty or not JSON.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::testing::RecordedRequest;
    ///
    /// let request = RecordedRequest {
    ///     method: messenger_webhook::matrix_out::transport::Method::Put,
    ///     url: "u".into(),
    ///     headers: Vec::new(),
    ///     body: br#"{"msgtype":"m.text"}"#.to_vec(),
    /// };
    /// assert_eq!(request.json()?["msgtype"], "m.text");
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    pub fn json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }
}

impl fmt::Debug for RecordedRequest {
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
        f.debug_struct("RecordedRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body", &format_args!("{} bytes", self.body.len()))
            .finish()
    }
}

/// What the stub does for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// Answer with this response.
    Response(Response),
    /// Fail the way a transport fails.
    Failure(TransportError),
}

/// A [`Transport`] that replays a scripted queue and records what it was asked
/// to do.
///
/// Interior mutability throughout, so one stub can be shared by 32 tasks behind
/// an `Arc` — which is how the crate's concurrency test drives one client.
///
/// ```
/// use messenger_webhook::matrix_out::StubTransport;
///
/// let stub = StubTransport::new();
/// stub.push_json(200, r#"{"event_id":"$1"}"#)
///     .push_status(204);
/// assert_eq!(stub.remaining(), 2);
/// assert_eq!(stub.call_count(), 0);
/// ```
#[derive(Debug, Default)]
pub struct StubTransport {
    script: Mutex<VecDeque<Outcome>>,
    recorded: Mutex<Vec<RecordedRequest>>,
}

impl StubTransport {
    /// An empty script: every call so far has to be queued before the client is
    /// driven.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    ///
    /// assert_eq!(StubTransport::new().remaining(), 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a literal response.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    /// use messenger_webhook::matrix_out::transport::Response;
    ///
    /// let stub = StubTransport::new();
    /// stub.push(Response::json(200, "{}"));
    /// assert_eq!(stub.remaining(), 1);
    /// ```
    pub fn push(&self, response: Response) -> &Self {
        self.lock_script().push_back(Outcome::Response(response));
        self
    }

    /// Queues a JSON response.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    ///
    /// let stub = StubTransport::new();
    /// stub.push_json(200, r#"{"event_id":"$1"}"#);
    /// assert_eq!(stub.remaining(), 1);
    /// ```
    pub fn push_json(&self, status: u16, body: &str) -> &Self {
        self.push(Response::json(status, body))
    }

    /// Queues a status with an empty body — what a typing or receipt leg
    /// answers.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    ///
    /// let stub = StubTransport::new();
    /// stub.push_status(200);
    /// assert_eq!(stub.remaining(), 1);
    /// ```
    pub fn push_status(&self, status: u16) -> &Self {
        self.push(Response::new(status, Vec::new()))
    }

    /// Queues a transport-level failure.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    /// use messenger_webhook::matrix_out::transport::TransportError;
    ///
    /// let stub = StubTransport::new();
    /// stub.push_failure(TransportError::timeout("30s"));
    /// assert_eq!(stub.remaining(), 1);
    /// ```
    pub fn push_failure(&self, error: TransportError) -> &Self {
        self.lock_script().push_back(Outcome::Failure(error));
        self
    }

    /// Every request the stub has answered, in call order.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    ///
    /// assert!(StubTransport::new().requests().is_empty());
    /// ```
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.lock_recorded().clone()
    }

    /// How many requests have been made.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    ///
    /// assert_eq!(StubTransport::new().call_count(), 0);
    /// ```
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.lock_recorded().len()
    }

    /// The most recent request, if any.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    ///
    /// assert!(StubTransport::new().last_request().is_none());
    /// ```
    #[must_use]
    pub fn last_request(&self) -> Option<RecordedRequest> {
        self.lock_recorded().last().cloned()
    }

    /// How many scripted outcomes are still queued.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::StubTransport;
    ///
    /// let stub = StubTransport::new();
    /// stub.push_status(200);
    /// assert_eq!(stub.remaining(), 1);
    /// ```
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.lock_script().len()
    }

    fn lock_script(&self) -> std::sync::MutexGuard<'_, VecDeque<Outcome>> {
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_recorded(&self) -> std::sync::MutexGuard<'_, Vec<RecordedRequest>> {
        self.recorded.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Transport for StubTransport {
    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Response, TransportError>> {
        Box::pin(async move {
            let recorded = RecordedRequest {
                method: request.method(),
                url: request.url().to_owned(),
                headers: request.headers().to_vec(),
                body: match request.body() {
                    Body::Empty => Vec::new(),
                    body => body.as_bytes().to_vec(),
                },
            };
            self.lock_recorded().push(recorded);
            match self.lock_script().pop_front() {
                Some(Outcome::Response(response)) => Ok(response),
                Some(Outcome::Failure(error)) => Err(error),
                None => Err(TransportError::other(
                    "stub transport ran out of scripted responses",
                )),
            }
        })
    }
}

/// A [`Sleeper`] that records the delays instead of waiting for them.
///
/// Inject it through
/// [`ClientConfig::with_sleeper`](crate::matrix_out::ClientConfig::with_sleeper)
/// to assert the exact backoff schedule a delivery follows without making the
/// test slow.
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, RecordingSleeper, StubTransport};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let sleeper = Arc::new(RecordingSleeper::new());
/// let transport = Arc::new(StubTransport::new());
/// transport
///     .push_status(503)
///     .push_json(200, r#"{"event_id":"$1"}"#);
///
/// let client = MatrixClient::new(
///     transport.clone(),
///     ClientConfig::new("https://m.example")?.with_sleeper(sleeper.clone()),
///     "syt",
/// )?;
///
/// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// let outcome = runtime.block_on(client.send_text("!r:m.example", "Hallo"))?;
/// assert_eq!(outcome.attempts, 2);
/// // One wait, and it is the policy's first backoff step — not a guessed delay.
/// assert_eq!(sleeper.delays(), vec![Duration::from_millis(500)]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct RecordingSleeper {
    delays: Mutex<Vec<Duration>>,
}

impl RecordingSleeper {
    /// A sleeper that has waited for nothing yet.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::RecordingSleeper;
    ///
    /// assert!(RecordingSleeper::new().delays().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every delay the client asked for, in order.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::RecordingSleeper;
    ///
    /// assert_eq!(RecordingSleeper::new().delays().len(), 0);
    /// ```
    #[must_use]
    pub fn delays(&self) -> Vec<Duration> {
        self.delays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The total time the client would have waited.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::RecordingSleeper;
    ///
    /// assert_eq!(RecordingSleeper::new().total(), std::time::Duration::ZERO);
    /// ```
    #[must_use]
    pub fn total(&self) -> Duration {
        self.delays().iter().copied().sum()
    }
}

impl Sleeper for RecordingSleeper {
    fn sleep(&self, delay: Duration) -> BoxFuture<'_, ()> {
        self.delays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(delay);
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix_out::transport::TransportErrorKind;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime")
    }

    fn call(stub: &StubTransport) -> Result<Response, TransportError> {
        runtime().block_on(
            stub.send(
                Request::new(Method::Put, "https://example.org/x")
                    .with_header("authorization", "Bearer secret")
                    .with_body(Body::Json(b"{}".to_vec())),
            ),
        )
    }

    #[test]
    fn the_script_is_replayed_in_order_and_exhaustion_is_a_failure() {
        let stub = StubTransport::new();
        stub.push_json(200, r#"{"event_id":"$1"}"#);
        stub.push_failure(TransportError::connect("dns"));
        assert_eq!(call(&stub).expect("first").status(), 200);
        let second = call(&stub).expect_err("second");
        assert_eq!(second.kind, TransportErrorKind::Connect);
        let third = call(&stub).expect_err("past the end of the script");
        assert_eq!(third.kind, TransportErrorKind::Other);
        assert!(third.message.contains("ran out"));
        assert_eq!(stub.remaining(), 0);
        assert_eq!(stub.call_count(), 3);
    }

    #[test]
    fn requests_are_recorded_without_the_credential() {
        let stub = StubTransport::new();
        stub.push_status(200);
        call(&stub).expect("scripted");
        let recorded = stub.last_request().expect("one call");
        assert_eq!(recorded.method, Method::Put);
        assert_eq!(recorded.header("authorization"), Some("Bearer secret"));
        assert_eq!(recorded.json().expect("json body"), serde_json::json!({}));
        assert!(!format!("{recorded:?}").contains("secret"));
        assert_eq!(stub.requests().len(), 1);
    }

    #[test]
    fn a_missing_header_or_body_is_reported_honestly() {
        let request = RecordedRequest {
            method: Method::Get,
            url: "u".into(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        assert_eq!(request.header("accept"), None);
        assert!(request.json().is_err());
        assert!(format!("{request:?}").contains("0 bytes"));
    }

    #[test]
    fn the_recording_sleeper_collects_delays() {
        let sleeper = RecordingSleeper::new();
        runtime().block_on(async {
            sleeper.sleep(Duration::from_millis(10)).await;
            sleeper.sleep(Duration::from_millis(20)).await;
        });
        assert_eq!(
            sleeper.delays(),
            vec![Duration::from_millis(10), Duration::from_millis(20)]
        );
        assert_eq!(sleeper.total(), Duration::from_millis(30));
    }
}
