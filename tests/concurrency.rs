//! Adapters must be `Send + Sync`, immutable, and safe to hammer from many
//! tasks at once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::thread;

use messenger_webhook::{Adapter, Channel, Headers, Registry};

const THREADS: usize = 32;
const ITERATIONS: usize = 100;

#[test]
fn adapters_and_registry_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<Registry>();
    assert_send_sync::<Box<dyn Adapter>>();
    #[cfg(feature = "whatsapp")]
    assert_send_sync::<messenger_webhook::WhatsAppAdapter>();
    #[cfg(feature = "telegram")]
    assert_send_sync::<messenger_webhook::TelegramAdapter>();
    #[cfg(feature = "matrix")]
    assert_send_sync::<messenger_webhook::MatrixAdapter>();
}

/// One body per channel, plus the headers that verify it.
fn corpus() -> Vec<(Channel, Headers, &'static [u8])> {
    #[allow(unused_mut)]
    let mut corpus: Vec<(Channel, Headers, &'static [u8])> = Vec::new();

    #[cfg(feature = "whatsapp")]
    {
        let body: &'static [u8] = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"wamid.1","from":"4915112345678","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;
        let signature = messenger_webhook::signature::meta_signature(
            &messenger_webhook::Secret::new("secret"),
            body,
        )
        .unwrap();
        corpus.push((
            Channel::WhatsApp,
            Headers::from_pairs([("x-hub-signature-256", signature)]),
            body,
        ));
    }
    #[cfg(feature = "telegram")]
    {
        corpus.push((
            Channel::Telegram,
            Headers::from_iter([("x-telegram-bot-api-secret-token", "secret")]),
            br#"{"update_id":1,"message":{"message_id":7,"from":{"id":42},"chat":{"id":-1001},"date":1,"text":"Hallo"}}"#,
        ));
    }
    #[cfg(feature = "matrix")]
    {
        corpus.push((
            Channel::Matrix,
            Headers::from_iter([("authorization", "Bearer secret")]),
            br#"{"type":"m.room.message","event_id":"$1","room_id":"!r:s","sender":"@a:s","content":{"msgtype":"m.text","body":"Hallo"}}"#,
        ));
    }
    corpus
}

fn registry() -> Registry {
    #[allow(unused_mut)]
    let mut registry = Registry::new();
    #[cfg(feature = "whatsapp")]
    registry.register(messenger_webhook::WhatsAppAdapter::new("secret"));
    #[cfg(feature = "telegram")]
    registry.register(messenger_webhook::TelegramAdapter::new("secret"));
    #[cfg(feature = "matrix")]
    registry.register(messenger_webhook::MatrixAdapter::new("secret"));
    registry
}

#[test]
fn thirty_two_tasks_share_one_registry() {
    let corpus = corpus();
    let registry = Arc::new(registry());

    // Baseline from a single thread.
    let expected: Vec<usize> = corpus
        .iter()
        .map(|(channel, headers, body)| {
            registry
                .handle(channel, headers, body)
                .expect("baseline must verify")
                .len()
        })
        .collect();

    let mut handles = Vec::new();
    for _ in 0..THREADS {
        let registry = Arc::clone(&registry);
        let corpus = corpus.clone();
        let expected = expected.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..ITERATIONS {
                for ((channel, headers, body), expected) in corpus.iter().zip(&expected) {
                    let events = registry.handle(channel, headers, body).unwrap();
                    assert_eq!(events.len(), *expected);
                    for event in &events {
                        assert_eq!(event.channel, *channel);
                        // Encoding from many threads at once must be safe too.
                        let _ = registry.encode(channel, event);
                    }
                }
            }
        }));
    }
    for handle in handles {
        handle.join().expect("worker panicked");
    }
}

#[test]
fn thirty_two_tasks_share_one_borrowed_adapter() {
    // No Arc, no interior mutability: a plain shared reference is enough.
    #[cfg(feature = "matrix")]
    {
        let adapter = messenger_webhook::MatrixAdapter::new("secret");
        let adapter: &dyn Adapter = &adapter;
        let body = br#"{"events":[
            {"type":"m.room.message","event_id":"$1","room_id":"!r:s","sender":"@a:s","content":{"msgtype":"m.text","body":"a"}},
            {"type":"m.reaction","event_id":"$2","room_id":"!r:s","sender":"@b:s","content":{"m.relates_to":{"rel_type":"m.annotation","event_id":"$1","key":"\ud83d\udc4d"}}}]}"#;

        let expected = adapter.adapt(body).unwrap();
        thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..ITERATIONS {
                        assert_eq!(adapter.adapt(body).unwrap(), expected);
                    }
                });
            }
        });
    }
}

#[test]
fn concurrent_verification_of_a_forged_body_always_fails() {
    #[cfg(feature = "whatsapp")]
    {
        let adapter = Arc::new(messenger_webhook::WhatsAppAdapter::new("secret"));
        let mut handles = Vec::new();
        for worker in 0..THREADS {
            let adapter = Arc::clone(&adapter);
            handles.push(thread::spawn(move || {
                let body = format!(r#"{{"entry":[],"worker":{worker}}}"#);
                let headers = Headers::from_iter([(
                    "x-hub-signature-256",
                    "sha256=0000000000000000000000000000000000000000000000000000000000000000",
                )]);
                for _ in 0..ITERATIONS {
                    assert!(adapter.verify(&headers, body.as_bytes()).is_err());
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker panicked");
        }
    }
}
