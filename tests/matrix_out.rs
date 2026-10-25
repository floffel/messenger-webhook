//! Delivery behaviour of the outbound client: one real fake, no sockets.
//!
//! Every test drives a [`MatrixClient`] through a [`StubTransport`] and a
//! [`RecordingSleeper`], so the assertions are about the *protocol* — which URL,
//! which transaction id, how many attempts, which error — and never about a
//! wall clock or a server.

#![cfg(feature = "matrix-out")]

use std::sync::Arc;
use std::time::Duration;

use messenger_webhook::matrix_out::testing::RecordedRequest;
use messenger_webhook::matrix_out::transport::{Method, TransportError};
use messenger_webhook::matrix_out::{
    ClientConfig, MatrixClient, MediaKind, MediaUpload, OutboundError, RecordingSleeper,
    RetryPolicy, StubTransport, TxnId,
};

/// A client wired to a stub and a recording sleeper, plus both handles.
struct Harness {
    client: MatrixClient,
    transport: Arc<StubTransport>,
    sleeper: Arc<RecordingSleeper>,
}

fn harness(configure: impl FnOnce(ClientConfig) -> ClientConfig) -> Harness {
    let transport = Arc::new(StubTransport::new());
    let sleeper = Arc::new(RecordingSleeper::new());
    let config = configure(
        ClientConfig::new("https://matrix.example.org")
            .expect("a valid homeserver")
            .with_user_id("@bot:example.org")
            .with_sleeper(sleeper.clone()),
    );
    let client = MatrixClient::new(transport.clone(), config, "syt_TOKEN").expect("a valid client");
    Harness {
        client,
        transport,
        sleeper,
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(future)
}

fn requests(harness: &Harness) -> Vec<RecordedRequest> {
    harness.transport.requests()
}

#[test]
fn a_text_send_is_one_put_with_a_bearer_token_and_a_json_body() {
    let harness = harness(|config| config);
    harness.transport.push_json(200, r#"{"event_id":"$abc"}"#);

    let outcome = block_on(harness.client.send_text("!room:example.org", "Hallo")).expect("sent");

    assert_eq!(outcome.event_id, "$abc");
    assert_eq!(outcome.attempts, 1);
    let request = &requests(&harness)[0];
    assert_eq!(request.method, Method::Put);
    assert_eq!(
        request.url,
        format!(
            "https://matrix.example.org/_matrix/client/v3/rooms/%21room%3Aexample.org/send/m.room.message/{}",
            outcome.txn_id
        )
    );
    assert_eq!(request.header("authorization"), Some("Bearer syt_TOKEN"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(
        request.json().expect("json"),
        serde_json::json!({"msgtype": "m.text", "body": "Hallo"})
    );
}

#[test]
fn a_429_is_retried_after_exactly_the_delay_the_server_asked_for() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(
            429,
            r#"{"errcode":"M_LIMIT_EXCEEDED","error":"Too many requests","retry_after_ms":2500}"#,
        )
        .push_json(200, r#"{"event_id":"$abc"}"#);

    let outcome = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect("sent");

    assert_eq!(outcome.attempts, 2);
    assert_eq!(harness.sleeper.delays(), vec![Duration::from_millis(2_500)]);
    // Both attempts carry the same transaction id, so the room sees one event.
    let calls = requests(&harness);
    let urls: Vec<&str> = calls.iter().map(|request| request.url.as_str()).collect();
    assert_eq!(urls[0], urls[1]);
}

#[test]
fn a_429_without_a_retry_after_falls_back_to_the_backoff() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(429, r#"{"errcode":"M_LIMIT_EXCEEDED"}"#)
        .push_json(200, r#"{"event_id":"$abc"}"#);

    block_on(harness.client.send_text("!r:example.org", "Hallo")).expect("sent");
    assert_eq!(harness.sleeper.delays(), vec![Duration::from_millis(500)]);
}

#[test]
fn a_429_asking_for_more_than_the_budget_stops_without_waiting() {
    let harness = harness(|config| {
        config.with_retry(RetryPolicy {
            max_rate_limit_wait: Duration::from_secs(5),
            ..RetryPolicy::default()
        })
    });
    harness.transport.push_json(
        429,
        r#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":60000}"#,
    );

    let error = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect_err("limited");

    assert!(matches!(
        error,
        OutboundError::RateLimited {
            retry_after: Some(_),
            ..
        }
    ));
    assert!(error.is_retryable());
    assert!(!error.is_ambiguous());
    assert!(
        harness.sleeper.delays().is_empty(),
        "nothing should be waited out"
    );
    assert_eq!(harness.transport.call_count(), 1);
}

#[test]
fn a_5xx_on_a_txn_keyed_send_is_retried_with_the_same_transaction_id() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(502, r#"{"errcode":"M_UNKNOWN"}"#)
        .push_json(200, r#"{"event_id":"$abc"}"#);

    let outcome = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect("sent");

    assert_eq!(outcome.attempts, 2);
    let calls = requests(&harness);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].url, calls[1].url, "a retry must reuse the txn id");
    assert_eq!(calls[0].body, calls[1].body);
    assert_eq!(harness.sleeper.delays(), vec![Duration::from_millis(500)]);
}

#[test]
fn an_exhausted_send_is_ambiguous_and_carries_the_transaction_id() {
    let harness = harness(|config| config);
    for _ in 0..3 {
        harness
            .transport
            .push_json(503, r#"{"errcode":"M_UNKNOWN","error":"overloaded"}"#);
    }

    let error = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect_err("5xx");

    let txn = match &error {
        OutboundError::Ambiguous {
            txn_id: Some(txn),
            attempts: 3,
            ..
        } => txn.clone(),
        other => panic!("expected an ambiguous write, got {other:?}"),
    };
    assert!(error.is_ambiguous());
    assert!(
        !error.is_retryable(),
        "an ambiguous write must not be retried blindly"
    );
    assert_eq!(harness.transport.call_count(), 3);
    assert_eq!(harness.sleeper.delays().len(), 2);
    // The error tells the caller how to repeat it safely.
    assert!(error.to_string().contains(txn.as_str()));

    // Repeating with that id is the documented recovery path, and it works.
    harness.transport.push_json(200, r#"{"event_id":"$late"}"#);
    let outcome = block_on(harness.client.send_text_with_txn(
        txn.clone(),
        "!r:example.org",
        "Hallo",
    ))
    .expect("the retry lands");
    assert_eq!(outcome.txn_id, txn);
    assert!(requests(&harness)
        .last()
        .expect("a request")
        .url
        .ends_with(txn.as_str()));
}

#[test]
fn an_upload_is_never_retried_after_a_5xx() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(500, r#"{"errcode":"M_UNKNOWN"}"#);

    let media = MediaUpload::new(b"\x89PNG", "image/png");
    let error = block_on(harness.client.upload_media(&media)).expect_err("5xx");

    assert!(error.is_ambiguous(), "{error:?}");
    assert!(!error.is_retryable());
    assert_eq!(
        harness.transport.call_count(),
        1,
        "a non-idempotent upload must not be repeated after a 5xx"
    );
    assert_eq!(harness.sleeper.delays(), [] as [std::time::Duration; 0]);
}

#[test]
fn an_upload_timeout_is_ambiguous_but_a_connect_failure_is_retried() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_failure(TransportError::timeout("30s"));
    let media = MediaUpload::new(b"x", "text/plain");
    let ambiguous = block_on(harness.client.upload_media(&media)).expect_err("timeout");
    assert!(ambiguous.is_ambiguous());
    assert_eq!(harness.transport.call_count(), 1);

    // A connect failure never reached the homeserver, so it is safe to repeat —
    // even for an upload.
    harness
        .transport
        .push_failure(TransportError::connect("dns"))
        .push_json(200, r#"{"content_uri":"mxc://m.example/x"}"#);
    let uri = block_on(harness.client.upload_media(&media)).expect("uploaded");
    assert_eq!(uri.as_str(), "mxc://m.example/x");
    assert_eq!(harness.sleeper.delays(), vec![Duration::from_millis(500)]);
}

#[test]
fn a_connection_that_never_opened_is_retryable_not_ambiguous() {
    let harness = harness(|config| {
        config.with_retry(RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        })
    });
    harness
        .transport
        .push_failure(TransportError::connect("dns failure"))
        .push_failure(TransportError::connect("dns failure"));

    let error = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect_err("dns");

    assert!(matches!(
        error,
        OutboundError::Transport { attempts: 2, .. }
    ));
    assert!(error.is_retryable());
    assert!(!error.is_ambiguous());
}

#[test]
fn a_read_timeout_on_a_send_is_retried_and_then_ambiguous() {
    let harness = harness(|config| {
        config.with_retry(RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        })
    });
    harness
        .transport
        .push_failure(TransportError::timeout("30s"))
        .push_failure(TransportError::timeout("30s"));

    let error = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect_err("timeout");

    assert!(error.is_ambiguous(), "{error:?}");
    assert_eq!(
        harness.transport.call_count(),
        2,
        "a txn-keyed send may be retried"
    );
    assert_eq!(harness.sleeper.delays(), vec![Duration::from_millis(500)]);
}

#[test]
fn a_4xx_is_terminal_and_never_retried() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(403, r#"{"errcode":"M_FORBIDDEN","error":"not joined"}"#);

    let error = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect_err("403");

    match &error {
        OutboundError::Rejected {
            status,
            errcode,
            message,
        } => {
            assert_eq!(*status, 403);
            assert_eq!(errcode, "M_FORBIDDEN");
            assert_eq!(message, "not joined");
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(!error.is_retryable());
    assert!(!error.is_ambiguous());
    assert_eq!(harness.transport.call_count(), 1);
    assert_eq!(harness.sleeper.delays(), [] as [std::time::Duration; 0]);
}

#[test]
fn a_2xx_with_an_unreadable_body_is_ambiguous_for_a_write() {
    let harness = harness(|config| config);
    harness.transport.push_json(200, "<html>not json</html>");

    let error = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect_err("garbage");
    assert!(error.is_ambiguous(), "{error:?}");
    assert!(!error.is_retryable());
    assert_eq!(harness.transport.call_count(), 1);
}

#[test]
fn a_2xx_without_an_event_id_is_ambiguous_for_a_write() {
    let harness = harness(|config| config);
    harness.transport.push_json(200, r#"{"room_id":"!r"}"#);

    let error = block_on(harness.client.send_text("!r:example.org", "Hallo")).expect_err("no id");
    assert!(error.is_ambiguous(), "{error:?}");
    assert!(error.to_string().contains("event_id"));
}

#[test]
fn an_unreadable_probe_answer_is_a_decode_error_not_an_ambiguous_write() {
    let harness = harness(|config| config);
    harness.transport.push_json(200, r#"{"device_id":"X"}"#);

    let error = block_on(harness.client.whoami()).expect_err("no user_id");
    assert!(matches!(
        error,
        OutboundError::Decode { what: "whoami", .. }
    ));
    assert!(!error.is_ambiguous());
}

#[test]
fn the_probe_returns_the_user_id_and_tolerates_extra_fields() {
    let harness = harness(|config| config);
    harness.transport.push_json(
        200,
        r#"{"user_id":"@bot:example.org","device_id":"ABC","is_guest":false}"#,
    );

    assert_eq!(
        block_on(harness.client.whoami()).expect("probe"),
        "@bot:example.org"
    );
    let request = &requests(&harness)[0];
    assert_eq!(request.method, Method::Get);
    assert_eq!(
        request.url,
        "https://matrix.example.org/_matrix/client/v3/account/whoami"
    );
    assert_eq!(request.body, [] as [u8; 0]);
}

#[test]
fn the_probe_is_retried_after_a_5xx_and_reports_a_retryable_server_error() {
    let harness = harness(|config| config);
    for _ in 0..3 {
        harness
            .transport
            .push_json(503, r#"{"errcode":"M_UNKNOWN","error":"down"}"#);
    }

    let error = block_on(harness.client.whoami()).expect_err("down");

    match &error {
        OutboundError::Server {
            status,
            errcode,
            attempts,
            ..
        } => {
            assert_eq!(*status, 503);
            assert_eq!(errcode, "M_UNKNOWN");
            assert_eq!(*attempts, 3);
        }
        other => panic!("expected a server error, got {other:?}"),
    }
    assert!(error.is_retryable(), "a GET may always be repeated");
    assert!(!error.is_ambiguous());
}

#[test]
fn typing_and_receipts_post_the_documented_bodies() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_status(200)
        .push_status(200)
        .push_status(200);

    block_on(harness.client.send_typing("!r:example.org", 4_000)).expect("typing");
    block_on(harness.client.stop_typing("!r:example.org")).expect("stop typing");
    block_on(harness.client.send_read_receipt("!r:example.org", "$abc")).expect("receipt");

    let calls = requests(&harness);
    assert_eq!(
        calls[0].url,
        "https://matrix.example.org/_matrix/client/v3/rooms/%21r%3Aexample.org/typing/%40bot%3Aexample.org"
    );
    assert_eq!(
        calls[0].json().expect("json"),
        serde_json::json!({"typing": true, "timeout": 4000})
    );
    assert_eq!(
        calls[1].json().expect("json"),
        serde_json::json!({"typing": false})
    );
    assert_eq!(calls[2].method, Method::Post);
    assert_eq!(
        calls[2].url,
        "https://matrix.example.org/_matrix/client/v3/rooms/%21r%3Aexample.org/receipt/m.read/%24abc"
    );
    assert_eq!(calls[2].json().expect("json"), serde_json::json!({}));
}

#[test]
fn typing_without_a_configured_user_id_is_a_configuration_error() {
    let transport = Arc::new(StubTransport::new());
    let client = MatrixClient::new(
        transport.clone(),
        ClientConfig::new("https://m.example").expect("valid"),
        "syt",
    )
    .expect("valid");

    let error = block_on(client.send_typing("!r:example.org", 4_000)).expect_err("no user id");
    assert!(matches!(error, OutboundError::InvalidConfig { .. }));
    assert_eq!(transport.call_count(), 0);

    let zero = block_on(client.send_typing("!r:example.org", 0)).expect_err("zero timeout");
    assert!(matches!(zero, OutboundError::InvalidRequest { .. }));
}

#[test]
fn an_exhausted_typing_pulse_is_a_retryable_server_error_not_ambiguous() {
    let harness = harness(|config| {
        config.with_retry(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        })
    });
    harness
        .transport
        .push_json(503, r#"{"errcode":"M_UNKNOWN"}"#);

    let error = block_on(harness.client.send_typing("!r:example.org", 4_000)).expect_err("503");
    assert!(matches!(error, OutboundError::Server { .. }), "{error:?}");
    assert!(error.is_retryable());
    assert!(
        !error.is_ambiguous(),
        "a typing pulse leaves nothing behind"
    );
}

#[test]
fn a_reaction_carries_the_annotation_relation() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"event_id":"$reaction"}"#);

    let outcome =
        block_on(harness.client.send_reaction("!r:example.org", "$abc", "👍")).expect("reacted");

    assert_eq!(outcome.event_id, "$reaction");
    let request = &requests(&harness)[0];
    assert!(request.url.contains("/send/m.reaction/"));
    assert_eq!(
        request.json().expect("json"),
        serde_json::json!({
            "m.relates_to": {
                "rel_type": "m.annotation",
                "event_id": "$abc",
                "key": "👍",
            }
        })
    );
}

#[test]
fn an_empty_reaction_key_is_refused_before_a_request_exists() {
    let harness = harness(|config| config);
    let error = block_on(harness.client.send_reaction("!r:example.org", "$abc", ""))
        .expect_err("empty key");
    assert!(matches!(error, OutboundError::InvalidRequest { .. }));
    assert!(error.to_string().contains("redact"));
    assert_eq!(harness.transport.call_count(), 0);
}

#[test]
fn a_redaction_puts_the_reason_or_nothing() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"event_id":"$redaction"}"#)
        .push_json(200, r#"{"event_id":"$redaction2"}"#);

    block_on(
        harness
            .client
            .redact("!r:example.org", "$abc", Some("typo")),
    )
    .expect("redacted");
    block_on(harness.client.redact("!r:example.org", "$abc", None)).expect("redacted");

    let calls = requests(&harness);
    assert_eq!(calls[0].method, Method::Put);
    assert!(calls[0].url.contains("/redact/%24abc/"));
    assert_eq!(
        calls[0].json().expect("json"),
        serde_json::json!({"reason": "typo"})
    );
    assert_eq!(calls[1].json().expect("json"), serde_json::json!({}));
}

#[test]
fn an_event_type_the_crate_does_not_model_is_sent_verbatim() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"event_id":"$custom"}"#);

    let outcome = block_on(harness.client.send_event(
        "!r:example.org",
        "com.example.deploy",
        serde_json::json!({"status": "green", "nested": {"a": [1, 2]}}),
    ))
    .expect("sent");

    assert_eq!(outcome.event_id, "$custom");
    let request = &requests(&harness)[0];
    assert!(request.url.contains("/send/com.example.deploy/"));
    assert_eq!(
        request.json().expect("json"),
        serde_json::json!({"status": "green", "nested": {"a": [1, 2]}})
    );
}

#[test]
fn media_is_uploaded_with_its_content_type_and_its_name_in_the_query() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"content_uri":"mxc://example.org/abc"}"#);

    let upload = MediaUpload::new(b"\x89PNG\r\n", "image/png").with_filename("Bild München.png");
    let uri = block_on(harness.client.upload_media(&upload)).expect("uploaded");

    assert_eq!(uri.as_str(), "mxc://example.org/abc");
    let request = &requests(&harness)[0];
    assert_eq!(request.method, Method::Post);
    assert_eq!(
        request.url,
        "https://matrix.example.org/_matrix/media/v3/upload?filename=Bild%20M%C3%BCnchen.png"
    );
    assert_eq!(request.header("content-type"), Some("image/png"));
    assert_eq!(request.body, b"\x89PNG\r\n");
}

#[test]
fn an_upload_without_a_name_has_no_query_parameter() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"content_uri":"mxc://example.org/abc"}"#);

    block_on(
        harness
            .client
            .upload_media(&MediaUpload::new(b"x", "text/plain")),
    )
    .expect("uploaded");
    assert_eq!(
        requests(&harness)[0].url,
        "https://matrix.example.org/_matrix/media/v3/upload"
    );
}

