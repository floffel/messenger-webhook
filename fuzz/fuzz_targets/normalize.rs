#![no_main]
//! Nothing a provider (or an attacker) sends may panic this crate.
//!
//! Every input goes through all three providers: verification with the correct
//! and with a wrong credential, normalisation, and encoding of whatever came
//! out. The seeds in `corpus/normalize/` are the same files the test suite
//! compiles in, so the fuzzer starts from real provider payloads.

use libfuzzer_sys::fuzz_target;

use messenger_webhook::{
    signature::meta_signature, Adapter, Headers, MatrixAdapter, NormalizedMessage, Secret,
    TelegramAdapter, WhatsAppAdapter,
};

fuzz_target!(|data: &[u8]| {
    let whatsapp = WhatsAppAdapter::new("fuzz-secret");
    let telegram = TelegramAdapter::new("fuzz-secret");
    let matrix = MatrixAdapter::new("fuzz-secret");

    // A valid signature for exactly this body, so verification succeeds and
    // the parser is reached on every iteration.
    let signature = meta_signature(&Secret::new("fuzz-secret"), data).unwrap_or_default();
    let mut authentic = Headers::new();
    authentic.insert("x-hub-signature-256", signature);
    authentic.insert("x-telegram-bot-api-secret-token", "fuzz-secret");
    authentic.insert("authorization", "Bearer fuzz-secret");

    let mut forged = Headers::new();
    forged.insert("x-hub-signature-256", "sha256=00");
    forged.insert("x-telegram-bot-api-secret-token", "wrong");
    forged.insert("authorization", "Bearer wrong");

    for adapter in [&whatsapp as &dyn Adapter, &telegram, &matrix] {
        let _ = adapter.verify(&authentic, data);
        assert!(adapter.verify(&forged, data).is_err());

        match adapter.adapt(data) {
            Ok(events) => {
                for event in &events {
                    assert_eq!(event.channel, adapter.channel());
                    assert!(!event.external_id.is_empty());
                    assert!(!event.conversation_id.is_empty());
                    // Encoding a normalised event must succeed or fail typed.
                    let _ = adapter.encode(event);
                }
            }
            Err(_) => {}
        }
    }

    // The same bytes as text, in both directions.
    let text = String::from_utf8_lossy(data);
    let message = NormalizedMessage::text(messenger_webhook::Channel::WhatsApp, "4915112345678", text);
    let _ = whatsapp.encode(&message);
});
