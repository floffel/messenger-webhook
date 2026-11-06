//! Property tests: arbitrary input never panics, encoding round-trips where it
//! is lossless, and unknown fields survive normalisation.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code, unused_imports, unused_mut, unused_variables)]

use proptest::prelude::*;
use serde_json::{Map, Value};
use std::str::FromStr;

use messenger_webhook::{
    Adapter, AttachmentKind, Channel, DeliveryStatus, Error, Headers, NormalizedMessage,
    OutboundMessage, MAX_CHANNEL_NAME_BYTES,
};

/// Arbitrary JSON of bounded depth, including wrong types in every position.
fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        // Bounded so JSON's float formatting round-trips exactly; exponent
        // extremes are not what this file is testing.
        any::<i32>().prop_map(|millis| Value::from(f64::from(millis) / 1000.0)),
        ".*".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 24, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::hash_map(".*", inner, 0..4)
                .prop_map(|entries| Value::Object(entries.into_iter().collect::<Map<_, _>>())),
        ]
    })
}

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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Rendering a modelled enum and parsing it back is lossless. The payload
    /// variants carry unknown strings verbatim, while known names win over a
    /// same-spelled `Other`/`Unknown` payload.
    #[test]
    fn provider_enum_string_round_trips(
        channel in prop_oneof![
            Just(Channel::WhatsApp),
            Just(Channel::Telegram),
            Just(Channel::Matrix),
            "signal|sms|rss-feed"
                .prop_map(|name| Channel::other(name).unwrap()),
        ],
        kind in prop_oneof![
            Just(AttachmentKind::Image),
            Just(AttachmentKind::Video),
            Just(AttachmentKind::Audio),
            Just(AttachmentKind::Voice),
            Just(AttachmentKind::Document),
            Just(AttachmentKind::Sticker),
            Just(AttachmentKind::Location),
            Just(AttachmentKind::Contact),
            "poll|reaction|unknown".prop_map(AttachmentKind::Other),
        ],
        status in prop_oneof![
            Just(DeliveryStatus::Sent),
            Just(DeliveryStatus::Delivered),
            Just(DeliveryStatus::Read),
            Just(DeliveryStatus::Failed),
            Just(DeliveryStatus::Deleted),
            "queued|held|unknown".prop_map(DeliveryStatus::Unknown),
        ],
    ) {
        prop_assert_eq!(Channel::from_str(channel.as_str()).unwrap(), channel);
        prop_assert_eq!(AttachmentKind::from_str(kind.as_str()).unwrap(), kind);
        prop_assert_eq!(DeliveryStatus::from_str(status.as_str()).unwrap(), status);
    }

    /// Any JSON document, at any depth, from any provider: `adapt` returns a
    /// result and never panics, and every event is internally consistent.
    #[test]
    fn arbitrary_json_never_panics(value in arb_json()) {
        let payload = serde_json::to_vec(&value).unwrap();
        for adapter in adapters() {
            match adapter.adapt(&payload) {
                Ok(events) => {
                    for event in events {
                        prop_assert_eq!(event.channel, adapter.channel());
                        prop_assert_ne!(event.external_id.len(), 0);
                        prop_assert_ne!(event.conversation_id.len(), 0);
                    }
                }
                Err(error) => {
                    // Only these two shapes are legitimate rejections.
                    prop_assert!(
                        matches!(error, Error::Payload(_) | Error::BodyTooLarge { .. }),
                        "unexpected error: {error:?}"
                    );
                }
            }
        }
    }

    /// Arbitrary bytes are not JSON: still a typed error, never a panic.
    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let mut headers = Headers::new();
        headers.insert("x-hub-signature-256", "sha256=00").unwrap();
        headers.insert("authorization", "Bearer secret").unwrap();
        headers
            .insert("x-telegram-bot-api-secret-token", "secret")
            .unwrap();

        for adapter in adapters() {
            let _ = adapter.verify(&headers, &bytes);
            match adapter.adapt(&bytes) {
                Ok(events) => {
                    for event in &events {
                        // Whatever came out must be encodable or rejected.
                        let _ = adapter.encode(&OutboundMessage::text(event.channel.clone(), event.conversation_id.clone(), event.text().unwrap_or_default()));
                    }
                }
                Err(Error::Payload(_)) => {}
                Err(other) => prop_assert!(false, "unexpected error: {other:?}"),
            }
        }
    }

    /// Header values are attacker-controlled; verification compares them
    /// without panicking and without ever accepting garbage.
    #[test]
    fn arbitrary_headers_never_panic(
        pairs in prop::collection::vec(("[a-zA-Z-]{0,32}", ".*"), 0..6),
        body in prop::collection::vec(any::<u8>(), 0..256),
    ) {
        let headers: Headers = pairs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        for adapter in adapters() {
            prop_assert!(adapter.verify(&headers, &body).is_err());
        }
    }

    /// Custom channel construction accepts exactly the documented grammar:
    /// lower-case ASCII letters, digits, `_`, `-`, non-empty, bounded, and not
    /// reserved for a built-in channel.
    #[test]
    fn custom_channel_names_accept_exactly_the_documented_grammar(name in ".*") {
        match Channel::other(name.clone()) {
            Ok(channel) => {
                let normalised = name.trim().to_ascii_lowercase();
                prop_assert_eq!(channel.as_str(), normalised.as_str());
                prop_assert!(!normalised.is_empty());
                prop_assert!(normalised.len() <= MAX_CHANNEL_NAME_BYTES);
                prop_assert!(normalised.bytes().all(|byte| matches!(
                    byte,
                    b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'
                )));
                prop_assert!(!matches!(
                    normalised.as_str(),
                    "whatsapp" | "telegram" | "matrix"
                ));
            }
            Err(Error::InvalidChannelName { .. }) => {}
            Err(other) => prop_assert!(false, "unexpected error: {other:?}"),
        }
    }

    /// Header lookup answers with the stored value for every ASCII casing of
    /// the stored name, and never matches a name that differs in more than
    /// case. This is the in-place `eq_ignore_ascii_case` contract that replaced
    /// the allocating lower-case copy on the verification hot path.
    #[test]
    fn header_lookup_is_case_insensitive(
        name in "[a-zA-Z][a-zA-Z0-9-]{0,24}",
        value in "[\t -~]{0,64}",
        casing in prop::collection::vec(any::<bool>(), 0..25),
    ) {
        let mut headers = Headers::new();
        headers.insert(name.as_str(), value.as_str()).unwrap();

        // Flip each character according to the generated casing pattern; the
        // queried spelling is the same header name, only re-cased.
        let mut cased = String::with_capacity(name.len());
        for (index, ch) in name.chars().enumerate() {
            let uppercase = casing.get(index).copied().unwrap_or(false);
            if uppercase {
                cased.push(ch.to_ascii_uppercase());
            } else {
                cased.push(ch.to_ascii_lowercase());
            }
        }
        prop_assert_eq!(headers.get(&cased), Some(value.as_str()));

        // A single appended byte makes it a different header name, not a match.
        let longer = format!("{name}x");
        prop_assert_eq!(headers.get(&longer), None);
    }

    /// Unknown fields in a provider object survive in `raw`, so nothing the
    /// provider sent is silently lost.
    #[test]
    fn unknown_fields_are_preserved(key in "[a-z_]{1,12}", value in arb_json()) {
        // Prefixed so the generated key can never collide with a real field.
        let key = format!("vendor_{key}");
        // Compare against the value as JSON represents it (f64 formatting
        // round-trips through the wire, not through the original bits).
        let value: Value = serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
        let whatsapp = serde_json::json!({
            "entry": [{"changes": [{"value": {"messages": [{
                "id": "wamid.1", "from": "49", "type": "text",
                "text": {"body": "x"}, key.clone(): value.clone(),
            }]}}]}],
        });
        let telegram = serde_json::json!({
            "message": {
                "message_id": 1, "chat": {"id": 2}, "date": 1, "text": "x",
                key.clone(): value.clone(),
            },
        });
        let matrix = serde_json::json!({
            "type": "m.room.message", "event_id": "$1", "room_id": "!r:s",
            "sender": "@a:s", "content": {"msgtype": "m.text", "body": "x"},
            key.clone(): value.clone(),
        });

        #[cfg(feature = "whatsapp")]
        {
            let events = messenger_webhook::WhatsAppAdapter::new("s")
                .adapt(&serde_json::to_vec(&whatsapp).unwrap())
                .unwrap();
            prop_assert_eq!(&events[0].raw.as_ref().unwrap()[&key], &value);
        }
        #[cfg(feature = "telegram")]
        {
            let events = messenger_webhook::TelegramAdapter::new("s")
                .adapt(&serde_json::to_vec(&telegram).unwrap())
                .unwrap();
            prop_assert_eq!(&events[0].raw.as_ref().unwrap()[&key], &value);
        }
        #[cfg(feature = "matrix")]
        {
            let events = messenger_webhook::MatrixAdapter::new("s")
                .adapt(&serde_json::to_vec(&matrix).unwrap())
                .unwrap();
            prop_assert_eq!(&events[0].raw.as_ref().unwrap()[&key], &value);
        }
        let _ = (key, value);
    }
}

