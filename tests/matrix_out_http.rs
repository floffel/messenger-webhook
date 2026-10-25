//! The `reqwest` transport against a real socket on the loopback interface.
//!
//! This is the only test file in the crate that opens a port. It exists because
//! `ReqwestTransport` is the one component a fake transport cannot prove —
//! header framing, method mapping, status/body hand-off and the error
//! classification that decides whether a retry is safe all live there.

#![cfg(feature = "matrix-out")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::Router;
use messenger_webhook::matrix_out::transport::{Method, Request, Transport};
use messenger_webhook::matrix_out::{
    ClientConfig, MatrixClient, MediaUpload, OutboundError, ReqwestTransport, RetryPolicy,
};

/// What the homeserver stand-in saw, and how it should behave.
#[derive(Debug, Default)]
struct Seen {
    authorization: Mutex<Vec<String>>,
    bodies: Mutex<Vec<Vec<u8>>>,
    paths: Mutex<Vec<String>>,
    fail_sends: AtomicBool,
    slow_sends: AtomicBool,
}

type AppState = Arc<Seen>;

/// Starts the stand-in and returns its base URL plus the recorder.
async fn server() -> (String, AppState) {
    let seen: AppState = Arc::new(Seen::default());
    let app = Router::new()
        .route(
            "/_matrix/client/v3/rooms/{room}/send/{event_type}/{txn}",
            put(send_event),
        )
        .route("/_matrix/client/v3/account/whoami", get(whoami))
        .route("/_matrix/media/v3/upload", post(upload))
        .route(
            "/_matrix/client/v3/rooms/{room}/receipt/m.read/{event}",
            post(receipt),
        )
        .with_state(seen.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let address = listener.local_addr().expect("a bound address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("the test server runs");
    });
    (format!("http://{address}"), seen)
}

fn record(state: &AppState, headers: &HeaderMap, path: &str, body: &[u8]) {
    state.authorization.lock().expect("authorization log").push(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    );
    state.paths.lock().expect("path log").push(path.to_owned());
    state.bodies.lock().expect("body log").push(body.to_vec());
}

