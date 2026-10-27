//! Events the provider sent without an id: they are carried, not dropped, and
//! the synthesised key is stable across replays and distinct per content.

#![cfg(all(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use proptest::prelude::*;
use serde_json::{json, Value};

use messenger_webhook::{Adapter, NormalizedMessage, SYNTHETIC_ID_PREFIX};

fn whatsapp() -> impl Adapter {
    messenger_webhook::WhatsAppAdapter::new("app-secret")
}

fn telegram() -> impl Adapter {
    messenger_webhook::TelegramAdapter::new("bot-secret")
}

fn matrix() -> impl Adapter {
    messenger_webhook::MatrixAdapter::new("hs-token")
}

fn adapt(adapter: &dyn Adapter, body: &Value) -> Vec<NormalizedMessage> {
    adapter
        .adapt(body.to_string().as_bytes())
        .expect("the body is valid JSON for this adapter")
}

#[test]
fn a_whatsapp_message_without_an_id_is_carried_with_a_synthetic_one() {
    let body = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            { "from": "4915112345678", "timestamp": "1729600000",
              "type": "text", "text": { "body": "Hallo" } }
        ] } }] }]
    });
    let events = adapt(&whatsapp(), &body);
    assert_eq!(events.len(), 1, "the event must not disappear");
    assert!(events[0].has_synthetic_id());
    assert!(events[0].external_id.starts_with(SYNTHETIC_ID_PREFIX));
    assert_eq!(events[0].text.as_deref(), Some("Hallo"));
    assert_eq!(events[0].conversation_id, "4915112345678");

    // A provider retry delivers the identical body: same key, so a caller's
    // ledger dedupes instead of double-processing.
    let replay = adapt(&whatsapp(), &body);
    assert_eq!(events[0].external_id, replay[0].external_id);
}

#[test]
fn a_provider_id_is_never_replaced() {
    let body = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            { "id": "wamid.1", "from": "4915112345678", "type": "text",
              "text": { "body": "Hallo" } }
        ] } }] }]
    });
    let events = adapt(&whatsapp(), &body);
    assert_eq!(events[0].external_id, "wamid.1");
    assert!(!events[0].has_synthetic_id());
}

#[test]
fn different_content_yields_a_different_synthetic_id() {
    let build = |text: &str| {
        json!({
            "entry": [{ "changes": [{ "value": { "messages": [
                { "from": "4915112345678", "type": "text", "text": { "body": text } }
            ] } }] }]
        })
    };
    let first = adapt(&whatsapp(), &build("one"));
    let second = adapt(&whatsapp(), &build("two"));
    assert_ne!(first[0].external_id, second[0].external_id);
}

#[test]
fn a_whatsapp_status_without_an_id_is_a_status_event() {
    let body = json!({
        "entry": [{ "changes": [{ "value": { "statuses": [
            { "recipient_id": "4915112345678", "status": "read" }
        ] } }] }]
    });
    let events = adapt(&whatsapp(), &body);
    assert_eq!(events.len(), 1);
    assert!(events[0].is_status_update());
    assert!(events[0].has_synthetic_id());
}

#[test]
fn a_message_without_a_sender_is_carried_with_an_empty_conversation() {
    let body = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            { "type": "text", "text": { "body": "anonymous" } }
        ] } }] }]
    });
    let events = adapt(&whatsapp(), &body);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].conversation_id, "");
    assert!(events[0].has_synthetic_id());
    assert_eq!(events[0].text.as_deref(), Some("anonymous"));
}

#[test]
fn entries_that_are_not_objects_are_skipped() {
    let body = json!({
        "entry": [{ "changes": [{ "value": {
            "messages": ["not an object", 42, { "id": "wamid.2", "from": "49", "type": "text" }]
        } }] }]
    });
    let events = adapt(&whatsapp(), &body);
    assert_eq!(events.len(), 1, "only the real message survives");
    assert_eq!(events[0].external_id, "wamid.2");
}

#[test]
fn a_telegram_update_without_a_message_id_is_carried() {
    let body = json!({
        "update_id": 1,
        "message": { "chat": { "id": -1001 }, "text": "Hallo" }
    });
    let events = adapt(&telegram(), &body);
    assert_eq!(events.len(), 1);
    assert!(events[0].has_synthetic_id());
    assert_eq!(events[0].conversation_id, "-1001");
    assert_eq!(events[0].text.as_deref(), Some("Hallo"));
}

#[test]
fn a_telegram_update_without_a_chat_keeps_an_empty_conversation() {
    let body = json!({ "update_id": 2, "message": { "text": "Hallo" } });
    let events = adapt(&telegram(), &body);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].conversation_id, "");
    assert!(events[0].has_synthetic_id());
}

#[test]
fn a_matrix_event_without_an_event_id_is_carried() {
    let body = json!({
        "type": "m.room.message", "room_id": "!r:example.org",
        "sender": "@a:example.org", "origin_server_ts": 1_729_600_000_000_i64,
        "content": { "msgtype": "m.text", "body": "Hallo" }
    });
    let events = adapt(&matrix(), &body);
    assert_eq!(events.len(), 1);
    assert!(events[0].has_synthetic_id());
    assert_eq!(events[0].conversation_id, "!r:example.org");
    assert_eq!(events[0].text.as_deref(), Some("Hallo"));
}

