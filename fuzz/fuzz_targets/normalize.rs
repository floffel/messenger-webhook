#![no_main]
//! Nothing a provider (or an attacker) sends may panic this crate.
//!
//! Every input goes through all three providers: verification with the correct
//! and with a wrong credential, normalisation, and encoding of whatever came
//! out. The seeds in `corpus/normalize/` are the same files the test suite
//! compiles in, so the fuzzer starts from real provider payloads.

use libfuzzer_sys::fuzz_target;

use messenger_webhook::{
    signature::meta_signature, Adapter, Headers, MatrixAdapter, OutboundMessage, Secret,
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
    authentic.insert("x-hub-signature-256", signature).unwrap();
    authentic
        .insert("x-telegram-bot-api-secret-token", "fuzz-secret")
        .unwrap();
    authentic
        .insert("authorization", "Bearer fuzz-secret")
        .unwrap();

    let mut forged = Headers::new();
    forged.insert("x-hub-signature-256", "sha256=00").unwrap();
    forged
        .insert("x-telegram-bot-api-secret-token", "wrong")
        .unwrap();
    forged.insert("authorization", "Bearer wrong").unwrap();

    for adapter in [&whatsapp as &dyn Adapter, &telegram, &matrix] {
        let _ = adapter.verify(&authentic, data);
        assert!(adapter.verify(&forged, data).is_err());

        match adapter.adapt(data) {
            Ok(events) => {
                for event in &events {
                    assert_eq!(event.channel, adapter.channel());
                    assert!(!event.external_id.is_empty());
                    // An event whose conversation the provider did not send is
                    // carried on purpose (the id is synthesised, see
                    // `SYNTHETIC_ID_PREFIX`), but it must not be encodable:
                    // there is no recipient to send it to.
                    let outbound = OutboundMessage::text(
                        event.channel.clone(),
                        event.conversation_id.clone(),
                        event.text().unwrap_or_default(),
                    );
                    if event.conversation_id.is_empty() {
                        assert!(
                            adapter.encode(&outbound).is_err(),
                            "{} encoded an event without a recipient",
                            event.channel
                        );
                    }
                    // Encoding an outbound message must succeed or fail typed.
                    let _ = adapter.encode(&outbound);
                }
            }
            Err(_) => {}
        }
    }

    // The same bytes as text, in both directions.
    let text = String::from_utf8_lossy(data);
    let message =
        OutboundMessage::text(messenger_webhook::Channel::WhatsApp, "4915112345678", text);
    let _ = whatsapp.encode(&message);
});
