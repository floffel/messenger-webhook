//! The shipped [`Transport`]: `reqwest` with rustls.
//!
//! This is the only module in the crate that opens a socket. It is deliberately
//! dumb — build the request, execute it, hand back the status, headers and body —
//! because every decision that matters (retry, idempotency, decoding) belongs to
//! the client, where it can be tested without a network.

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client as HttpClient, Method as HttpMethod};

use crate::matrix_out::error::{OutboundError, OutboundResult};
use crate::matrix_out::transport::{
    Body, BoxFuture, Method, Request, Response, Transport, TransportError,
};

/// Request timeout the transport applies when the caller does not pick one.
///
/// Thirty seconds is long enough for a media upload of a few megabytes on a slow
/// link and short enough that a wedged connection does not hold a Lambda
/// invocation open. It doubles as the boundary between "safe to retry" and
/// "ambiguous": a timeout here is reported as
/// [`TransportErrorKind::Timeout`](crate::matrix_out::transport::TransportErrorKind::Timeout),
/// never as a connection failure.
///
/// ```
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::DEFAULT_REQUEST_TIMEOUT;
///
/// assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(30));
/// ```
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A [`Transport`] backed by `reqwest`.
///
/// ```
/// use messenger_webhook::matrix_out::ReqwestTransport;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let transport = ReqwestTransport::new()?;
/// assert_eq!(transport.timeout(), messenger_webhook::matrix_out::DEFAULT_REQUEST_TIMEOUT);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: HttpClient,
    timeout: Duration,
}

impl ReqwestTransport {
    /// A transport with the default request timeout.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidConfig`] when the underlying TLS backend cannot be
    /// initialised — a build-time problem, not a request problem.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ReqwestTransport;
    ///
    /// assert!(ReqwestTransport::new().is_ok());
    /// ```
    pub fn new() -> OutboundResult<Self> {
        Self::with_timeout(DEFAULT_REQUEST_TIMEOUT)
    }

    /// A transport with an explicit request timeout.
    ///
    /// # Errors
    ///
    /// [`OutboundError::InvalidConfig`] when the client cannot be built, or
    /// [`OutboundError::InvalidConfig`] for a zero timeout (which `reqwest` reads
    /// as "no timeout at all", the opposite of what a caller asking for `0`
    /// wants).
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::ReqwestTransport;
    ///
    /// assert!(ReqwestTransport::with_timeout(Duration::from_secs(5)).is_ok());
    /// assert!(ReqwestTransport::with_timeout(Duration::ZERO).is_err());
    /// ```
    pub fn with_timeout(timeout: Duration) -> OutboundResult<Self> {
        if timeout.is_zero() {
            return Err(OutboundError::InvalidConfig {
                reason: "a zero request timeout means no timeout; pick a duration".to_owned(),
            });
        }
        let client = HttpClient::builder()
            .timeout(timeout)
            .user_agent(concat!("messenger-webhook/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| OutboundError::InvalidConfig {
                reason: format!("building the HTTP client failed: {}", error.without_url()),
            })?;
        Ok(Self { client, timeout })
    }

    /// The request timeout in force.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::ReqwestTransport;
    ///
    /// assert_eq!(
    ///     ReqwestTransport::new()?.timeout(),
    ///     messenger_webhook::matrix_out::DEFAULT_REQUEST_TIMEOUT,
    /// );
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Transport for ReqwestTransport {
    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Response, TransportError>> {
        Box::pin(async move {
            let method = http_method(request.method());
            let mut builder = self.client.request(method, request.url());
            let headers = header_map(request.headers())?;
            builder = builder.headers(headers);

            let body = match request.body() {
                Body::Empty => None,
                body => Some(body.as_bytes().to_vec()),
            };
            if let Some(body) = body {
                builder = builder.body(body);
            }

            let response = builder
                .send()
                .await
                .map_err(|error| transport_error(&error))?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect();
            let body = response
                .bytes()
                .await
                .map_err(|error| transport_error(&error))?
                .to_vec();
            Ok(Response {
                status,
                headers,
                body,
            })
        })
    }
}

/// The `reqwest` spelling of the method.
fn http_method(method: Method) -> HttpMethod {
    match method {
        Method::Get => HttpMethod::GET,
        Method::Put => HttpMethod::PUT,
        Method::Post => HttpMethod::POST,
    }
}

/// Builds the header map, refusing a name or value HTTP cannot carry.
fn header_map(headers: &[(String, String)]) -> Result<HeaderMap, TransportError> {
    let mut map = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            TransportError::other(format!(
                "the header name {name:?} is not valid HTTP: {error}"
            ))
        })?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            TransportError::other(format!(
                "the value of header {name:?} is not valid HTTP: {error}"
            ))
        })?;
        map.insert(name, value);
    }
    Ok(map)
}

/// Maps a `reqwest` failure onto the write boundary the retry policy cares about.
fn transport_error(error: &reqwest::Error) -> TransportError {
    // `without_url` takes ownership and a `reqwest::Error` is not `Clone`; the
    // URL it would strip carries a room id or an upload path, never the token
    // (that travels in a header), so keeping it in the message is harmless.
    let message = error.to_string();
    if error.is_connect() {
        TransportError::connect(message)
    } else if error.is_timeout() {
        TransportError::timeout(message)
    } else {
        TransportError::other(message)
    }
}

impl fmt::Display for ReqwestTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReqwestTransport(timeout {:?})", self.timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_map_onto_http() {
        assert_eq!(http_method(Method::Get), HttpMethod::GET);
        assert_eq!(http_method(Method::Put), HttpMethod::PUT);
        assert_eq!(http_method(Method::Post), HttpMethod::POST);
    }

    #[test]
    fn a_header_that_http_cannot_carry_is_a_transport_failure() {
        let ok = header_map(&[("accept".to_owned(), "application/json".to_owned())])
            .expect("a valid header");
        assert_eq!(
            ok.get("accept").expect("present").to_str().expect("ascii"),
            "application/json"
        );

        let bad_name = header_map(&[("bad header".to_owned(), "x".to_owned())]);
        assert!(bad_name.is_err());
        let bad_value = header_map(&[("accept".to_owned(), "line\r\nbreak".to_owned())]);
        assert!(bad_value.is_err());
    }

    #[test]
    fn the_transport_reports_its_timeout_and_names_itself() {
        let transport =
            ReqwestTransport::with_timeout(Duration::from_millis(1_500)).expect("built");
        assert_eq!(transport.timeout(), Duration::from_millis(1_500));
        assert!(transport.to_string().contains("1.5s"));
        assert!(format!("{transport:?}").contains("ReqwestTransport"));
    }
}
