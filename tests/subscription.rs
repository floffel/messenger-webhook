//! The Meta subscription handshake: what is accepted, what fails closed.
//!
//! The contract is the sample request from Meta's *Getting Started* page
//! (`hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=meatyhamhock`),
//! quoted in `docs/contracts.md`. Every case below is one side of a boundary:
//! a matching token and a token that differs in one byte, a challenge of exactly
//! the integer boundary and one past it, a parameter that appears once, one
//! whose repeated value is identical, and one whose repeated values conflict.

#![cfg(feature = "whatsapp")]

use std::collections::BTreeMap;

use messenger_webhook::subscription::{
    meta_subscription_challenge, MAX_CHALLENGE_BYTES, META_CHALLENGE_PARAM, META_MODE_PARAM,
    META_SUBSCRIBE_MODE, META_VERIFY_TOKEN_PARAM,
};
use messenger_webhook::{Channel, Error, Secret};
use proptest::prelude::*;

/// Meta's own sample request, byte for byte.
const DOCS_QUERY: &str =
    "hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=meatyhamhock-meatyhamhock";

/// The token from the sample request.
const TOKEN: &str = "meatyhamhock-meatyhamhock";

/// Builds a query string from pairs, percent-encoding as Meta would.
fn query(pairs: &[(&str, &str)]) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

/// A valid request with one parameter replaced.
fn request_with(key: &str, value: &str) -> String {
    let mut pairs: BTreeMap<&str, &str> = BTreeMap::from([
        (META_MODE_PARAM, META_SUBSCRIBE_MODE),
        (META_CHALLENGE_PARAM, "1158201444"),
        (META_VERIFY_TOKEN_PARAM, TOKEN),
    ]);
    pairs.insert(key, value);
    query(&pairs.into_iter().collect::<Vec<_>>())
}

#[test]
fn the_documented_request_returns_the_challenge() {
    let challenge = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), DOCS_QUERY)
        .expect("the documented request is accepted");
    assert_eq!(challenge, "1158201444");
}

#[test]
fn a_wrong_token_is_a_bad_signature_on_the_whatsapp_channel() {
    let query = request_with(META_VERIFY_TOKEN_PARAM, "wrong-secret-value");
    let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
        .expect_err("mismatch");
    assert!(
        matches!(
            error,
            Error::BadSignature {
                channel: Channel::WhatsApp
            }
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn a_token_that_shares_the_whole_prefix_is_still_rejected() {
    // Same length, same prefix, one byte different: the comparison runs over
    // the whole value instead of stopping at the first difference.
    let almost = format!("{}X", &TOKEN[..TOKEN.len() - 1]);
    let query = request_with(META_VERIFY_TOKEN_PARAM, &almost);
    assert!(meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query).is_err());
}

#[test]
fn a_token_that_is_a_prefix_of_the_secret_is_rejected() {
    let query = request_with(META_VERIFY_TOKEN_PARAM, &TOKEN[..TOKEN.len() - 1]);
    let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
        .expect_err("short token");
    assert!(matches!(error, Error::BadSignature { .. }));
}

#[test]
fn an_empty_secret_fails_closed_even_for_a_perfect_request() {
    // An unset dashboard token cannot even be constructed.
    assert!(matches!(
        Secret::new(""),
        Err(Error::WeakSecret {
            actual: 0,
            minimum: messenger_webhook::secret::MIN_SECRET_BYTES,
        })
    ));
}

#[test]
fn the_empty_secret_is_rejected_before_the_query_is_parsed() {
    // The constructor rejects the credential before any query is parsed.
    assert!(matches!(
        Secret::new(""),
        Err(Error::WeakSecret { actual: 0, .. })
    ));
}

#[test]
fn every_parameter_is_required() {
    for missing in [
        META_MODE_PARAM,
        META_CHALLENGE_PARAM,
        META_VERIFY_TOKEN_PARAM,
    ] {
        let pairs: Vec<(&str, &str)> = [
            (META_MODE_PARAM, META_SUBSCRIBE_MODE),
            (META_CHALLENGE_PARAM, "1158201444"),
            (META_VERIFY_TOKEN_PARAM, TOKEN),
        ]
        .into_iter()
        .filter(|(key, _)| *key != missing)
        .collect();
        let error =
            meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query(&pairs))
                .expect_err("a missing parameter is rejected");
        assert!(
            matches!(error, Error::MalformedVerificationRequest { .. }),
            "{missing} missing gave {error}"
        );
        assert!(
            error.to_string().contains(missing),
            "{missing} should be named in {error}"
        );
    }
}

#[test]
fn mode_must_be_subscribe() {
    for value in ["unsubscribe", "Subscribe!", "sub", ""] {
        let query = request_with(META_MODE_PARAM, value);
        let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect_err("a wrong mode is rejected");
        assert!(
            matches!(error, Error::MalformedVerificationRequest { .. }),
            "mode {value:?} gave {error}"
        );
    }
}

#[test]
fn a_trailing_space_in_the_mode_is_not_accepted() {
    // `+` decodes to a space, so this is the classic copy-paste slip.
    let query = format!("hub.mode=subscribe+&hub.challenge=1&hub.verify_token={TOKEN}");
    assert!(meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query).is_err());
}