#[test]
fn an_oversized_upload_fails_before_any_bytes_are_sent() {
    let harness = harness(|config| config.with_max_upload_bytes(4));
    let at_limit = MediaUpload::new(b"abcd", "text/plain");
    let over = MediaUpload::new(b"abcde", "text/plain");

    harness
        .transport
        .push_json(200, r#"{"content_uri":"mxc://example.org/abc"}"#);
    block_on(harness.client.upload_media(&at_limit)).expect("exactly at the ceiling");

    let error = block_on(harness.client.upload_media(&over)).expect_err("over the ceiling");
    assert!(matches!(
        error,
        OutboundError::MediaTooLarge {
            limit: 4,
            actual: 5
        }
    ));
    assert_eq!(
        harness.transport.call_count(),
        1,
        "the second upload never left"
    );
}

#[test]
fn send_media_uploads_then_sends_one_message() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"content_uri":"mxc://example.org/abc"}"#)
        .push_json(200, r#"{"event_id":"$img"}"#);

    let upload = MediaUpload::new(b"png!", "image/png")
        .with_filename("cat.png")
        .with_kind(MediaKind::Image);
    let outcome =
        block_on(harness.client.send_media("!r:example.org", &upload, None)).expect("sent");

    assert_eq!(outcome.event_id, "$img");
    let calls = requests(&harness);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].json().expect("json"),
        serde_json::json!({
            "msgtype": "m.image",
            "body": "cat.png",
            "url": "mxc://example.org/abc",
            "info": {"mimetype": "image/png", "size": 4},
        })
    );
}

