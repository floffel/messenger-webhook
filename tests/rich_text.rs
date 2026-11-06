//! Rich text: entity offsets become byte spans, or are dropped.
//!
//! Telegram counts `MessageEntity.offset`/`.length` in UTF-16 code units, while
//! `NormalizedMessage::text()` is a Rust string. These tests pin the conversion at
//! its edges — an emoji outside the BMP is two units, a boundary inside its
//! surrogate pair has no byte equivalent — and pin every entity type onto a
//! [`TextFormat`] instead of trusting the happy path.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(unused_imports, unused_variables)]

#[cfg(feature = "telegram")]
use messenger_webhook::telegram::TelegramAdapter;
#[cfg(any(feature = "telegram", feature = "whatsapp"))]
use messenger_webhook::Adapter;
use messenger_webhook::{
    Channel, NormalizedMessage, OutboundMessage, SpanError, TextFormat, TextSpan,
};
use proptest::prelude::*;
use serde_json::{json, Value};

/// One Telegram update carrying `message`, adapted without a socket.
#[cfg(feature = "telegram")]
fn adapt(message: &Value) -> NormalizedMessage {
    let body = json!({ "update_id": 1, "message": message });
    let bytes = serde_json::to_vec(&body).expect("body");
    TelegramAdapter::new("secret-secret-secret")
        .expect("adapter")
        .adapt(&bytes)
        .expect("adapt")
        .into_iter()
        .next()
        .expect("one event")
}

/// A text message with `entities`, adapted.
#[cfg(feature = "telegram")]
fn with_entities(text: &str, entities: &Value) -> NormalizedMessage {
    adapt(&json!({
        "message_id": 7,
        "from": { "id": 42 },
        "chat": { "id": -1001 },
        "date": 1_729_600_000,
        "text": text,
        "entities": entities,
    }))
}

/// The single span of an adapted message, with the text it covers.
#[cfg(feature = "telegram")]
fn only_span(message: &NormalizedMessage) -> (TextFormat, String) {
    assert_eq!(message.spans().len(), 1, "expected exactly one span");
    let span = &message.spans()[0];
    let text = message.text().expect("text");
    (
        span.format().clone(),
        span.slice(text).expect("span slices its text").to_owned(),
    )
}

