#![no_main]

//! Fuzzes the outbound client's public validators.
//!
//! Everything a caller hands the Matrix client is a string that has to pass a
//! grammar before it becomes a URL: transaction ids, `mxc://` URIs and the
//! homeserver base. The room/event-id validators sit behind async methods, and
//! the response decoders are private (covered by the property suite with
//! `StubTransport` feeding them arbitrary bodies), so this target covers the
//! half a stranger can reach without a runtime.
//!
//! Invariants: nothing panics, an accepted value round-trips unchanged, an
//! accepted transaction id needs no percent-encoding, and an accepted homeserver
//! base never ends in a slash.

use libfuzzer_sys::fuzz_target;
use messenger_webhook::matrix_out::{ClientConfig, MxcUri, TxnId};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // 1. Transaction ids: accepted ids are already URL-safe, so the client can
    //    paste one into a path segment without changing it.
    if let Ok(txn) = TxnId::new(text) {
        assert_eq!(txn.as_str(), text);
        assert_eq!(txn.to_string(), text);
        assert!(
            txn.as_str().bytes().all(|byte| byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'_' | b'.' | b'~')),
            "{text:?} was accepted but is not a URL-unreserved segment"
        );
    }

    // 2. Content URIs: both halves of an accepted `mxc://` URI are non-empty.
    if let Ok(uri) = MxcUri::parse(text) {
        assert_eq!(uri.as_str(), text);
        assert!(!uri.server_name().is_empty());
        assert!(!uri.media_id().is_empty());
    }

    // 3. Homeserver bases: an accepted base has a scheme and no trailing slash,
    //    so no path can ever double up.
    if let Ok(config) = ClientConfig::new(text) {
        assert!(!config.homeserver().ends_with('/'));
        assert!(
            config.homeserver().to_ascii_lowercase().starts_with("http"),
            "{text:?} was accepted without an http scheme"
        );
    }
});