#[test]
fn send_media_prefers_the_caption_and_falls_back_to_the_msgtype() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"content_uri":"mxc://example.org/a"}"#)
        .push_json(200, r#"{"event_id":"$1"}"#)
        .push_json(200, r#"{"content_uri":"mxc://example.org/b"}"#)
        .push_json(200, r#"{"event_id":"$2"}"#);

    let upload = MediaUpload::new(b"x", "application/octet-stream").with_kind(MediaKind::File);
    block_on(
        harness
            .client
            .send_media("!r:example.org", &upload, Some("hier das PDF")),
    )
    .expect("captioned");
    block_on(harness.client.send_media("!r:example.org", &upload, None)).expect("bare");

    let bodies: Vec<String> = requests(&harness)
        .iter()
        .filter(|request| request.url.contains("/send/"))
        .map(|request| {
            request.json().expect("json")["body"]
                .as_str()
                .expect("a body")
                .to_owned()
        })
        .collect();
    assert_eq!(bodies, vec!["hier das PDF".to_owned(), "m.file".to_owned()]);
}

#[test]
fn an_ambiguous_upload_stops_send_media_before_the_message() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(500, r#"{"errcode":"M_UNKNOWN"}"#);

    let upload = MediaUpload::new(b"x", "text/plain");
    let error = block_on(harness.client.send_media("!r:example.org", &upload, None))
        .expect_err("upload failed");

    assert!(error.is_ambiguous());
    assert_eq!(harness.transport.call_count(), 1, "nothing was sent");
}

#[test]
fn a_caller_can_repeat_a_send_under_the_same_transaction_id() {
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"event_id":"$abc"}"#)
        .push_json(200, r#"{"event_id":"$abc"}"#);

    let txn = TxnId::new("order-42").expect("valid");
    let first = block_on(
        harness
            .client
            .send_text_with_txn(txn.clone(), "!r:example.org", "Hallo"),
    )
    .expect("first");
    let second = block_on(harness.client.send_text_with_txn(
        txn.clone(),
        "!r:example.org",
        "Hallo",
    ))
    .expect("second");

    assert_eq!(first.txn_id, second.txn_id);
    assert_eq!(first.event_id, second.event_id);
    let calls = requests(&harness);
    assert_eq!(
        calls[0].url, calls[1].url,
        "the id is the whole idempotency story"
    );
}

#[test]
fn empty_or_unaddressable_requests_never_reach_the_transport() {
    let harness = harness(|config| config);

    let cases: Vec<(Result<(), OutboundError>, &str)> = vec![
        (
            block_on(harness.client.send_text("", "Hallo")).map(|_| ()),
            "room id is empty",
        ),
        (
            block_on(harness.client.send_text("#alias:example.org", "Hallo")).map(|_| ()),
            "start with `!`",
        ),
        (
            block_on(harness.client.send_text("!r:example.org", "")).map(|_| ()),
            "body is empty",
        ),
        (
            block_on(harness.client.redact("!r:example.org", "", None)).map(|_| ()),
            "event id is empty",
        ),
        (
            block_on(harness.client.send_event(
                "!r:example.org",
                "m.bad/type",
                serde_json::json!({}),
            ))
            .map(|_| ()),
            "path separator",
        ),
        (
            block_on(harness.client.send_read_receipt("!r:example.org", "$a\nb")),
            "whitespace",
        ),
    ];
    for (result, needle) in cases {
        let error = result.expect_err("refused");
        assert!(
            error.to_string().contains(needle),
            "{error} should mention {needle:?}"
        );
    }
    assert_eq!(harness.transport.call_count(), 0);
}

#[test]
fn content_over_the_event_ceiling_is_refused_before_a_request_exists() {
    let harness = harness(|config| config);
    let body = "x".repeat(messenger_webhook::matrix_out::MAX_CONTENT_BYTES);

    let error = block_on(harness.client.send_text("!r:example.org", &body)).expect_err("too long");
    assert!(matches!(error, OutboundError::MessageTooLong { .. }));
    assert!(error.is_client_error());
    assert_eq!(harness.transport.call_count(), 0);
}

#[test]
fn every_transport_failure_kind_is_reported_with_its_own_wording() {
    for (error, expected) in [
        (
            TransportError::connect("dns"),
            "transport failure (connect)",
        ),
        (
            TransportError::timeout("30s"),
            "transport failure (timeout)",
        ),
        (TransportError::other("reset"), "transport failure (other)"),
    ] {
        let harness = harness(|config| {
            config.with_retry(RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::default()
            })
        });
        harness.transport.push_failure(error);
        let error = block_on(harness.client.whoami()).expect_err("failed");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn a_body_of_arbitrary_method_and_shape_never_panics_the_decoder() {
    let harness = harness(|config| config);
    for body in [
        "null",
        "[]",
        "\"string\"",
        "{\"event_id\":null}",
        "{\"event_id\":{\"nested\":true}}",
        "{\"event_id\":\"\"}",
        "\u{feff}{}",
    ] {
        harness.transport.push_json(200, body);
        let outcome = block_on(harness.client.send_text("!r:example.org", "Hallo"));
        assert!(outcome.is_err(), "{body} should not decode to an event id");
    }
}

#[test]
fn a_room_gate_is_released_after_a_failed_send() {
    // If the gate leaked, the second send would hang forever.
    let harness = harness(|config| {
        config.with_retry(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        })
    });
    harness
        .transport
        .push_json(403, r#"{"errcode":"M_FORBIDDEN"}"#);
    harness.transport.push_json(200, r#"{"event_id":"$ok"}"#);

    assert!(block_on(harness.client.send_text("!r:example.org", "a")).is_err());
    let outcome = block_on(harness.client.send_text("!r:example.org", "b")).expect("second send");
    assert_eq!(outcome.event_id, "$ok");
    assert_eq!(harness.transport.call_count(), 2);
}

#[test]
fn bodies_are_sent_as_bytes_without_a_json_round_trip_for_media() {
    // A body that is not valid UTF-8 must reach the transport unchanged.
    let harness = harness(|config| config);
    harness
        .transport
        .push_json(200, r#"{"content_uri":"mxc://example.org/abc"}"#);
    let bytes = [0xff_u8, 0xfe, 0x00, 0x80];
    block_on(
        harness
            .client
            .upload_media(&MediaUpload::new(&bytes, "application/octet-stream")),
    )
    .expect("uploaded");
    assert_eq!(requests(&harness)[0].body, bytes);
    assert_eq!(
        requests(&harness)[0].header("content-type"),
        Some("application/octet-stream")
    );
}
