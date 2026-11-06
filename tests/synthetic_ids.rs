//! Events the provider sent without an id: they are carried, not dropped, and
//! the synthesised key is stable across replays and distinct per content,
//! envelope position and Telegram update id.

#![cfg(all(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use proptest::prelude::*;
use serde_json::{json, Value};

use messenger_webhook::{Adapter, Error, NormalizedMessage, SYNTHETIC_ID_PREFIX};

fn whatsapp() -> impl Adapter {
    messenger_webhook::WhatsAppAdapter::new("app-secret-app-secret").expect("adapter")
}

fn telegram() -> impl Adapter {
    messenger_webhook::TelegramAdapter::new("bot-secret-bot-secret").expect("adapter")
}

fn matrix() -> impl Adapter {
    messenger_webhook::MatrixAdapter::new("hs-token-hs-token").expect("adapter")
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
    assert_eq!(events[0].text(), Some("Hallo"));
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
fn a_provider_id_that_starts_with_the_synthetic_prefix_is_retagged() {
    let whatsapp_body = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            { "id": "synthetic:from-provider", "from": "4915112345678",
              "type": "text", "text": { "body": "Hallo" } }
        ] } }] }]
    });
    let whatsapp_events = adapt(&whatsapp(), &whatsapp_body);
    assert_eq!(
        whatsapp_events[0].external_id,
        "provider:synthetic:from-provider"
    );
    assert!(!whatsapp_events[0].has_synthetic_id());

    let matrix_body = json!({
        "type": "m.room.message", "event_id": "synthetic:$from-provider",
        "room_id": "!r:example.org", "sender": "@a:example.org",
        "content": { "msgtype": "m.text", "body": "Hallo" }
    });
    let matrix_events = adapt(&matrix(), &matrix_body);
    assert_eq!(
        matrix_events[0].external_id,
        "provider:synthetic:$from-provider"
    );
    assert!(!matrix_events[0].has_synthetic_id());
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
fn identical_idless_bodies_at_different_envelope_paths_get_distinct_ids() {
    let whatsapp_message = json!({
        "from": "4915112345678", "timestamp": "1729600000",
        "type": "text", "text": { "body": "same" }
    });
    let whatsapp_body = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            whatsapp_message.clone(), whatsapp_message
        ] } }] }]
    });
    let whatsapp_events = adapt(&whatsapp(), &whatsapp_body);
    assert_eq!(whatsapp_events.len(), 2);
    assert!(whatsapp_events
        .iter()
        .all(NormalizedMessage::has_synthetic_id));
    assert_ne!(
        whatsapp_events[0].external_id, whatsapp_events[1].external_id,
        "the messages[] index must separate identical bodies"
    );

    let telegram_message = json!({ "chat": { "id": -1001 }, "text": "same" });
    let telegram_body = json!({
        "message": telegram_message.clone(), "edited_message": telegram_message
    });
    let telegram_events = adapt(&telegram(), &telegram_body);
    assert_eq!(telegram_events.len(), 2);
    assert!(telegram_events
        .iter()
        .all(NormalizedMessage::has_synthetic_id));
    assert_ne!(
        telegram_events[0].external_id, telegram_events[1].external_id,
        "the update field must separate identical bodies"
    );

    let matrix_event = json!({
        "type": "m.room.message", "room_id": "!r:example.org",
        "sender": "@a:example.org", "content": { "msgtype": "m.text", "body": "same" }
    });
    let matrix_body = json!({ "events": [matrix_event.clone(), matrix_event] });
    let matrix_events = adapt(&matrix(), &matrix_body);
    assert_eq!(matrix_events.len(), 2);
    assert!(matrix_events
        .iter()
        .all(NormalizedMessage::has_synthetic_id));
    assert_ne!(
        matrix_events[0].external_id, matrix_events[1].external_id,
        "the events[] index must separate identical bodies"
    );

    let replay = adapt(&whatsapp(), &whatsapp_body);
    assert_eq!(
        whatsapp_events[0].external_id, replay[0].external_id,
        "the same object at the same path must stay stable across replays"
    );
}

#[test]
fn telegram_update_id_separates_identical_idless_messages_across_deliveries() {
    let message = json!({ "chat": { "id": -1001 }, "text": "same" });
    let first_body = json!({ "update_id": 41, "message": message.clone() });
    let second_body = json!({ "update_id": 42, "message": message });

    let first = adapt(&telegram(), &first_body);
    let retry = adapt(&telegram(), &first_body);
    let second = adapt(&telegram(), &second_body);

    assert_eq!(first.len(), 1);
    assert!(first[0].has_synthetic_id());
    assert_eq!(
        first[0].external_id, retry[0].external_id,
        "a retry of the same update must keep its synthetic id"
    );
    assert_ne!(
        first[0].external_id, second[0].external_id,
        "a different update carrying an identical body is a different event"
    );
}

#[test]
fn canonical_numbers_keep_equivalent_payloads_together() {
    let integer = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            { "from": "4915112345678", "type": "text", "amount": 1,
              "nested": { "b": 2, "a": 1 } }
        ] } }] }]
    });
    let decimal_and_reordered = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            { "nested": { "a": 1, "b": 2 }, "amount": 1.0,
              "type": "text", "from": "4915112345678" }
        ] } }] }]
    });
    let first = adapt(&whatsapp(), &integer);
    let second = adapt(&whatsapp(), &decimal_and_reordered);
    assert_eq!(
        first[0].external_id, second[0].external_id,
        "JCS canonicalisation makes number spelling and object order irrelevant"
    );
}