#[test]
fn the_mode_is_compared_case_insensitively() {
    let query = request_with(META_MODE_PARAM, "SUBSCRIBE");
    let challenge = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
        .expect("Meta's value in any case is the same value");
    assert_eq!(challenge, "1158201444");
}

#[test]
fn a_conflicting_duplicate_parameter_is_rejected_even_when_one_value_matches() {
    // Two tokens would be ambiguous: one of them is the attacker's.
    let query = format!(
        "hub.mode=subscribe&hub.challenge=1&hub.verify_token={TOKEN}&hub.verify_token=other"
    );
    let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
        .expect_err("ambiguity rejected");
    assert!(
        matches!(error, Error::MalformedVerificationRequest { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn identical_duplicate_parameters_are_accepted() {
    // Repeating the same decoded value for one logical parameter is
    // unambiguous, including the issue's `hub.mode=subscribe` twice.
    let query = query(&[
        (META_MODE_PARAM, META_SUBSCRIBE_MODE),
        (META_MODE_PARAM, META_SUBSCRIBE_MODE),
        (META_CHALLENGE_PARAM, "42"),
        (META_CHALLENGE_PARAM, "42"),
        (META_VERIFY_TOKEN_PARAM, TOKEN),
        (META_VERIFY_TOKEN_PARAM, TOKEN),
    ]);
    assert_eq!(
        meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect("identical duplicates are accepted"),
        "42"
    );
}

#[test]
fn equal_php_aliases_are_accepted_as_one_parameter() {
    // A request that passes through a layer that rewrites `.` to `_` may carry
    // both spellings; equal decoded values are one logical parameter.
    let query = format!(
        "hub.mode=subscribe&hub_mode=subscribe&hub.challenge=42&hub_challenge=42&hub.verify_token={TOKEN}&hub_verify_token={TOKEN}"
    );
    assert_eq!(
        meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect("equal aliases are accepted"),
        "42"
    );
}

#[test]
fn conflicting_php_aliases_are_rejected() {
    for query in [
        "hub.mode=subscribe&hub_mode=unsubscribe&hub.challenge=42".to_owned(),
        format!("hub.mode=subscribe&hub.challenge=42&hub_challenge=43&hub.verify_token={TOKEN}"),
        format!(
            "hub.mode=subscribe&hub.challenge=42&hub.verify_token={TOKEN}&hub_verify_token=other"
        ),
    ] {
        let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect_err("conflicting aliases are rejected");
        assert!(
            matches!(error, Error::MalformedVerificationRequest { .. }),
            "unexpected error for {query}: {error}"
        );
    }
}

#[test]
fn a_duplicate_of_an_ignored_parameter_is_fine() {
    // Only the parameters this crate reads are checked for ambiguity.
    let query = format!("{DOCS_QUERY}&hub.debug=1&hub.debug=2");
    assert_eq!(
        meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect("accepted"),
        "1158201444"
    );
}

#[test]
fn php_style_parameter_names_are_accepted() {
    // Meta's page notes that PHP turns `.` into `_` in parameter names, so an
    // endpoint behind a PHP layer sees these spellings.
    let query = "hub_mode=subscribe&hub_challenge=99&hub_verify_token=meatyhamhock-meatyhamhock";
    assert_eq!(
        meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), query).expect("accepted"),
        "99"
    );
}

#[test]
fn unknown_parameters_are_ignored() {
    let query = format!("{DOCS_QUERY}&hub.mode_extra=1&unrelated=value");
    assert_eq!(
        meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect("accepted"),
        "1158201444"
    );
}

#[test]
fn percent_encoded_values_are_compared_after_decoding() {
    // A dashboard token may contain characters the query has to escape.
    let secret = Secret::new("a b&c=d/ü padded-pad").expect("secret");
    let query = query(&[
        (META_MODE_PARAM, META_SUBSCRIBE_MODE),
        (META_CHALLENGE_PARAM, "7"),
        (META_VERIFY_TOKEN_PARAM, "a b&c=d/ü padded-pad"),
    ]);
    assert_eq!(
        meta_subscription_challenge(&secret, &query).expect("accepted"),
        "7"
    );
}

#[test]
fn a_plus_in_the_token_is_a_space() {
    // `application/x-www-form-urlencoded`: `+` is a space, so a secret containing
    // a space arrives as `+`.
    let query = "hub.mode=subscribe&hub.challenge=1&hub.verify_token=space+in+secret+padded";
    assert_eq!(
        meta_subscription_challenge(
            &Secret::new("space in secret padded").expect("secret"),
            query
        )
        .expect("accepted"),
        "1"
    );
}

#[test]
fn an_encoded_token_matches_the_plain_secret() {
    // Encoding is an application detail; the compared values are the decoded ones.
    let query = "hub.mode=subscribe&hub.challenge=1&hub.verify_token=meaty%68amhock-meatyhamhock";
    assert_eq!(
        meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), query).expect("accepted"),
        "1"
    );
}

