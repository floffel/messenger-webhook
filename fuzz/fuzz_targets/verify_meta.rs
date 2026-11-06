#![no_main]
//! Fuzzes the attacker-controlled Meta signature parser.
//!
//! The header is a string supplied by the caller before any authentication has
//! happened. This target feeds arbitrary UTF-8 text into the public verifier
//! with both an empty body and the raw fuzzer bytes as the body, covering the
//! prefix check, digest decoder, and constant-time comparison path.

use libfuzzer_sys::fuzz_target;

use messenger_webhook::{signature::verify_meta, Secret};

fuzz_target!(|data: &[u8]| {
    let header = String::from_utf8_lossy(data);
    let secret = Secret::new("fuzz-secret-fuzz-secret").expect("secret");

    let _ = verify_meta(&secret, header.as_ref(), b"");
    let _ = verify_meta(&secret, header.as_ref(), data);
});