#[test]
fn a_number_change_still_changes_the_synthetic_id() {
    let build = |amount: serde_json::Value| {
        json!({
            "entry": [{ "changes": [{ "value": { "messages": [
                { "from": "4915112345678", "type": "text", "amount": amount }
            ] } }] }]
        })
    };
    let first = adapt(&whatsapp(), &build(json!(1)));
    let second = adapt(&whatsapp(), &build(json!(1.5)));
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
    assert_eq!(events[0].text(), Some("anonymous"));
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
    assert_eq!(events[0].text(), Some("Hallo"));
}

#[test]
fn a_telegram_update_without_a_chat_is_rejected() {
    let body = json!({ "update_id": 2, "message": { "text": "Hallo" } });
    let error = telegram()
        .adapt(body.to_string().as_bytes())
        .expect_err("chat.id is mandatory for Telegram routing");
    assert!(
        matches!(&error, Error::Payload(message) if message.contains("chat.id is required")),
        "{error:?}"
    );
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
    assert_eq!(events[0].text(), Some("Hallo"));
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
                "message": {"chat": {"id": 1}},
                "edited_message": {"message_id": 2, "chat": {"id": 2}},
                "message_reaction": {
                    "chat": {"id": 3}, "message_id": 4, "user": {"id": 5},
                    "date": 6, "new_reaction": []
                }
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
        let suffix = event
            .external_id
            .strip_prefix(SYNTHETIC_ID_PREFIX)
            .expect("the synthetic prefix is present");
        assert_eq!(suffix.len(), 64);
        assert!(
            suffix.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "the digest suffix must be 64 hex digits: {suffix:?}"
        );
        assert!(
            suffix.bytes().all(|byte| !byte.is_ascii_uppercase()),
            "the digest suffix must be lower-case hex: {suffix:?}"
        );
    }
    assert_ne!(
        whatsapp_events[0].external_id, matrix_events[0].external_id,
        "the channel is part of the digest"
    );
}

#[test]
fn a_synthetic_key_is_a_bounded_64_hex_character_digest() {
    let body = json!({
        "entry": [{ "changes": [{ "value": { "messages": [{
            "from": "49",
            "type": "text",
            "text": { "body": "x".repeat(512) }
        }] } }] }]
    });
    let events = adapt(&whatsapp(), &body);
    assert_eq!(events.len(), 1);
    assert!(events[0].has_synthetic_id());
    assert_eq!(events[0].external_id.len(), SYNTHETIC_ID_PREFIX.len() + 64);
    assert!(events[0]
        .external_id
        .trim_start_matches(SYNTHETIC_ID_PREFIX)
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit()));
}

#[test]
fn placeholder_string_ids_are_synthesised_not_carried_verbatim() {
    // "" was already handled; " ", the zero-width space and a surrounding
    // padding all name nothing, so each must be synthesised. If they were kept
    // verbatim every such event would share one key and the ledger would drop
    // all but the first.
    let build = |id: &str| {
        json!({
            "entry": [{ "changes": [{ "value": { "messages": [
                { "id": id, "from": "49", "type": "text", "text": { "body": "Hallo" } }
            ] } }] }]
        })
    };
    for placeholder in ["", " ", "\t\n", "\u{200b}", "\u{00a0}", "   "] {
        let events = adapt(&whatsapp(), &build(placeholder));
        assert_eq!(events.len(), 1);
        assert!(
            events[0].has_synthetic_id(),
            "{placeholder:?} must not be carried as a real id, got {:?}",
            events[0].external_id
        );
    }
}

#[test]
fn a_string_id_with_graphic_content_is_still_carried_verbatim() {
    // The value just inside the boundary: whitespace padding around a real id.
    let body = json!({
        "entry": [{ "changes": [{ "value": { "messages": [
            { "id": " wamid.1 ", "from": "49", "type": "text", "text": { "body": "Hallo" } }
        ] } }] }]
    });
    let events = adapt(&whatsapp(), &body);
    assert_eq!(events[0].external_id, " wamid.1 ");
    assert!(!events[0].has_synthetic_id());
}

#[test]
fn a_numeric_zero_id_stays_a_real_id() {
    // Telegram message ids are numbers and 0 is legitimate; only strings have a
    // placeholder form.
    let body = json!({
        "update_id": 7,
        "message": { "message_id": 0, "chat": { "id": 1 }, "text": "Hallo" }
    });
    let events = adapt(&telegram(), &body);
    assert_eq!(events[0].external_id, "0");
    assert!(!events[0].has_synthetic_id());
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
    assert_eq!(
        adapt(&matrix(), &whatsapp_body),
        Vec::new(),
        "matrix saw a WhatsApp body"
    );
    assert_eq!(
        adapt(&telegram(), &whatsapp_body),
        Vec::new(),
        "telegram saw a WhatsApp body"
    );

    let matrix_body = json!({
        "type": "m.room.message", "room_id": "!r:s", "content": {}
    });
    assert_eq!(
        adapt(&whatsapp(), &matrix_body),
        Vec::new(),
        "whatsapp saw a Matrix body"
    );

    let telegram_body = json!({ "update_id": 1, "message": { "text": "hi" } });
    assert_eq!(
        adapt(&matrix(), &telegram_body),
        Vec::new(),
        "matrix saw a Telegram body"
    );
    assert_eq!(
        adapt(&whatsapp(), &telegram_body),
        Vec::new(),
        "whatsapp saw a Telegram body"
    );
}