#[cfg(feature = "telegram")]
fn encode(message: &OutboundMessage) -> serde_json::Value {
    TelegramAdapter::new("secret-secret-secret")
        .expect("adapter")
        .encode(message)
        .expect("encode")
        .json()
        .expect("json")
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_entities_become_byte_spans() {
    let message = with_entities(
        "Hallo Welt",
        &json!([{ "type": "bold", "offset": 6, "length": 4 }]),
    );
    assert!(message.is_formatted());
    let span = &message.spans()[0];
    assert_eq!((span.start(), span.end()), (6, 10));
    assert_eq!(only_span(&message).0, TextFormat::Bold);
    assert_eq!(message.text(), Some("Hallo Welt"));
}

#[cfg(feature = "telegram")]
#[test]
fn astral_characters_shift_utf16_offsets_to_bytes() {
    // "a😀b" is 6 bytes but 4 UTF-16 units; the emoji spans units 1..3, i.e.
    // bytes 1..5.
    let message = with_entities(
        "a😀b",
        &json!([{ "type": "italic", "offset": 1, "length": 2 }]),
    );
    let span = &message.spans()[0];
    assert_eq!((span.start(), span.end()), (1, 5));
    assert_eq!(only_span(&message), (TextFormat::Italic, "😀".to_owned()));
    // ...and the byte offsets are char boundaries, so slicing cannot panic.
    assert_eq!(&message.text().unwrap()[1..5], "😀");
}

#[cfg(feature = "telegram")]
#[test]
fn boundary_inside_a_surrogate_pair_is_dropped() {
    // Offset 2 lands between the two UTF-16 units of the emoji.
    let message = with_entities(
        "a😀b",
        &json!([{ "type": "bold", "offset": 2, "length": 1 }]),
    );
    assert_eq!(message.spans(), Vec::new(), "no invented run");
    assert!(!message.is_formatted());
    assert_eq!(message.text(), Some("a😀b"), "text stays complete");
}

#[cfg(feature = "telegram")]
#[test]
fn ranges_past_the_end_or_empty_are_dropped() {
    for entity in [
        json!({ "type": "bold", "offset": 3, "length": 99 }),
        json!({ "type": "bold", "offset": 0, "length": 0 }),
        json!({ "type": "bold", "offset": 2, "length": 0 }),
        json!({ "type": "bold", "offset": 0 }),
        json!({ "offset": 0, "length": 3 }),
        json!({ "type": 7, "offset": 0, "length": 3 }),
        json!({ "type": "bold", "offset": -1, "length": 3 }),
    ] {
        let message = with_entities("abc", &json!([entity.clone()]));
        assert_eq!(message.spans(), Vec::new(), "dropped: {entity}");
    }
}

#[cfg(feature = "telegram")]
#[test]
fn every_structural_entity_type_maps_onto_a_format() {
    let cases = [
        ("bold", TextFormat::Bold),
        ("italic", TextFormat::Italic),
        ("underline", TextFormat::Underline),
        ("strikethrough", TextFormat::Strikethrough),
        ("spoiler", TextFormat::Spoiler),
        ("code", TextFormat::Code),
        ("mention", TextFormat::Mention),
        ("hashtag", TextFormat::Hashtag),
        ("cashtag", TextFormat::Cashtag),
        ("bot_command", TextFormat::BotCommand),
        ("url", TextFormat::Url),
        ("email", TextFormat::Email),
        ("phone_number", TextFormat::PhoneNumber),
        ("blockquote", TextFormat::Blockquote),
        ("expandable_blockquote", TextFormat::ExpandableBlockquote),
        ("pre", TextFormat::Pre { language: None }),
        ("date_time", TextFormat::Other("date_time".to_owned())),
        (
            "something_new",
            TextFormat::Other("something_new".to_owned()),
        ),
    ];
    for (kind, expected) in cases {
        let message = with_entities("abc", &json!([{ "type": kind, "offset": 0, "length": 3 }]));
        assert_eq!(only_span(&message), (expected, "abc".to_owned()), "{kind}");
    }
}

#[cfg(feature = "telegram")]
#[test]
fn payloads_of_structured_entities_are_carried() {
    let pre = with_entities(
        "fn main() {}",
        &json!([{ "type": "pre", "offset": 0, "length": 12, "language": "rust" }]),
    );
    assert_eq!(
        pre.spans()[0].format(),
        &TextFormat::Pre {
            language: Some("rust".to_owned())
        }
    );

    let link = with_entities(
        "docs",
        &json!([{ "type": "text_link", "offset": 0, "length": 4, "url": "https://docs.rs" }]),
    );
    assert_eq!(
        link.spans()[0].format(),
        &TextFormat::Link {
            url: "https://docs.rs".to_owned()
        }
    );

    let mention = with_entities(
        "Ann",
        &json!([{ "type": "text_mention", "offset": 0, "length": 3, "user": { "id": 4242 } }]),
    );
    assert_eq!(
        mention.spans()[0].format(),
        &TextFormat::TextMention {
            user_id: "4242".to_owned()
        }
    );

    let emoji = with_entities(
        "x",
        &json!([{ "type": "custom_emoji", "offset": 0, "length": 1, "custom_emoji_id": "5368324170671202286" }]),
    );
    assert_eq!(
        emoji.spans()[0].format(),
        &TextFormat::CustomEmoji {
            id: "5368324170671202286".to_owned()
        }
    );
}

#[cfg(feature = "telegram")]
#[test]
fn structured_entities_without_their_payload_keep_the_provider_name() {
    for (entity, expected) in [
        (
            json!({ "type": "text_link", "offset": 0, "length": 3 }),
            TextFormat::Other("text_link".to_owned()),
        ),
        (
            json!({ "type": "text_link", "offset": 0, "length": 3, "url": "" }),
            TextFormat::Other("text_link".to_owned()),
        ),
        (
            json!({ "type": "text_mention", "offset": 0, "length": 3 }),
            TextFormat::Other("text_mention".to_owned()),
        ),
        (
            json!({ "type": "custom_emoji", "offset": 0, "length": 3 }),
            TextFormat::Other("custom_emoji".to_owned()),
        ),
    ] {
        let message = with_entities("abc", &json!([entity.clone()]));
        assert_eq!(only_span(&message).0, expected, "{entity}");
    }
}

#[cfg(feature = "telegram")]
#[test]
fn caption_entities_describe_the_caption() {
    let message = adapt(&json!({
        "message_id": 8,
        "from": { "id": 42 },
        "chat": { "id": -1001 },
        "date": 1_729_600_000,
        "photo": [{ "file_id": "small", "width": 90, "height": 90 }],
        "caption": "ein Bild",
        "caption_entities": [{ "type": "underline", "offset": 4, "length": 4 }],
    }));
    assert_eq!(message.text(), Some("ein Bild"));
    assert_eq!(
        only_span(&message),
        (TextFormat::Underline, "Bild".to_owned())
    );
}

#[cfg(feature = "telegram")]
#[test]
fn entities_in_provider_order_and_overlapping_stay_untouched() {
    let message = with_entities(
        "abcdef",
        &json!([
            { "type": "bold", "offset": 0, "length": 6 },
            { "type": "italic", "offset": 2, "length": 2 },
        ]),
    );
    assert_eq!(
        message
            .spans()
            .iter()
            .map(|span| (span.start(), span.end()))
            .collect::<Vec<_>>(),
        [(0, 6), (2, 4)]
    );
}

#[cfg(feature = "telegram")]
#[test]
fn many_entities_are_converted_in_provider_order_and_bad_ranges_do_not_shift() {
    let message = with_entities(
        "a😀bc",
        &json!([
            { "type": "bold", "offset": 1, "length": 2 },
            { "type": "italic", "offset": 2, "length": 1 },
            { "type": "underline", "offset": 3, "length": 2 },
            { "type": "code", "offset": 0, "length": 99 },
            { "type": "spoiler", "offset": 3, "length": 1 },
        ]),
    );

    assert_eq!(
        message
            .spans()
            .iter()
            .map(|span| (span.start(), span.end(), span.format().as_str()))
            .collect::<Vec<_>>(),
        [(1, 5, "bold"), (5, 7, "underline"), (5, 6, "spoiler")]
    );
}

/// An adapter that carries neither markup nor a payload for one sends no spans.
#[test]
fn adapters_that_send_no_entities_produce_no_spans() {
    assert_eq!(
        NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "hi").spans(),
        Vec::new()
    );
    assert!(!NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "hi").is_formatted());
}