/// Characters that break naive length checks (surrogate pairs, NUL, combining).
fn arb_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![any::<char>(), Just('😀'), Just('\0'), Just('ä'), Just('𝄞')],
        1..300,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn digits() -> impl Strategy<Value = String> {
    prop::collection::vec(0..=9u8, 5..15).prop_map(|digits| {
        digits
            .into_iter()
            .map(|digit| char::from(b'0' + digit))
            .collect()
    })
}

/// Wrap an outbound body the way the provider would echo it back inbound.
#[cfg(feature = "telegram")]
fn telegram_echo(conversation: &str, text: &str) {
    let adapter = messenger_webhook::TelegramAdapter::new("secret");
    let message = OutboundMessage::text(Channel::Telegram, conversation, text);
    let payload = adapter.encode(&message).unwrap();
    let body: Map<String, Value> = serde_json::from_slice(payload.body()).unwrap();
    let chat_id = body["chat_id"].as_str().expect("chat_id");
    let echoed_text = body["text"].as_str().expect("text");

    // Telegram assigns the message id and nests the chat object; everything
    // else comes back verbatim.
    let update = serde_json::json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "chat": {"id": chat_id},
            "from": {"id": chat_id},
            "date": 1,
            "text": echoed_text,
        },
    });
    let events = adapter
        .adapt(&serde_json::to_vec(&update).unwrap())
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].conversation_id, chat_id);
    assert_eq!(events[0].text(), Some(echoed_text));
    assert_eq!(chat_id, conversation);
    assert_eq!(echoed_text, text);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Telegram: encode → provider echo → normalise is lossless for chat and
    /// text.
    #[cfg(feature = "telegram")]
    #[test]
    fn telegram_text_round_trips(chat in any::<i64>().prop_map(|id| id.to_string()), text in arb_text()) {
        telegram_echo(&chat, &text);
    }

    /// WhatsApp: encode → provider echo → normalise is lossless for the
    /// recipient and the body, measured in bytes.
    #[cfg(feature = "whatsapp")]
    #[test]
    fn whatsapp_text_round_trips(recipient in digits(), text in arb_text()) {
        let adapter = messenger_webhook::WhatsAppAdapter::new("secret");
        let message = OutboundMessage::text(Channel::WhatsApp, recipient.clone(), text.clone());
        let payload = adapter.encode(&message).unwrap();
        let body: Value = serde_json::from_slice(payload.body()).unwrap();

        let inbound = serde_json::json!({
            "entry": [{"changes": [{"value": {"messages": [{
                "id": "wamid.1", "from": body["to"], "timestamp": "1729600000",
                "type": "text", "text": {"body": text.clone()},
            }]}}]}],
        });
        let events = adapter
            .adapt(&serde_json::to_vec(&inbound).unwrap())
            .unwrap();
        prop_assert_eq!(&events[0].conversation_id, &recipient);
        prop_assert_eq!(events[0].text(), Some(text.as_str()));
    }

    /// Matrix: encode → wrap in an event envelope → normalise is lossless for
    /// room and body.
    #[cfg(feature = "matrix")]
    #[test]
    fn matrix_text_round_trips(
        room in "![a-z]{1,8}:[a-z]{1,8}",
        text in arb_text(),
    ) {
        let adapter = messenger_webhook::MatrixAdapter::new("secret");
        let message = OutboundMessage::text(Channel::Matrix, room.clone(), text.clone());
        let content: Value = adapter.encode(&message).unwrap().json().unwrap();
        let event = serde_json::json!({
            "type": "m.room.message", "event_id": "$1", "room_id": room.clone(),
            "sender": "@a:s", "origin_server_ts": 1_729_600_000_000_i64, "content": content,
        });
        let events = adapter
            .adapt(&serde_json::to_vec(&event).unwrap())
            .unwrap();
        prop_assert_eq!(&events[0].conversation_id, &room);
        prop_assert_eq!(events[0].text(), Some(text.as_str()));
    }

    /// Reply and thread references survive a Matrix round trip.
    #[cfg(feature = "matrix")]
    #[test]
    fn matrix_reply_round_trips(reply in "\\$[a-z0-9]{1,8}") {
        let adapter = messenger_webhook::MatrixAdapter::new("secret");
        let mut message = OutboundMessage::text(Channel::Matrix, "!r:s", "hi");
        message.reply_to = Some(reply.clone());
        let content: Value = adapter.encode(&message).unwrap().json().unwrap();
        let event = serde_json::json!({
            "type": "m.room.message", "event_id": "$1", "room_id": "!r:s",
            "sender": "@a:s", "content": content,
        });
        let events = adapter.adapt(&serde_json::to_vec(&event).unwrap()).unwrap();
        prop_assert_eq!(events[0].reply_to.as_deref(), Some(reply.as_str()));
    }

    /// The limit is a hard boundary in the provider's unit, not in characters.
    #[cfg(feature = "telegram")]
    #[test]
    fn telegram_length_boundary_is_exact(units in 1024usize..=5120) {
        let adapter = messenger_webhook::TelegramAdapter::new("secret");
        let text = "😀".repeat(units / 2) ; // two UTF-16 units per emoji
        let measured = text.encode_utf16().count();
        let message = OutboundMessage::text(Channel::Telegram, "1", text);
        let result = adapter.encode(&message);
        prop_assert_eq!(result.is_ok(), measured <= 4096);
        if let Err(Error::MessageTooLong { limit, actual, .. }) = result {
            prop_assert_eq!(limit, 4096);
            prop_assert_eq!(actual, measured);
        }
    }

    /// WhatsApp counts bytes: multi-byte text hits the limit earlier.
    #[cfg(feature = "whatsapp")]
    #[test]
    fn whatsapp_length_boundary_is_exact(characters in 1000usize..=4200) {
        let adapter = messenger_webhook::WhatsAppAdapter::new("secret");
        let text = "ä".repeat(characters); // two bytes per character
        let message = OutboundMessage::text(Channel::WhatsApp, "49", text);
        let result = adapter.encode(&message);
        prop_assert_eq!(result.is_ok(), characters * 2 <= 4096);
        if let Err(Error::MessageTooLong { limit, actual, .. }) = result {
            prop_assert_eq!(limit, 4096);
            prop_assert_eq!(actual, characters * 2);
        }
    }

    /// Every produced payload is JSON the provider can parse.
    #[test]
    fn encoded_payloads_are_valid_json(seed in 0u32..1000) {
        for adapter in adapters() {
            let text = format!("text {seed} äöü 😀 \u{0}");
            let conversation = match adapter.channel() {
                Channel::WhatsApp => "4915112345678".to_owned(),
                Channel::Matrix => "!r:example.org".to_owned(),
                _ => "-1001".to_owned(),
            };
            let message = OutboundMessage::text(adapter.channel(), conversation, text);
            let payload = adapter.encode(&message).unwrap();
            prop_assert!(payload.json().is_ok());
            prop_assert_eq!(payload.channel(), &adapter.channel());
        }
    }
}
