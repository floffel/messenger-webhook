//! Rich text: entity offsets become byte spans, or are dropped.
//!
//! Telegram counts `MessageEntity.offset`/`.length` in UTF-16 code units, while
//! `NormalizedMessage::text` is a Rust string. These tests pin the conversion at
//! its edges — an emoji outside the BMP is two units, a boundary inside its
//! surrogate pair has no byte equivalent — and pin every entity type onto a
//! [`TextFormat`] instead of trusting the happy path.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(unused_imports, unused_variables)]

#[cfg(feature = "telegram")]
use messenger_webhook::telegram::TelegramAdapter;
#[cfg(any(feature = "telegram", feature = "whatsapp"))]
use messenger_webhook::Adapter;
use messenger_webhook::{Channel, NormalizedMessage, SpanError, TextFormat, TextSpan};
use proptest::prelude::*;
use serde_json::{json, Value};

/// One Telegram update carrying `message`, adapted without a socket.
#[cfg(feature = "telegram")]
fn adapt(message: &Value) -> NormalizedMessage {
    let body = json!({ "update_id": 1, "message": message });
    let bytes = serde_json::to_vec(&body).expect("body");
    TelegramAdapter::new("secret")
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
    assert_eq!(message.spans.len(), 1, "expected exactly one span");
    let span = &message.spans[0];
    let text = message.text.as_deref().expect("text");
    (
        span.format.clone(),
        span.slice(text).expect("span slices its text").to_owned(),
    )
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_entities_become_byte_spans() {
    let message = with_entities(
        "Hallo Welt",
        &json!([{ "type": "bold", "offset": 6, "length": 4 }]),
    );
    assert!(message.is_formatted());
    let span = &message.spans[0];
    assert_eq!((span.start, span.end), (6, 10));
    assert_eq!(only_span(&message).0, TextFormat::Bold);
    assert_eq!(message.text.as_deref(), Some("Hallo Welt"));
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
    let span = &message.spans[0];
    assert_eq!((span.start, span.end), (1, 5));
    assert_eq!(only_span(&message), (TextFormat::Italic, "😀".to_owned()));
    // ...and the byte offsets are char boundaries, so slicing cannot panic.
    assert_eq!(&message.text.as_deref().unwrap()[1..5], "😀");
}

#[cfg(feature = "telegram")]
#[test]
fn boundary_inside_a_surrogate_pair_is_dropped() {
    // Offset 2 lands between the two UTF-16 units of the emoji.
    let message = with_entities(
        "a😀b",
        &json!([{ "type": "bold", "offset": 2, "length": 1 }]),
    );
    assert_eq!(message.spans, Vec::new(), "no invented run");
    assert!(!message.is_formatted());
    assert_eq!(message.text.as_deref(), Some("a😀b"), "text stays complete");
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
        assert_eq!(message.spans, Vec::new(), "dropped: {entity}");
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
        pre.spans[0].format,
        TextFormat::Pre {
            language: Some("rust".to_owned())
        }
    );

    let link = with_entities(
        "docs",
        &json!([{ "type": "text_link", "offset": 0, "length": 4, "url": "https://docs.rs" }]),
    );
    assert_eq!(
        link.spans[0].format,
        TextFormat::Link {
            url: "https://docs.rs".to_owned()
        }
    );

    let mention = with_entities(
        "Ann",
        &json!([{ "type": "text_mention", "offset": 0, "length": 3, "user": { "id": 4242 } }]),
    );
    assert_eq!(
        mention.spans[0].format,
        TextFormat::TextMention {
            user_id: "4242".to_owned()
        }
    );

    let emoji = with_entities(
        "x",
        &json!([{ "type": "custom_emoji", "offset": 0, "length": 1, "custom_emoji_id": "5368324170671202286" }]),
    );
    assert_eq!(
        emoji.spans[0].format,
        TextFormat::CustomEmoji {
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
    assert_eq!(message.text.as_deref(), Some("ein Bild"));
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
            .spans
            .iter()
            .map(|span| (span.start, span.end))
            .collect::<Vec<_>>(),
        [(0, 6), (2, 4)]
    );
}

/// An adapter that carries neither markup nor a payload for one sends no spans.
#[test]
fn adapters_that_send_no_entities_produce_no_spans() {
    assert_eq!(
        NormalizedMessage::text(Channel::Matrix, "!r:s", "hi").spans,
        Vec::new()
    );
    assert!(!NormalizedMessage::text(Channel::Matrix, "!r:s", "hi").is_formatted());
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
    let events = messenger_webhook::WhatsAppAdapter::new("app-secret")
        .adapt(&serde_json::to_vec(&body).expect("body"))
        .expect("adapt");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].spans, Vec::new());
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
        (0, 99, SpanError::OutOfBounds { end: 99, len: 10 }),
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
        SpanError::OutOfBounds { end: 9, len: 3 }.to_string(),
        "span 0..9 does not fit in 3 bytes of text"
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
    let events = TelegramAdapter::new("secret").adapt(body).expect("adapt");
    let message = &events[0];

    assert_eq!(message.text.as_deref(), Some("Hallo Welt"));
    let span = &message.spans[0];
    assert_eq!(span.format, TextFormat::Bold);
    assert_eq!(span.slice("Hallo Welt"), Some("Welt"));
    assert!(span.covers(6));
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
#[cfg(feature = "telegram")]
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

#[cfg(feature = "telegram")]
mod utf16_round_trip {
    use super::{any_text, with_entities};
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
            prop_assert_eq!(message.spans.len(), 1);
            let span = &message.spans[0];
            prop_assert_eq!((span.start, span.end), (start, end));
            prop_assert_eq!(span.slice(&text), Some(&text[start..end]));
        }
    }
}