/// The Cloud API reports a `WhatsApp` text message as `text.body`; there is no
/// markup in it, so `spans` stays empty and `is_formatted` stays false.
#[cfg(feature = "whatsapp")]
#[test]
fn whatsapp_text_messages_carry_no_spans() {
    let body = json!({
        "object": "whatsapp_business_account",
        "entry": [{ "changes": [{ "field": "messages", "value": {
            "messaging_product": "whatsapp",
            "messages": [{ "id": "wamid.1", "from": "49", "type": "text", "text": { "body": "hi" } }]
        }}]}]
    });
    let events = messenger_webhook::WhatsAppAdapter::new("app-secret-app-secret")
        .expect("adapter")
        .adapt(&serde_json::to_vec(&body).expect("body"))
        .expect("adapt");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].spans(), Vec::new());
    assert!(!events[0].is_formatted());
}

#[test]
fn span_constructor_and_accessors_hold_their_contract() {
    let text = "Hallo Welt";
    let span = TextSpan::new(text, 6, 10, TextFormat::Bold).expect("valid span");
    assert_eq!(span.slice(text), Some("Welt"));
    assert_eq!(span.slice("servus"), None);
    assert!(span.covers(6));
    assert!(!span.covers(10));

    for (start, end, expected) in [
        (2, 2, SpanError::Empty { start: 2, end: 2 }),
        (3, 2, SpanError::Empty { start: 3, end: 2 }),
        (
            2,
            99,
            SpanError::OutOfBounds {
                start: 2,
                end: 99,
                len: 10,
            },
        ),
    ] {
        assert_eq!(
            TextSpan::new(text, start, end, TextFormat::Code).unwrap_err(),
            expected
        );
    }
    assert_eq!(
        TextSpan::new("ä", 1, 2, TextFormat::Code).unwrap_err(),
        SpanError::NotCharBoundary { offset: 1 }
    );

    // Every error variant has a message that names the offending numbers.
    assert_eq!(
        SpanError::Empty { start: 2, end: 2 }.to_string(),
        "span 2..2 is empty or inverted"
    );
    assert_eq!(
        SpanError::OutOfBounds {
            start: 2,
            end: 9,
            len: 3,
        }
        .to_string(),
        "span 2..9 does not fit in 3 bytes of text"
    );
    assert_eq!(
        SpanError::NotCharBoundary { offset: 1 }.to_string(),
        "offset 1 is not a character boundary of the text"
    );
}

