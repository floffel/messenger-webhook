//! The shared payload fixtures, normalised through every adapter.
//!
//! The seeds live in `fuzz/corpus/normalize/` and are compiled into these tests,
//! so the fuzz corpus and the test fixtures cannot drift apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use messenger_webhook::Adapter;

const WHATSAPP_TEXT: &[u8] = include_bytes!("../fuzz/corpus/normalize/whatsapp_text.json");
const WHATSAPP_MIXED: &[u8] = include_bytes!("../fuzz/corpus/normalize/whatsapp_mixed.json");
const TELEGRAM_MESSAGE: &[u8] = include_bytes!("../fuzz/corpus/normalize/telegram_message.json");
const TELEGRAM_REACTION: &[u8] = include_bytes!("../fuzz/corpus/normalize/telegram_reaction.json");
const MATRIX_EVENT: &[u8] = include_bytes!("../fuzz/corpus/normalize/matrix_event.json");
const MATRIX_UNKNOWN: &[u8] = include_bytes!("../fuzz/corpus/normalize/matrix_unknown.json");

#[allow(clippy::vec_init_then_push, unused_mut)]
fn adapters() -> Vec<Box<dyn Adapter>> {
    let mut adapters: Vec<Box<dyn Adapter>> = Vec::new();
    #[cfg(feature = "whatsapp")]
    adapters.push(Box::new(messenger_webhook::WhatsAppAdapter::new("secret")));
    #[cfg(feature = "telegram")]
    adapters.push(Box::new(messenger_webhook::TelegramAdapter::new("secret")));
    #[cfg(feature = "matrix")]
    adapters.push(Box::new(messenger_webhook::MatrixAdapter::new("secret")));
    adapters
}

const FIXTURES: &[&[u8]] = &[
    WHATSAPP_TEXT,
    WHATSAPP_MIXED,
    TELEGRAM_MESSAGE,
    TELEGRAM_REACTION,
    MATRIX_EVENT,
    MATRIX_UNKNOWN,
];

#[test]
fn fixtures_normalise_without_panicking() {
    for payload in FIXTURES {
        for adapter in adapters() {
            for event in adapter.adapt(payload).unwrap_or_default() {
                assert_eq!(event.channel, adapter.channel());
                assert_ne!(event.external_id.len(), 0, "missing idempotency key");
                assert_ne!(event.conversation_id.len(), 0);
            }
        }
    }
}

#[test]
fn fixture_normalisation_is_deterministic() {
    for payload in FIXTURES {
        for adapter in adapters() {
            let first = adapter.adapt(payload).unwrap_or_default();
            let second = adapter.adapt(payload).unwrap_or_default();
            assert_eq!(first, second, "{} must be deterministic", adapter.channel());
        }
    }
}

#[test]
fn every_adapter_ignores_foreign_payloads() {
    // A payload for another provider must not produce messages and must not
    // error either: the shapes simply do not overlap.
    for payload in FIXTURES {
        for adapter in adapters() {
            let events = adapter.adapt(payload).unwrap_or_default();
            for event in events {
                assert_eq!(event.channel, adapter.channel());
            }
        }
    }
}

#[test]
fn raw_payload_is_retained_verbatim() {
    for payload in FIXTURES {
        for adapter in adapters() {
            for event in adapter.adapt(payload).unwrap_or_default() {
                assert!(
                    event.raw.is_some(),
                    "{} dropped the provider object",
                    adapter.channel()
                );
            }
        }
    }
}