#[test]
fn a_matrix_transaction_carries_every_object_it_holds() {
    let body = json!({
        "events": [
            { "type": "m.room.message", "content": { "msgtype": "m.text", "body": "one" } },
            "not an object",
            { "type": "m.room.message", "event_id": "$2", "room_id": "!r:s",
              "content": { "msgtype": "m.text", "body": "two" } }
        ]
    });
    let events = adapt(&matrix(), &body);
    assert_eq!(events.len(), 2, "the string entry is not an event");
    assert!(events[0].has_synthetic_id());
    assert_eq!(events[0].conversation_id, "");
    assert_eq!(events[1].external_id, "$2");
}

#[test]
fn every_adapter_leaves_no_event_without_a_key() {
    let bodies = vec![
        (
            Box::new(whatsapp()) as Box<dyn Adapter>,
            json!({
                "entry": [{ "changes": [{ "value": {
                    "messages": [{ "type": "text" }, {}],
                    "statuses": [{ "status": "sent" }]
                } }] }]
            }),
        ),
        (
            Box::new(telegram()) as Box<dyn Adapter>,
            json!({
                "message": {"chat": {"id": 1}}, "edited_message": {}, "message_reaction": {}
            }),
        ),
        (
            Box::new(matrix()) as Box<dyn Adapter>,
            json!({
                "events": [{ "type": "m.room.message", "content": {} }, {}]
            }),
        ),
    ];
    for (adapter, body) in bodies {
        let events = adapter
            .adapt(body.to_string().as_bytes())
            .expect("valid JSON");
        assert!(
            !events.is_empty(),
            "{} dropped every event",
            adapter.channel()
        );
        for event in &events {
            assert!(
                !event.external_id.is_empty(),
                "{} produced an event without a key",
                event.channel
            );
            assert_eq!(event.channel, adapter.channel());
        }
    }
}

#[test]
fn a_synthetic_key_is_prefixed_and_never_empty() {
    // Two byte-identical bodies through two adapters: the keys differ because
    // the channel is part of the digest.
    let whatsapp_events = adapt(
        &whatsapp(),
        &json!({
            "entry": [{ "changes": [{ "value": { "messages": [{ "type": "text" }] } }] }]
        }),
    );
    let matrix_events = adapt(
        &matrix(),
        &json!({ "type": "m.room.message", "content": {} }),
    );
    for event in whatsapp_events.iter().chain(matrix_events.iter()) {
        assert!(event.has_synthetic_id());
        assert_eq!(event.external_id.len(), SYNTHETIC_ID_PREFIX.len() + 64);
        assert!(event.external_id.starts_with(SYNTHETIC_ID_PREFIX));
    }
    assert_ne!(
        whatsapp_events[0].external_id, matrix_events[0].external_id,
        "the channel is part of the digest"
    );
}

proptest! {
    /// The key is a pure function of the body: any object without an `id` maps
    /// to one stable synthesised key, and two parses of the same body agree.
    #[test]
    fn the_synthetic_key_is_a_function_of_the_body(
        entries in prop::collection::hash_map(
            prop::sample::select(vec!["a", "b", "c"]),
            any::<i64>(),
            0..4,
        )
    ) {
        // `type` is always present: without a field of its own the object is
        // not a WhatsApp message at all and is skipped by design.
        let mut message: serde_json::Map<String, Value> = entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), Value::from(value)))
            .collect();
        message.insert("type".to_owned(), Value::from("text"));
        let body = json!({
            "entry": [{ "changes": [{ "value": { "messages": [ Value::Object(message) ] } }] }]
        });

        let first = adapt(&whatsapp(), &body);
        let second = adapt(&whatsapp(), &body);

        prop_assert_eq!(first.len(), 1);
        prop_assert_eq!(first[0].external_id.len(), SYNTHETIC_ID_PREFIX.len() + 64);
        prop_assert!(first[0].has_synthetic_id());
        prop_assert_eq!(&first[0].external_id, &second[0].external_id);
    }
}

#[test]
fn a_payload_from_another_provider_does_not_become_an_event() {
    // Nothing here has a field this adapter knows, so nothing is invented.
    let whatsapp_body = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            { "id": "wamid.1", "from": "49", "type": "text" }
        ] } }] }]
    });
    assert!(adapt(&matrix(), &whatsapp_body).is_empty());
    assert!(adapt(&telegram(), &whatsapp_body).is_empty());

    let matrix_body = json!({
        "type": "m.room.message", "room_id": "!r:s", "content": {}
    });
    assert!(adapt(&whatsapp(), &matrix_body).is_empty());

    let telegram_body = json!({ "update_id": 1, "message": { "text": "hi" } });
    assert!(adapt(&matrix(), &telegram_body).is_empty());
    assert!(adapt(&whatsapp(), &telegram_body).is_empty());
}