/// The README's rich-text example, compiled and run so it cannot rot.
#[cfg(feature = "telegram")]
#[test]
fn readme_example_runs() {
    let body = br#"{"update_id":1,"message":{"message_id":7,"from":{"id":42},
        "chat":{"id":-1001},"date":1729600000,"text":"Hallo Welt",
        "entities":[{"type":"bold","offset":6,"length":4}]}}"#;
    let events = TelegramAdapter::new("secret-secret-secret")
        .expect("adapter")
        .adapt(body)
        .expect("adapt");
    let message = &events[0];

    assert_eq!(message.text(), Some("Hallo Welt"));
    let span = &message.spans()[0];
    assert_eq!(span.format(), &TextFormat::Bold);
    assert_eq!(span.slice("Hallo Welt"), Some("Welt"));
    assert!(span.covers(6));
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_encode_maps_spans_to_entities() {
    let text = "Hallo Welt";
    let span = TextSpan::new(text, 6, 10, TextFormat::Bold).expect("valid span");
    let message = OutboundMessage::text(Channel::Telegram, "-1001", text).with_spans([span]);

    let body = encode(&message);
    assert_eq!(body["text"], text);
    assert!(
        body.get("parse_mode").is_none(),
        "entities replace parse_mode"
    );
    assert_eq!(
        body["entities"],
        json!([{ "type": "bold", "offset": 6, "length": 4 }])
    );
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_encode_offsets_astral_characters_in_utf16_units() {
    let text = "a😀b";
    let emoji = TextSpan::new(text, 1, 5, TextFormat::Italic).expect("emoji span");
    let all = TextSpan::new(text, 0, text.len(), TextFormat::Code).expect("full span");
    let message = OutboundMessage::text(Channel::Telegram, "-1001", text).with_spans([emoji, all]);

    let body = encode(&message);
    assert_eq!(
        body["entities"],
        json!([
            { "type": "italic", "offset": 1, "length": 2 },
            { "type": "code", "offset": 0, "length": 4 },
        ])
    );
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_encode_caption_entities_use_caption_offsets() {
    let text = "ein Bild";
    let span = TextSpan::new(text, 4, 8, TextFormat::Underline).expect("caption span");
    let mut message = OutboundMessage::text(Channel::Telegram, "-1001", text).with_spans([span]);
    let mut photo = messenger_webhook::Attachment::new(messenger_webhook::AttachmentKind::Image);
    photo.id = Some("AgAC-file".to_owned());
    message.attachments.push(photo);

    let body = encode(&message);
    assert_eq!(body["caption"], text);
    assert_eq!(
        body["caption_entities"],
        json!([{ "type": "underline", "offset": 4, "length": 4 }])
    );
    assert!(body.get("entities").is_none());
    assert!(body.get("parse_mode").is_none());
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_encode_rejects_span_that_does_not_slice_text() {
    let text = "Hallo Welt";
    let over = TextSpan::new("Hallo Welt!", 6, 11, TextFormat::Bold).expect("one byte too long");
    let message = OutboundMessage::text(Channel::Telegram, "-1001", text).with_spans([over]);

    let error = TelegramAdapter::new("secret-secret-secret")
        .expect("adapter")
        .encode(&message)
        .expect_err("invalid span");
    assert!(
        matches!(
            &error,
            messenger_webhook::Error::InvalidMessage { reason, .. }
                if reason.contains("span 6..11")
        ),
        "unexpected error: {error:?}"
    );
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_encode_keeps_entities_at_the_array_ceiling() {
    // The whole path is in-process: `encode` builds the Bot API body without
    // opening a socket, so the mock transport is just the serialised payload.
    let at_limit = |count: usize| {
        let text = "a".repeat(count);
        let spans = (0..count)
            .map(|index| {
                TextSpan::new(&text, index, index + 1, TextFormat::Bold).expect("one-char span")
            })
            .collect::<Vec<_>>();
        OutboundMessage::text(Channel::Telegram, "-1001", text).with_spans(spans)
    };

    // Just inside: exactly the ceiling Telegram stores, all of them survive.
    let body = encode(&at_limit(messenger_webhook::telegram::MAX_ENTITIES));
    assert_eq!(
        body["entities"].as_array().expect("array").len(),
        messenger_webhook::telegram::MAX_ENTITIES
    );

    // Just outside: one more would be silently dropped by Telegram, so the
    // encoder refuses it before the wire.
    let over = at_limit(messenger_webhook::telegram::MAX_ENTITIES + 1);
    let error = TelegramAdapter::new("secret-secret-secret")
        .expect("adapter")
        .encode(&over)
        .expect_err("too many entities");
    assert!(
        matches!(
            &error,
            messenger_webhook::Error::InvalidMessage { reason, .. }
                if reason.contains("at most 100 entities")
        ),
        "unexpected error: {error:?}"
    );
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_encode_maps_every_format_or_rejects_it() {
    let text = "abc";
    let cases = [
        (TextFormat::Bold, "bold"),
        (TextFormat::Italic, "italic"),
        (TextFormat::Underline, "underline"),
        (TextFormat::Strikethrough, "strikethrough"),
        (TextFormat::Spoiler, "spoiler"),
        (TextFormat::Code, "code"),
        (TextFormat::Pre { language: None }, "pre"),
        (
            TextFormat::Pre {
                language: Some("rust".to_owned()),
            },
            "pre",
        ),
        (TextFormat::Mention, "mention"),
        (TextFormat::Hashtag, "hashtag"),
        (TextFormat::Cashtag, "cashtag"),
        (TextFormat::BotCommand, "bot_command"),
        (TextFormat::Url, "url"),
        (TextFormat::Email, "email"),
        (TextFormat::PhoneNumber, "phone_number"),
        (TextFormat::Blockquote, "blockquote"),
        (TextFormat::ExpandableBlockquote, "expandable_blockquote"),
        (TextFormat::Other("date_time".to_owned()), "date_time"),
    ];
    for (format, kind) in cases {
        let span = TextSpan::new(text, 0, text.len(), format).expect("span");
        let body = encode(&OutboundMessage::text(Channel::Telegram, "1", text).with_spans([span]));
        assert_eq!(body["entities"][0]["type"], kind);
    }

    let link = TextSpan::new(
        text,
        0,
        text.len(),
        TextFormat::Link {
            url: "https://example.com".to_owned(),
        },
    )
    .expect("span");
    let body = encode(&OutboundMessage::text(Channel::Telegram, "1", text).with_spans([link]));
    assert_eq!(body["entities"][0]["type"], "text_link");
    assert_eq!(body["entities"][0]["url"], "https://example.com");

    let emoji = TextSpan::new(
        text,
        0,
        text.len(),
        TextFormat::CustomEmoji {
            id: "5368324170671202286".to_owned(),
        },
    )
    .expect("span");
    let body = encode(&OutboundMessage::text(Channel::Telegram, "1", text).with_spans([emoji]));
    assert_eq!(body["entities"][0]["type"], "custom_emoji");
    assert_eq!(
        body["entities"][0]["custom_emoji_id"],
        "5368324170671202286"
    );
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_encode_rejects_unexpressible_structured_formats() {
    let text = "abc";
    for format in [
        TextFormat::TextMention {
            user_id: "42".to_owned(),
        },
        TextFormat::CustomEmoji { id: String::new() },
        TextFormat::Other(String::new()),
    ] {
        let span = TextSpan::new(text, 0, text.len(), format).expect("span");
        let error = TelegramAdapter::new("secret-secret-secret")
            .expect("adapter")
            .encode(&OutboundMessage::text(Channel::Telegram, "1", text).with_spans([span]))
            .expect_err("format must be rejected");
        assert!(
            matches!(error, messenger_webhook::Error::InvalidMessage { .. }),
            "unexpected error: {error:?}"
        );
    }
}

#[test]
fn every_format_has_a_stable_name() {
    let cases = [
        (TextFormat::Bold, "bold"),
        (TextFormat::Italic, "italic"),
        (TextFormat::Underline, "underline"),
        (TextFormat::Strikethrough, "strikethrough"),
        (TextFormat::Spoiler, "spoiler"),
        (TextFormat::Code, "code"),
        (TextFormat::Pre { language: None }, "pre"),
        (
            TextFormat::Link {
                url: "u".to_owned(),
            },
            "link",
        ),
        (TextFormat::Mention, "mention"),
        (TextFormat::Hashtag, "hashtag"),
        (TextFormat::Cashtag, "cashtag"),
        (TextFormat::BotCommand, "bot_command"),
        (TextFormat::Url, "url"),
        (TextFormat::Email, "email"),
        (TextFormat::PhoneNumber, "phone_number"),
        (
            TextFormat::TextMention {
                user_id: "1".to_owned(),
            },
            "text_mention",
        ),
        (
            TextFormat::CustomEmoji { id: "2".to_owned() },
            "custom_emoji",
        ),
        (TextFormat::Blockquote, "blockquote"),
        (TextFormat::ExpandableBlockquote, "expandable_blockquote"),
        (TextFormat::Other("date_time".to_owned()), "date_time"),
    ];
    for (format, name) in cases {
        assert_eq!(format.as_str(), name);
    }
}

/// A text whose characters differ in UTF-8 and UTF-16 width.
fn any_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just('a'),
            Just('Z'),
            Just(' '),
            Just('ä'),
            Just('ß'),
            Just('€'),
            Just('😀'),
            Just('🧑'),
            Just('\n'),
        ],
        0..24,
    )
    .prop_map(|characters| characters.into_iter().collect())
}

fn any_span() -> impl Strategy<Value = TextSpan> {
    (0_usize..24, 0_usize..24, any::<u8>()).prop_map(|(a, b, format)| {
        let (mut start, mut end) = if a <= b { (a, b) } else { (b, a) };
        if start == end {
            if end < 24 {
                end += 1;
            } else {
                start -= 1;
            }
        }
        TextSpan::new(
            "aaaaaaaaaaaaaaaaaaaaaaaa",
            start,
            end,
            match format % 3 {
                0 => TextFormat::Bold,
                1 => TextFormat::Code,
                _ => TextFormat::Italic,
            },
        )
        .expect("fixture span")
    })
}

#[test]
fn changing_text_drops_spans_that_no_longer_slice_it() {
    let text = "Hallo Welt";
    let valid = TextSpan::new(text, 6, 10, TextFormat::Bold).expect("valid span");
    let just_inside = TextSpan::new(text, 0, text.len(), TextFormat::Code).expect("full span");
    let message = NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", text)
        .with_spans([valid.clone(), just_inside.clone()]);
    assert_eq!(message.spans(), [valid.clone(), just_inside.clone()]);
    assert_eq!(message.span_text(0), Some("Welt"));
    assert_eq!(message.span_text(1), Some(text));

    let shortened = message.clone().with_text("Hallo");
    assert_eq!(shortened.text(), Some("Hallo"));
    assert_eq!(shortened.spans(), []);
    assert_eq!(shortened.span_text(0), None);
    assert_eq!(message.clone().without_text().text(), None);
    assert_eq!(message.clone().without_text().spans(), []);

    let multibyte = message.with_text("Hällo W€");
    assert_eq!(multibyte.text(), Some("Hällo W€"));
    assert_eq!(multibyte.spans(), []);
}

#[test]
fn setting_spans_filters_every_span_against_the_final_text() {
    let text = "Hallo Welt";
    let valid = TextSpan::new(text, 6, 10, TextFormat::Bold).expect("valid span");
    let end_at_limit =
        TextSpan::new(text, 6, text.len(), TextFormat::Italic).expect("end at limit");
    let over = TextSpan::new("Hallo Welt!", 6, 11, TextFormat::Code).expect("past end for text");

    let message = NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", text)
        .with_spans([valid.clone(), end_at_limit.clone(), over]);
    assert_eq!(message.spans(), [valid, end_at_limit]);
    assert_eq!(message.span_text(0), Some("Welt"));
    assert_eq!(message.span_text(1), Some("Welt"));
    assert_eq!(message.span_text(2), None);
}

#[test]
fn with_text_and_spans_validates_the_pair_together() {
    let text = "Hallo Welt";
    let spans = [
        TextSpan::new(text, 0, 5, TextFormat::Bold).expect("prefix"),
        TextSpan::new(text, 6, 10, TextFormat::Italic).expect("suffix"),
    ];
    let message = NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "")
        .with_text_and_spans(text, spans.clone());
    assert_eq!(message.spans(), spans);
    assert_eq!(message.span_text(0), Some("Hallo"));
    assert_eq!(message.span_text(1), Some("Welt"));

    let invalid = TextSpan::new(text, 6, 10, TextFormat::Code).expect("suffix");
    let message = NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "")
        .with_text_and_spans("Hallo", [invalid]);
    assert_eq!(message.text(), Some("Hallo"));
    assert_eq!(message.spans(), []);
}

mod text_span_invariant {
    use super::{any_span, any_text};
    use messenger_webhook::{Channel, NormalizedMessage};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn every_stored_span_slices_the_stored_text(
            text in any_text(),
            spans in proptest::collection::vec(any_span(), 0..8),
        ) {
            let message = NormalizedMessage::inbound_text(Channel::Matrix, "$1", "!r:s", "")
                .with_text_and_spans(text.clone(), spans);

            let stored = message.text().unwrap_or_default();
            prop_assert!(message
                .spans()
                .iter()
                .all(|span| span.slice(stored).is_some()));
            for (index, span) in message.spans().iter().enumerate() {
                prop_assert_eq!(message.span_text(index), span.slice(stored));
            }
            prop_assert_eq!(message.span_text(message.spans().len()), None);
        }
    }
}