#[test]
fn the_challenge_must_be_an_unsigned_integer() {
    // The reflected body must be digits only: keeping the query value verbatim
    // would reintroduce markup/header-reflection risk for handlers that use it
    // somewhere other than the documented text/plain response body.
    for value in [
        "<script>alert(1)</script>",
        "\"quoted\"",
        "angle< brackets>",
        "back\\slash",
        "a\r\nX: y",
        "line\nbreak",
        "nul\0byte",
        "tab\there",
        "ümlaut",
        " ",
        "-1",
        "1.5",
        "1e3",
        "0x10",
    ] {
        let query = request_with(META_CHALLENGE_PARAM, value);
        let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect_err("a non-integer challenge is rejected");
        assert!(
            matches!(error, Error::MalformedVerificationRequest { .. }),
            "challenge {value:?} gave {error}"
        );
    }
}

#[test]
fn the_challenge_is_returned_as_canonical_decimal() {
    let query = request_with(META_CHALLENGE_PARAM, "0001158201444");
    let challenge = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
        .expect("leading zeroes are still an integer");
    assert_eq!(challenge, "1158201444");
    assert!(challenge.bytes().all(|byte| byte.is_ascii_digit()));
}

#[test]
fn an_empty_challenge_is_rejected() {
    let query = request_with(META_CHALLENGE_PARAM, "");
    assert!(meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query).is_err());
}

#[test]
fn the_challenge_length_is_a_pre_parse_guard() {
    let at_limit = "0".repeat(MAX_CHALLENGE_BYTES);
    let query = request_with(META_CHALLENGE_PARAM, &at_limit);
    assert_eq!(
        meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect("at the ceiling"),
        "0"
    );

    let past_limit = "1".repeat(MAX_CHALLENGE_BYTES + 1);
    let query = request_with(META_CHALLENGE_PARAM, &past_limit);
    let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
        .expect_err("one byte past the ceiling");
    assert!(matches!(error, Error::MalformedVerificationRequest { .. }));
}