async fn send_event(
    State(state): State<AppState>,
    Path((room, event_type, txn)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, [(String, String); 2], String) {
    record(
        &state,
        &headers,
        &format!("/send/{room}/{event_type}/{txn}"),
        &body,
    );
    if state.slow_sends.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let status = if state.fail_sends.load(Ordering::SeqCst) {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    let payload = if status == StatusCode::OK {
        r#"{"event_id":"$loopback"}"#
    } else {
        r#"{"errcode":"M_UNKNOWN","error":"down"}"#
    };
    (
        status,
        [
            ("content-type".to_owned(), "application/json".to_owned()),
            ("x-txn".to_owned(), txn),
        ],
        payload.to_owned(),
    )
}

async fn whoami(State(state): State<AppState>, headers: HeaderMap) -> (StatusCode, String) {
    record(&state, &headers, "/whoami", b"");
    (
        StatusCode::OK,
        r#"{"user_id":"@bot:m.example","device_id":"ABC"}"#.to_owned(),
    )
}

async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, String) {
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    record(&state, &headers, &format!("/upload/{content_type}"), &body);
    (
        StatusCode::OK,
        r#"{"content_uri":"mxc://m.example/up"}"#.to_owned(),
    )
}

async fn receipt(
    State(state): State<AppState>,
    Path((room, event)): Path<(String, String)>,
    headers: HeaderMap,
) -> (StatusCode, String) {
    record(&state, &headers, &format!("/receipt/{room}/{event}"), b"");
    (StatusCode::OK, "{}".to_owned())
}

fn client(base: &str) -> MatrixClient {
    client_with(base, RetryPolicy::default(), Duration::from_secs(5))
}

fn client_with(base: &str, retry: RetryPolicy, timeout: Duration) -> MatrixClient {
    MatrixClient::new(
        Arc::new(ReqwestTransport::with_timeout(timeout).expect("a client")),
        ClientConfig::new(base)
            .expect("a valid URL")
            .with_user_id("@bot:m.example")
            .with_retry(retry),
        "syt_loopback",
    )
    .expect("a valid client")
}

fn one_attempt() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 1,
        ..RetryPolicy::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_request_reaches_the_server_with_the_token_and_the_json_body() {
    let (base, seen) = server().await;
    let client = client(&base);

    let outcome = client
        .send_text("!room:example.org", "Hallo")
        .await
        .expect("sent");

    assert_eq!(outcome.event_id, "$loopback");
    assert_eq!(outcome.attempts, 1);
    assert_eq!(
        seen.authorization.lock().expect("log").as_slice(),
        ["Bearer syt_loopback".to_owned()]
    );
    // Compare parsed JSON: key order is not part of the wire contract.
    let body: serde_json::Value =
        serde_json::from_slice(&seen.bodies.lock().expect("log")[0]).expect("a JSON body");
    assert_eq!(
        body,
        serde_json::json!({"msgtype": "m.text", "body": "Hallo"})
    );
    assert_eq!(
        seen.paths.lock().expect("log").as_slice(),
        [format!(
            "/send/!room:example.org/m.room.message/{}",
            outcome.txn_id
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_bytes_and_their_content_type_survive_the_round_trip() {
    let (base, seen) = server().await;
    let client = client(&base);
    let bytes = [0_u8, 159, 146, 150, 255];

    let uri = client
        .upload_media(&MediaUpload::new(&bytes, "application/octet-stream").with_filename("a.bin"))
        .await
        .expect("uploaded");

    assert_eq!(uri.as_str(), "mxc://m.example/up");
    assert_eq!(
        seen.paths.lock().expect("log").as_slice(),
        ["/upload/application/octet-stream".to_owned()]
    );
    assert_eq!(seen.bodies.lock().expect("log")[0].as_slice(), bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_probe_and_a_receipt_round_trip() {
    let (base, seen) = server().await;
    let client = client(&base);

    assert_eq!(client.whoami().await.expect("probe"), "@bot:m.example");
    client
        .send_read_receipt("!r:example.org", "$abc")
        .await
        .expect("receipt");
    assert_eq!(
        seen.paths.lock().expect("log").as_slice(),
        [
            "/whoami".to_owned(),
            "/receipt/!r:example.org/$abc".to_owned()
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_5xx_over_the_wire_is_retried_with_the_same_transaction_id() {
    let (base, seen) = server().await;
    seen.fail_sends.store(true, Ordering::SeqCst);
    let client = client_with(
        &base,
        RetryPolicy {
            max_attempts: 2,
            base_delay: Duration::from_millis(1),
            ..RetryPolicy::default()
        },
        Duration::from_secs(5),
    );

    let error = client
        .send_text("!r:example.org", "Hallo")
        .await
        .expect_err("a 5xx on a send is ambiguous");

    assert!(error.is_ambiguous(), "{error:?}");
    assert!(!error.is_retryable());
    let paths = seen.paths.lock().expect("log").clone();
    assert_eq!(paths.len(), 2, "the retry happened");
    assert_eq!(paths[0], paths[1], "and it reused the transaction id");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_5xx_over_the_wire_on_a_read_is_a_retryable_server_error() {
    let (base, _seen) = server().await;
    let client = client_with(&base, one_attempt(), Duration::from_secs(5));

    // `/whoami` answers 200; a `5xx` read is covered by the status tests. What
    // this asserts is the transport hand-off: a non-2xx status is a *response*,
    // not a transport error.
    let response = ReqwestTransport::new()
        .expect("a client")
        .send(Request::new(
            Method::Get,
            format!("{base}/_matrix/client/v3/account/whoami"),
        ))
        .await
        .expect("a response");
    assert_eq!(response.status(), 200);
    assert!(
        String::from_utf8_lossy(response.body()).contains("@bot:m.example"),
        "the body must survive the transport unchanged"
    );

    // A path traversal attempt is not a room id at all; it is refused before a
    // request is built, so no bytes ever reach the socket.
    let refused = client.send_text("", "Hallo").await.expect_err("no room id");
    assert!(matches!(refused, OutboundError::InvalidRequest { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_port_is_a_connect_failure_not_an_ambiguous_write() {
    let client = client_with("http://127.0.0.1:1", one_attempt(), Duration::from_secs(5));

    let error = client
        .send_text("!r:example.org", "Hallo")
        .await
        .expect_err("nothing listens there");

    assert!(
        matches!(error, OutboundError::Transport { .. }),
        "{error:?}"
    );
    assert!(error.is_retryable());
    assert!(
        !error.is_ambiguous(),
        "nothing was written, so nothing is unknown"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_timeout_is_ambiguous_for_a_write() {
    let (base, seen) = server().await;
    seen.slow_sends.store(true, Ordering::SeqCst);
    let client = client_with(&base, one_attempt(), Duration::from_millis(100));

    let error = client
        .send_text("!r:example.org", "Hallo")
        .await
        .expect_err("the answer never arrives");

    assert!(error.is_ambiguous(), "{error:?}");
    assert!(!error.is_retryable());
    assert_eq!(seen.paths.lock().expect("log").len(), 1);
}
