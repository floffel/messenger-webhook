#![no_main]
//! Nothing an unauthenticated query string sends may panic the handshake parser.
//!
//! The Meta subscription request is the first request an endpoint sees from
//! Meta, before any signed event body exists. The parser has to decode raw
//! query text, compare a caller-supplied secret and reflect only a bounded
//! decimal challenge, so this target checks that every accepted answer stays in
//! that class.

use libfuzzer_sys::fuzz_target;
use messenger_webhook::{subscription::meta_subscription_challenge, Channel, Secret};

const TOKEN: &str = "fuzz-secret-fuzz-secret";

fuzz_target!(|data: &[u8]| {
    let Ok(query) = std::str::from_utf8(data) else {
        return;
    };

    if let Ok(challenge) = meta_subscription_challenge(
        Channel::Messenger,
        &Secret::new(TOKEN).expect("constant secret"),
        query,
    ) {
        assert!(!challenge.is_empty());
        assert!(challenge.len() <= messenger_webhook::subscription::MAX_CHALLENGE_BYTES);
        assert!(challenge.bytes().all(|byte| byte.is_ascii_digit()));
        let parsed = challenge
            .parse::<u64>()
            .expect("accepted challenge is an unsigned integer");
        assert_eq!(challenge, parsed.to_string());
    }
});