#[cfg(feature = "telegram")]
mod utf16_round_trip {
    use super::{adapt, any_text, encode, with_entities};
    use messenger_webhook::{Adapter, Channel, OutboundMessage, TextFormat, TextSpan};
    use proptest::prelude::*;
    use serde_json::json;

    proptest! {
        /// Every char-boundary range survives the UTF-16 round trip, and the span
        /// it produces slices exactly that range.
        #[test]
        fn utf16_offsets_round_trip_onto_byte_spans(
            text in any_text(),
            a in 0_usize..24,
            b in 0_usize..24,
        ) {
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
                .collect();
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let lo = lo.min(boundaries.len() - 1);
            let hi = hi.min(boundaries.len() - 1);
            let start = boundaries[lo];
            let end = boundaries[hi];
            prop_assume!(start < end);

            let offset = text[..start].encode_utf16().count();
            let length = text[start..end].encode_utf16().count();
            let message = with_entities(&text, &json!([{ "type": "bold", "offset": offset, "length": length }]),
            );
            prop_assert_eq!(message.spans().len(), 1);
            let span = &message.spans()[0];
            prop_assert_eq!((span.start(), span.end()), (start, end));
            prop_assert_eq!(span.slice(&text), Some(&text[start..end]));
        }

        #[test]
        fn decode_encode_decode_round_trips_spans(
            text in any_text(),
            a in 0_usize..24,
            b in 0_usize..24,
        ) {
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
                .collect();
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let lo = lo.min(boundaries.len() - 1);
            let hi = hi.min(boundaries.len() - 1);
            let start = boundaries[lo];
            let end = boundaries[hi];
            prop_assume!(start < end);

            let offset = text[..start].encode_utf16().count();
            let length = text[start..end].encode_utf16().count();
            let decoded = with_entities(&text, &json!([{ "type": "bold", "offset": offset, "length": length }]));
            let span = &decoded.spans()[0];
            let outbound = OutboundMessage::text(Channel::Telegram, "1", &text)
                .with_spans([span.clone()]);
            let body = encode(&outbound);
            let round_tripped = adapt(&json!({
                "message_id": 7,
                "from": { "id": 42 },
                "chat": { "id": -1001 },
                "date": 1_729_600_000,
                "text": text,
                "entities": body["entities"],
            }));

            prop_assert_eq!(round_tripped.spans().len(), 1);
            let round_tripped_span = &round_tripped.spans()[0];
            prop_assert_eq!(round_tripped_span.format().clone(), TextFormat::Bold);
            prop_assert_eq!((round_tripped_span.start(), round_tripped_span.end()), (start, end));
        }
    }
}