#[test]
fn the_challenge_integer_range_is_exact() {
    let max = u64::MAX.to_string();
    let query = request_with(META_CHALLENGE_PARAM, &max);
    assert_eq!(
        meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
            .expect("u64::MAX fits"),
        max
    );

    let past_max = (u128::from(u64::MAX) + 1).to_string();
    let query = request_with(META_CHALLENGE_PARAM, &past_max);
    let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
        .expect_err("one past u64::MAX is rejected");
    assert!(matches!(error, Error::MalformedVerificationRequest { .. }));
}

#[test]
fn the_error_never_echoes_the_received_token() {
    // A log line must not contain what the attacker sent.
    let query = request_with(META_VERIFY_TOKEN_PARAM, "attacker-supplied-token");
    let error = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query)
        .expect_err("mismatch");
    let rendered = error.to_string();
    assert!(
        !rendered.contains("attacker-supplied-token"),
        "the token leaked into {rendered}"
    );
    assert!(!format!("{error:?}").contains("attacker-supplied-token"));
}

#[test]
fn the_malformed_error_is_terminal_and_reads_sensibly() {
    let error = Error::MalformedVerificationRequest {
        channel: Channel::WhatsApp,
        reason: "no hub.challenge parameter".to_owned(),
    };
    assert_eq!(
        error.to_string(),
        "whatsapp subscription request is malformed: no hub.challenge parameter"
    );
    assert!(!error.is_retryable());
}

#[test]
fn subscription_errors_distinguish_rejection_from_misconfiguration() {
    let secret = Secret::new(TOKEN).expect("secret");
    let malformed = meta_subscription_challenge(&secret, "hub.mode=subscribe&hub.challenge=1")
        .expect_err("missing verify token");
    assert!(matches!(
        malformed,
        Error::MalformedVerificationRequest { .. }
    ));
    assert!(!malformed.is_misconfiguration());

    let wrong_token = request_with(META_VERIFY_TOKEN_PARAM, "wrong-secret-value");
    let bad_signature = meta_subscription_challenge(&secret, &wrong_token).expect_err("mismatch");
    assert!(matches!(bad_signature, Error::BadSignature { .. }));
    assert!(!bad_signature.is_misconfiguration());

    let missing_secret = Error::MissingSecret {
        channel: Channel::WhatsApp,
    };
    assert!(missing_secret.is_misconfiguration());
}

proptest! {
    /// The handshake succeeds exactly when the token matches, and the answer is
    /// the challenge in canonical decimal — never a value derived from anything
    /// else.
    #[test]
    fn the_challenge_is_returned_exactly_when_the_token_matches(
        secret in "[!-~]{16,32}",
        candidate in "[!-~]{1,32}",
        challenge in "[0-9]{1,12}",
    ) {
        let query = query(&[
            (META_MODE_PARAM, META_SUBSCRIBE_MODE),
            (META_CHALLENGE_PARAM, &challenge),
            (META_VERIFY_TOKEN_PARAM, &candidate),
        ]);
        let result = meta_subscription_challenge(
            &Secret::new(secret.clone()).expect("secret"),
            &query,
        );
        if candidate == secret {
            let canonical = challenge
                .parse::<u64>()
                .expect("the proptest strategy generates a u64-sized decimal")
                .to_string();
            prop_assert_eq!(result.expect("matching token"), canonical);
        } else {
            // Bound first: `prop_assert!` renders its argument as a format
            // string, and the `{ .. }` pattern would break it.
            let rejected_as_bad_signature = matches!(result, Err(Error::BadSignature { .. }));
            prop_assert!(rejected_as_bad_signature);
        }
    }

    /// An arbitrary query string is answered, never a panic.
    #[test]
    fn arbitrary_query_strings_never_panic(query in "\\PC{0,200}") {
        let _ = meta_subscription_challenge(&Secret::new(TOKEN).expect("secret"), &query);
    }
}
