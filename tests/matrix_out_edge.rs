//! The edge matrix: every boundary the client claims to enforce, from both
//! sides — the just-barely-valid value and the one past it.

#![cfg(feature = "matrix-out")]

use std::sync::Arc;
use std::time::Duration;

use messenger_webhook::matrix_out::transport::{Response, TransportError};
use messenger_webhook::matrix_out::{
    ClientConfig, MatrixClient, MediaKind, MediaUpload, OutboundError, RecordingSleeper,
    RetryPolicy, StubTransport, TxnId, MAX_CONTENT_BYTES, MAX_EVENT_TYPE_BYTES, MAX_ID_BYTES,
    MAX_TXN_ID_BYTES,
};

fn client_with(
    configure: impl FnOnce(ClientConfig) -> ClientConfig,
) -> (MatrixClient, Arc<StubTransport>, Arc<RecordingSleeper>) {
    let transport = Arc::new(StubTransport::new());
    let sleeper = Arc::new(RecordingSleeper::new());
    let config = configure(
        ClientConfig::new("https://m.example")
            .expect("valid")
            .with_user_id("@bot:m.example")
            .with_sleeper(sleeper.clone()),
    );
    let client = MatrixClient::new(transport.clone(), config, "syt").expect("valid client");
    (client, transport, sleeper)
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

#[test]
fn transaction_ids_at_and_past_the_ceiling() {
    let exact = "t".repeat(MAX_TXN_ID_BYTES);
    assert!(TxnId::new(exact.clone()).is_ok());
    let over = "t".repeat(MAX_TXN_ID_BYTES + 1);
    let error = TxnId::new(over).expect_err("one byte too long");
    assert!(matches!(error, OutboundError::InvalidTxnId { .. }));
    assert!(error.to_string().contains("256"), "{error}");
}

#[test]
fn transaction_ids_reject_every_byte_a_url_segment_cannot_carry() {
    for bad in [
        " ", "\t", "\n", "/", "?", "#", "%", "&", "+", "=", "\\", "ü", "😀", "\u{0}",
    ] {
        assert!(TxnId::new(bad).is_err(), "{bad:?} should be rejected");
    }
    for good in ["a", "A0", "x-y_z.9~", "mw-0000000000000001-abc"] {
        assert!(TxnId::new(good).is_ok(), "{good:?} should be accepted");
    }
}

#[test]
fn room_ids_at_and_past_the_ceiling() {
    let (client, transport, _) = client_with(|config| config);
    transport.push_json(200, r#"{"event_id":"$1"}"#);

    let prefix = "!r:";
    let exact = format!("{prefix}{}", "a".repeat(MAX_ID_BYTES - prefix.len()));
    assert_eq!(exact.len(), MAX_ID_BYTES);
    assert!(block_on(client.send_text(&exact, "x")).is_ok());

    let over = format!("{prefix}{}", "a".repeat(MAX_ID_BYTES - prefix.len() + 1));
    let error = block_on(client.send_text(&over, "x")).expect_err("one byte too long");
    assert!(matches!(error, OutboundError::InvalidRequest { .. }));
    assert!(error.to_string().contains("256"), "{error}");
}

#[test]
fn an_alias_is_not_a_room_id_and_a_unicode_room_id_is_encoded() {
    let (client, transport, _) = client_with(|config| config);
    transport.push_json(200, r#"{"event_id":"$1"}"#);

    let error = block_on(client.send_text("#alias:m.example", "x")).expect_err("an alias");
    assert!(error.to_string().contains("resolve a `#alias`"), "{error}");

    assert!(block_on(client.send_text("!Räum:m.example", "x")).is_ok());
    assert!(transport
        .last_request()
        .expect("sent")
        .url
        .contains("%21R%C3%A4um%3Am.example"));
}

#[test]
fn event_types_at_and_past_the_ceiling() {
    let (client, transport, _) = client_with(|config| config);
    transport
        .push_json(200, r#"{"event_id":"$1"}"#)
        .push_json(200, r#"{"event_id":"$2"}"#);

    let exact = "t".repeat(MAX_EVENT_TYPE_BYTES);
    assert!(block_on(client.send_event("!r:m.example", &exact, serde_json::json!({}))).is_ok());

    let over = "t".repeat(MAX_EVENT_TYPE_BYTES + 1);
    let error = block_on(client.send_event("!r:m.example", &over, serde_json::json!({})))
        .expect_err("one byte too long");
    assert!(error.to_string().contains("256"), "{error}");
}

#[test]
fn a_body_of_one_byte_is_sent_and_a_body_of_none_is_not() {
    let (client, transport, _) = client_with(|config| config);
    transport.push_json(200, r#"{"event_id":"$1"}"#);

    assert!(block_on(client.send_text("!r:m.example", "x")).is_ok());
    let error = block_on(client.send_text("!r:m.example", "")).expect_err("empty body");
    assert!(error.to_string().contains("body is empty"), "{error}");
    assert_eq!(transport.call_count(), 1);
}

#[test]
fn text_carries_nul_newlines_and_surrogateless_unicode_intact() {
    let (client, transport, _) = client_with(|config| config);
    transport.push_json(200, r#"{"event_id":"$1"}"#);
    let body = "a\u{0}b\nline\ttab 😀 \"quoted\" \\";

    block_on(client.send_text("!r:m.example", body)).expect("sent");
    let sent = transport.last_request().expect("sent");
    assert_eq!(sent.json().expect("json")["body"], body);
}

#[test]
fn media_size_boundaries_live_exactly_at_the_configured_ceiling() {
    let (client, transport, _) = client_with(|config| config.with_max_upload_bytes(3));
    transport.push_json(200, r#"{"content_uri":"mxc://m.example/a"}"#);

    let at_limit = [1_u8, 2, 3];
    assert!(block_on(client.upload_media(&MediaUpload::new(&at_limit, "text/plain"))).is_ok());

    let over = [1_u8, 2, 3, 4];
    let error = block_on(client.upload_media(&MediaUpload::new(&over, "text/plain")))
        .expect_err("one byte over");
    assert!(matches!(
        error,
        OutboundError::MediaTooLarge {
            limit: 3,
            actual: 4
        }
    ));

    let empty = block_on(client.upload_media(&MediaUpload::new(&[], "text/plain")))
        .expect_err("empty upload");
    assert!(empty.to_string().contains("empty"), "{empty}");
    assert_eq!(transport.call_count(), 1);
}

#[test]
fn media_metadata_that_cannot_go_into_a_header_or_a_query_is_refused() {
    let (client, transport, _) = client_with(|config| config);
    transport
        .push_json(200, r#"{"content_uri":"mxc://m.example/a"}"#)
        .push_json(200, r#"{"content_uri":"mxc://m.example/b"}"#);

    for bad in [
        MediaUpload::new(b"x", ""),
        MediaUpload::new(b"x", "text"),
        MediaUpload::new(b"x", "text/plain\r\nx-injected: 1"),
        MediaUpload::new(b"x", "text/plain "),
    ] {
        let error = block_on(client.upload_media(&bad)).expect_err("unusable content type");
        assert!(error.is_client_error(), "{error:?}");
    }

    let name = "n".repeat(255);
    assert!(block_on(
        client.upload_media(&MediaUpload::new(b"x", "text/plain").with_filename(&name))
    )
    .is_ok());
    let too_long = "n".repeat(256);
    let error = block_on(
        client.upload_media(&MediaUpload::new(b"x", "text/plain").with_filename(&too_long)),
    )
    .expect_err("name over 255 bytes");
    assert!(error.to_string().contains("255"), "{error}");
    assert_eq!(transport.call_count(), 1);
}

#[test]
fn every_media_kind_maps_to_its_msgtype_in_a_sent_message() {
    for (kind, msgtype) in [
        (MediaKind::Image, "m.image"),
        (MediaKind::Video, "m.video"),
        (MediaKind::Audio, "m.audio"),
        (MediaKind::File, "m.file"),
    ] {
        let (client, transport, _) = client_with(|config| config);
        transport
            .push_json(200, r#"{"content_uri":"mxc://m.example/a"}"#)
            .push_json(200, r#"{"event_id":"$1"}"#);
        let upload = MediaUpload::new(b"x", "application/octet-stream").with_kind(kind);
        block_on(client.send_media("!r:m.example", &upload, None)).expect("sent");
        assert_eq!(
            transport
                .last_request()
                .expect("sent")
                .json()
                .expect("json")["msgtype"],
            msgtype
        );
    }
}

#[test]
fn a_429_without_a_hint_with_garbage_and_with_a_zero_hint() {
    let (client, transport, sleeper) = client_with(|config| config);

    // (body, expected delay)
    for (body, expected) in [
        (
            r#"{"errcode":"M_LIMIT_EXCEEDED"}"#,
            Duration::from_millis(500),
        ),
        ("total garbage", Duration::from_millis(500)),
        ("", Duration::from_millis(500)),
        (r#"{"retry_after_ms":0}"#, Duration::ZERO),
        (r#"{"retry_after_ms":"7"}"#, Duration::from_millis(7)),
        (r#"{"retry_after_ms":-5}"#, Duration::from_millis(500)),
        (r#"{"retry_after_ms":null}"#, Duration::from_millis(500)),
    ] {
        transport
            .push_json(429, body)
            .push_json(200, r#"{"event_id":"$1"}"#);
        block_on(client.send_text("!r:m.example", "x")).expect("second attempt lands");
        let waited = *sleeper.delays().last().expect("a wait");
        assert_eq!(waited, expected, "for body {body:?}");
    }
}

#[test]
fn a_zero_rate_limit_budget_still_allows_a_zero_wait() {
    let (client, transport, sleeper) = client_with(|config| {
        config.with_retry(RetryPolicy {
            max_rate_limit_wait: Duration::ZERO,
            ..RetryPolicy::default()
        })
    });
    transport
        .push_json(429, r#"{"retry_after_ms":0}"#)
        .push_json(200, r#"{"event_id":"$1"}"#);

    block_on(client.send_text("!r:m.example", "x")).expect("sent");
    assert_eq!(sleeper.delays(), vec![Duration::ZERO]);

    // Any positive hint is over a zero budget.
    transport.push_json(429, r#"{"retry_after_ms":1}"#);
    let error = block_on(client.send_text("!r:m.example", "x")).expect_err("limited");
    assert!(
        matches!(error, OutboundError::RateLimited { .. }),
        "{error:?}"
    );
}

#[test]
fn an_absurd_rate_limit_is_reported_not_waited_out() {
    let (client, transport, sleeper) = client_with(|config| config);
    transport.push_json(429, r#"{"retry_after_ms":3600000}"#);

    let error = block_on(client.send_text("!r:m.example", "x")).expect_err("limited");
    assert!(error.is_retryable());
    assert_eq!(
        match error {
            OutboundError::RateLimited { retry_after, .. } => retry_after,
            other => panic!("expected a rate limit, got {other:?}"),
        },
        Some(Duration::from_secs(3_600))
    );
    assert_eq!(sleeper.delays(), [] as [std::time::Duration; 0]);
}

#[test]
fn a_retry_after_header_is_honoured_when_the_body_is_silent() {
    let (client, transport, sleeper) = client_with(|config| config);
    transport
        .push(Response::new(429, Vec::new()).with_header("retry-after", "2"))
        .push_json(200, r#"{"event_id":"$1"}"#);

    block_on(client.send_text("!r:m.example", "x")).expect("sent");
    assert_eq!(sleeper.delays(), vec![Duration::from_secs(2)]);
}

#[test]
fn every_non_2xx_non_429_status_is_classified_once() {
    // (status, retried?, final variant matcher)
    for status in [
        100_u16, 199, 300, 301, 304, 400, 401, 404, 409, 413, 418, 451, 600, 999,
    ] {
        let (client, transport, _) = client_with(|config| config);
        transport.push_json(status, r#"{"errcode":"M_UNKNOWN","error":"nope"}"#);
        let error = block_on(client.send_text("!r:m.example", "x")).expect_err("rejected");
        assert!(
            matches!(error, OutboundError::Rejected { status: got, .. } if got == status),
            "{status}: {error:?}"
        );
        assert_eq!(transport.call_count(), 1, "{status} must not be retried");
    }
}

#[test]
fn a_599_is_retried_but_a_600_is_not() {
    let (client, transport, _) = client_with(|config| {
        config.with_retry(RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        })
    });
    transport
        .push_json(599, "{}")
        .push_json(200, r#"{"event_id":"$1"}"#);
    assert!(block_on(client.send_text("!r:m.example", "x")).is_ok());
    assert_eq!(transport.call_count(), 2);

    let (client, transport, _) = client_with(|config| config);
    transport.push_json(600, "{}");
    assert!(block_on(client.send_text("!r:m.example", "x")).is_err());
    assert_eq!(transport.call_count(), 1);
}

#[test]
fn a_single_attempt_policy_never_retries_anything() {
    let (client, transport, sleeper) = client_with(|config| {
        config.with_retry(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        })
    });
    for (status, body) in [(503, "{}"), (429, r#"{"retry_after_ms":1}"#)] {
        // Exactly one scripted answer: a second attempt would run out of script
        // and fail with a transport error instead of the status under test.
        transport.push_json(status, body);
        let error = block_on(client.send_text("!r:m.example", "x")).expect_err("no retry");
        assert!(error.is_retryable() || error.is_ambiguous(), "{error:?}");
        assert_eq!(sleeper.delays(), [] as [std::time::Duration; 0]);
        assert_eq!(
            transport.remaining(),
            0,
            "the single answer was consumed once"
        );
    }
    assert_eq!(transport.call_count(), 2, "one call per operation, no more");
}

#[test]
fn a_zero_length_body_on_a_typing_leg_is_fine() {
    let (client, transport, _) = client_with(|config| config);
    transport.push_status(204);
    block_on(client.stop_typing("!r:m.example")).expect("cleared");
    assert_eq!(transport.call_count(), 1);
}

#[test]
fn the_error_body_is_read_from_the_raw_bytes_when_it_is_not_json() {
    let (client, transport, _) = client_with(|config| {
        config.with_retry(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        })
    });
    transport.push(Response::new(
        502,
        b"<html><body>bad gateway</body></html>".to_vec(),
    ));

    let error = block_on(client.whoami()).expect_err("gateway");
    let text = error.to_string();
    assert!(text.contains("bad gateway"), "{text}");
    assert!(text.contains("M_UNKNOWN"), "{text}");
}

#[test]
fn a_hostile_error_body_cannot_flood_a_log_line() {
    let (client, transport, _) = client_with(|config| config);
    transport.push(Response::new(400, vec![b'A'; 100_000]));

    let error = block_on(client.whoami()).expect_err("bad request");
    let text = error.to_string();
    assert!(text.len() < 500, "{} bytes of error text", text.len());
}

#[test]
fn an_upload_answer_that_is_not_an_mxc_uri_is_an_ambiguous_write() {
    // The bytes were stored (2xx) but the client cannot name them: the upload
    // happened and the caller has no URI, which is exactly "unknown outcome".
    let (client, transport, _) = client_with(|config| config);
    for body in [
        r#"{"content_uri":"https://m.example/a"}"#,
        r#"{"content_uri":""}"#,
        r#"{"content_uri":42}"#,
        r"{}",
    ] {
        transport.push_json(200, body);
        let error = block_on(client.upload_media(&MediaUpload::new(b"x", "text/plain")))
            .expect_err("undecodable");
        assert!(error.is_ambiguous(), "{body}: {error:?}");
        assert!(
            !error.is_retryable(),
            "{body}: the bytes may be stored already"
        );
    }
}

#[test]
fn the_probe_tolerates_a_snake_or_camel_user_id_and_nothing_else() {
    let (client, transport, _) = client_with(|config| config);
    for (body, expected) in [
        (r#"{"user_id":"@a:m"}"#, Some("@a:m")),
        (r#"{"userId":"@b:m"}"#, Some("@b:m")),
        (r#"{"user_id":""}"#, None),
        (r#"{"user_id":null}"#, None),
        (r#"{"user_id":7}"#, None),
        (r"[]", None),
        (r"null", None),
    ] {
        transport.push_json(200, body);
        let result = block_on(client.whoami());
        assert_eq!(result.as_deref().ok(), expected, "for {body}");
    }
}

#[test]
fn a_trailing_slash_on_the_homeserver_never_doubles_in_a_path() {
    let (_client, transport, _) = client_with(|config| config);
    let client = MatrixClient::new(
        transport.clone(),
        ClientConfig::new("https://m.example///").expect("valid"),
        "syt",
    )
    .expect("valid");
    transport.push_json(200, r#"{"event_id":"$1"}"#);

    block_on(client.send_text("!r:m.example", "x")).expect("sent");
    let url = transport.last_request().expect("sent").url;
    assert!(!url.contains("///"), "{url}");
    assert!(url.starts_with("https://m.example/_matrix/client/v3/rooms/"));
}

#[test]
fn a_connect_failure_on_a_read_exhausts_into_a_retryable_transport_error() {
    let (client, transport, _) = client_with(|config| {
        config.with_retry(RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        })
    });
    transport
        .push_failure(TransportError::connect("dns"))
        .push_failure(TransportError::connect("dns"));
    let error = block_on(client.whoami()).expect_err("dns");
    assert!(matches!(
        error,
        OutboundError::Transport { attempts: 2, .. }
    ));
    assert!(error.is_retryable());
}

#[test]
fn content_at_the_ceiling_is_accepted_and_one_byte_past_is_not() {
    let (client, transport, _) = client_with(|config| config);
    transport
        .push_json(200, r#"{"event_id":"$1"}"#)
        .push_json(200, r#"{"event_id":"$2"}"#);

    // `{"msgtype":"m.text","body":""}` is a 30 byte envelope.
    let at_limit = "x".repeat(MAX_CONTENT_BYTES - 30);
    assert!(block_on(client.send_text("!r:m.example", &at_limit)).is_ok());

    let over = "x".repeat(MAX_CONTENT_BYTES - 29);
    let error = block_on(client.send_text("!r:m.example", &over)).expect_err("over the ceiling");
    assert!(
        matches!(error, OutboundError::MessageTooLong { .. }),
        "{error:?}"
    );
    assert_eq!(transport.call_count(), 1);
}

#[test]
fn a_control_character_in_an_event_id_or_room_id_is_refused() {
    let (client, transport, _) = client_with(|config| config);
    for room in ["!r\nm.example", "!r m.example", "!r\tm.example"] {
        assert!(block_on(client.send_text(room, "x")).is_err(), "{room:?}");
    }
    assert!(block_on(client.send_read_receipt("!r:m.example", "$a\u{0}b")).is_err());
    assert_eq!(transport.call_count(), 0);
}
