//! Property tests for the outbound client's pure logic: transaction ids,
//! retry classification, backoff bounds and the "one event, not two" invariant.

#![cfg(feature = "matrix-out")]

use std::sync::Arc;
use std::time::Duration;

use proptest::prelude::*;

use messenger_webhook::matrix_out::transport::{Response, TransportError, TransportErrorKind};
use messenger_webhook::matrix_out::{
    ClientConfig, MatrixClient, MediaUpload, MxcUri, Operation, OutboundError, RecordingSleeper,
    RetryPolicy, StubTransport, TxnId, Verdict, MAX_MXC_URI_BYTES, MAX_TXN_ID_BYTES,
};

fn policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 4,
        base_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(1),
        max_rate_limit_wait: Duration::from_secs(60),
    }
}

proptest! {
    /// A transaction id is accepted exactly when it is a non-empty run of
    /// URL-unreserved bytes no longer than the ceiling — and what is accepted
    /// comes back byte for byte.
    #[test]
    fn txn_id_acceptance_is_exactly_the_documented_grammar(raw in "\\PC{0,400}") {
        let accepted = TxnId::new(raw.as_str()).is_ok();
        let expected = !raw.is_empty()
            && raw.len() <= MAX_TXN_ID_BYTES
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'));
        prop_assert_eq!(accepted, expected, "for {:?}", raw);
        if let Ok(txn) = TxnId::new(raw.as_str()) {
            prop_assert_eq!(txn.as_str(), raw.as_str());
            prop_assert_eq!(txn.to_string(), raw);
        }
    }

    /// Generated ids never collide and always increase, so a log sorted by id is
    /// sorted by time and a repeated id is always a repeated *send*.
    #[test]
    fn generated_txn_ids_are_unique_and_ascending(count in 2usize..40) {
        let ids: Vec<TxnId> = (0..count).map(|_| TxnId::generate()).collect();
        for pair in ids.windows(2) {
            prop_assert!(pair[0] < pair[1], "{} !< {}", pair[0], pair[1]);
        }
        let unique: std::collections::BTreeSet<&str> = ids.iter().map(TxnId::as_str).collect();
        prop_assert_eq!(unique.len(), ids.len(), "two ids collided");
        for id in &ids {
            prop_assert!(TxnId::new(id.as_str()).is_ok());
        }
    }

    /// The classification table is total: every status maps to exactly one
    /// verdict, and the three invariants that matter hold for all of them.
    #[test]
    fn classification_is_total_and_honours_the_write_rules(
        status in 0u16..=599,
        attempt in 1u32..=5,
        operation in prop_oneof![
            Just(Operation::Read),
            Just(Operation::Idempotent),
            Just(Operation::NonIdempotent),
        ],
        retry_after in proptest::option::of(0u64..120_000),
    ) {
        let body = match retry_after {
            Some(ms) => format!(r#"{{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":{ms}}}"#),
            None => String::from(r#"{"errcode":"M_LIMIT_EXCEEDED"}"#),
        };
        let response = Response::json(status, &body);
        let verdict = policy().classify(attempt, &response, operation);

        match status {
            200..=299 => prop_assert_eq!(verdict, Verdict::Success),
            429 => {
                // A rate limit is always answered before the request is
                // processed, so repetition is safe for every operation.
                prop_assert!(
                    matches!(verdict, Verdict::Retry { .. } | Verdict::Exhausted),
                    "{verdict:?} for 429"
                );
                if let Verdict::Retry { delay } = verdict {
                    let expected = retry_after.map_or_else(
                        || policy().backoff(attempt),
                        Duration::from_millis,
                    );
                    prop_assert_eq!(delay, expected);
                }
            }
            500..=599 if !operation.is_repeatable() => {
                prop_assert_eq!(verdict, Verdict::Ambiguous, "a 5xx on an upload must not retry");
            }
            500..=599 => prop_assert!(
                matches!(verdict, Verdict::Retry { .. } | Verdict::Exhausted),
                "{verdict:?} for {status}"
            ),
            _ => prop_assert_eq!(verdict, Verdict::Rejected),
        }
    }

    /// No non-idempotent request is ever retried after the request could have
    /// been written — that is the whole delivery contract.
    #[test]
    fn a_non_idempotent_request_is_never_retried_after_it_may_have_been_written(
        status in 500u16..=599,
        attempt in 1u32..=5,
    ) {
        prop_assert_eq!(
            policy().classify(attempt, &Response::json(status, "{}"), Operation::NonIdempotent),
            Verdict::Ambiguous
        );
    }

    /// A transport failure is retried exactly when nothing can have been written.
    #[test]
    fn transport_failures_follow_the_write_boundary(
        kind in prop_oneof![
            Just(TransportErrorKind::Connect),
            Just(TransportErrorKind::Timeout),
            Just(TransportErrorKind::Other),
        ],
        attempt in 1u32..=5,
        repeatable in any::<bool>(),
    ) {
        let operation = if repeatable {
            Operation::Idempotent
        } else {
            Operation::NonIdempotent
        };
        let error = TransportError { kind, message: "x".into() };
        let verdict = policy().classify_transport(attempt, &error, operation);
        match (kind.is_before_write(), repeatable) {
            (true, _) => prop_assert!(
                matches!(verdict, Verdict::Retry { .. } | Verdict::Exhausted),
                "{verdict:?}"
            ),
            (false, true) => prop_assert!(
                matches!(verdict, Verdict::Retry { .. } | Verdict::Exhausted),
                "{verdict:?}"
            ),
            (false, false) => prop_assert_eq!(verdict, Verdict::Ambiguous),
        }
    }

    /// Backoff is exponential, capped, monotone and never overflows.
    #[test]
    fn backoff_is_bounded_and_monotone(
        base_ms in 1u64..5_000,
        max_ms in 1u64..60_000,
        attempt in 1u32..=64,
    ) {
        let base = Duration::from_millis(base_ms);
        let max = Duration::from_millis(max_ms);
        let policy = RetryPolicy {
            max_attempts: 8,
            base_delay: base.min(max),
            max_delay: max,
            max_rate_limit_wait: max,
        };
        let delay = policy.backoff(attempt);
        prop_assert!(delay <= max, "{delay:?} over {max:?}");
        prop_assert!(delay >= policy.base_delay, "{delay:?} under {:?}", policy.base_delay);
        if attempt > 1 {
            prop_assert!(policy.backoff(attempt - 1) <= delay, "backoff must not shrink");
        }
        // Below the cap the growth is exactly doubling.
        let expected_exponent = attempt.saturating_sub(1).min(31);
        let doubling = policy
            .base_delay
            .checked_mul(1u32 << expected_exponent)
            .unwrap_or(max);
        prop_assert_eq!(delay, doubling.min(max));
    }

    /// An `mxc://` URI is accepted exactly when the two segments are non-empty
    /// and the value carries no whitespace or control characters.
    #[test]
    fn mxc_uris_are_validated_by_shape(raw in "\\PC{0,600}") {
        let parsed = MxcUri::parse(raw.as_str());
        let well_formed = raw.len() <= MAX_MXC_URI_BYTES
            && !raw.chars().any(|c| c.is_whitespace() || c.is_control())
            && raw.strip_prefix("mxc://").is_some_and(|rest| {
                rest.split_once('/')
                    .is_some_and(|(server, media)| !server.is_empty() && !media.is_empty())
            });
        prop_assert_eq!(parsed.is_ok(), well_formed, "for {:?}", raw);
        if let Ok(uri) = parsed {
            prop_assert_eq!(uri.as_str(), raw.as_str());
            prop_assert!(!uri.server_name().is_empty());
            prop_assert!(!uri.media_id().is_empty());
        }
    }

    /// However many times a send is rate-limited or answered with a `5xx`, every
    /// attempt carries the same transaction id: a retried send is one event.
    #[test]
    fn every_attempt_of_one_send_shares_one_transaction_id(failures in 0u8..3) {
        let transport = Arc::new(StubTransport::new());
        let sleeper = Arc::new(RecordingSleeper::new());
        let client = MatrixClient::new(
            transport.clone(),
            ClientConfig::new("https://m.example")
                .expect("valid")
                .with_sleeper(sleeper.clone()),
            "syt",
        )
        .expect("valid");
        for index in 0..failures {
            // Alternate the two retryable classes.
            if index % 2 == 0 {
                transport.push_json(502, r#"{"errcode":"M_UNKNOWN"}"#);
            } else {
                transport.push_json(429, r#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":1}"#);
            }
        }
        transport.push_json(200, r#"{"event_id":"$1"}"#);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let outcome = runtime
            .block_on(client.send_text("!r:m.example", "Hallo"))
            .expect("eventually sent");

        prop_assert_eq!(outcome.attempts, u32::from(failures) + 1);
        let calls = transport.requests();
        prop_assert_eq!(calls.len(), usize::from(failures) + 1);
        let first = &calls[0].url;
        for call in &calls {
            prop_assert_eq!(&call.url, first, "a retry must not mint a new id");
        }
    }

    /// A media upload is attempted at most once after a `5xx`, whatever the
    /// status and whatever the retry budget says.
    #[test]
    fn an_upload_after_a_5xx_is_attempted_exactly_once(
        status in 500u16..=599,
        max_attempts in 1u32..=5,
    ) {
        let transport = Arc::new(StubTransport::new());
        let client = MatrixClient::new(
            transport.clone(),
            ClientConfig::new("https://m.example")
                .expect("valid")
                .with_retry(RetryPolicy { max_attempts, ..RetryPolicy::default() }),
            "syt",
        )
        .expect("valid");
        for _ in 0..max_attempts {
            transport.push_json(status, "{}");
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let error = runtime
            .block_on(client.upload_media(&MediaUpload::new(b"x", "text/plain")))
            .expect_err("5xx");

        prop_assert!(matches!(error, OutboundError::Ambiguous { .. }), "{error:?}");
        prop_assert_eq!(transport.call_count(), 1);
    }

    /// The content ceiling is enforced at exactly N and N+1, never off by one.
    #[test]
    fn the_content_ceiling_is_exact(padding in 0usize..80) {
        let transport = Arc::new(StubTransport::new());
        let client = MatrixClient::new(
            transport.clone(),
            ClientConfig::new("https://m.example").expect("valid"),
            "syt",
        )
        .expect("valid");
        // `{"msgtype":"m.text","body":""}` is exactly 30 bytes of envelope, so
        // this body lands on the ceiling plus `padding`.
        let body = "x".repeat(65_536 - 30 + padding);
        transport.push_json(200, r#"{"event_id":"$1"}"#);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let outcome = runtime.block_on(client.send_text("!r:m.example", &body));

        let oversized = padding > 0;
        match outcome {
            Ok(_) => prop_assert!(!oversized, "{} bytes must not have been sent", body.len()),
            Err(error) => {
                prop_assert!(oversized, "{error}");
                prop_assert!(matches!(error, OutboundError::MessageTooLong { .. }), "{error}");
                prop_assert_eq!(transport.call_count(), 0);
            }
        }
    }

    /// A homeserver URL is accepted exactly when it carries an http(s) scheme,
    /// a host and no whitespace — and what is accepted loses only its trailing
    /// slash.
    #[test]
    fn homeserver_urls_are_normalised_by_rule(raw in "\\PC{0,120}") {
        let parsed = ClientConfig::new(raw.as_str());
        let trimmed = raw.trim();
        let lowercase = trimmed.to_ascii_lowercase();
        let scheme = lowercase.starts_with("http://") || lowercase.starts_with("https://");
        let clean = !trimmed.chars().any(|c| c.is_control() || c.is_whitespace());
        let base = trimmed.trim_end_matches('/');
        let host = !base.is_empty() && !base.ends_with(':');
        let expected = scheme && clean && host;
        prop_assert_eq!(parsed.is_ok(), expected, "for {:?}", raw);
        if let Ok(config) = parsed {
            prop_assert_eq!(config.homeserver(), base);
            prop_assert!(!config.homeserver().ends_with('/'));
        }
    }
}
