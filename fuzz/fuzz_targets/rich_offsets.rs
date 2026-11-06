#![no_main]
//! Fuzzes Telegram's UTF-16 entity offsets through the public adapter path.
//!
//! The raw bytes split into a little-endian `(offset, length)` pair and the
//! message text. Every emitted span has to slice the normalised text, and a
//! dropped entity is the adapter's `None` result for a range that has no byte
//! equivalent.

use libfuzzer_sys::fuzz_target;
use messenger_webhook::{Adapter, TelegramAdapter};
use serde_json::json;

fn utf16_boundaries(text: &str) -> Vec<Option<usize>> {
    let total_units = text.encode_utf16().count();
    let mut boundaries = vec![None; total_units + 1];
    boundaries[0] = Some(0);

    let mut unit = 0;
    for (byte, character) in text.char_indices() {
        for split in 1..character.len_utf16() {
            boundaries[unit + split] = None;
        }
        unit += character.len_utf16();
        boundaries[unit] = Some(byte + character.len_utf8());
    }

    boundaries
}

fn expected_range(text: &str, offset: u64, length: u64) -> Option<(usize, usize)> {
    if length == 0 {
        return None;
    }
    let end_unit = offset.checked_add(length)?;
    let boundaries = utf16_boundaries(text);
    let start = *boundaries.get(usize::try_from(offset).ok()?)?;
    let end = *boundaries.get(usize::try_from(end_unit).ok()?)?;
    Some((start?, end?))
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 16 {
        return;
    }

    let (range, text_bytes) = data.split_at(16);
    let offset = u64::from_le_bytes(range[..8].try_into().expect("8 bytes"));
    let length = u64::from_le_bytes(range[8..].try_into().expect("8 bytes"));
    let Ok(text) = std::str::from_utf8(text_bytes) else {
        return;
    };

    let body = json!({
        "update_id": 1,
        "message": {
            "message_id": 7,
            "from": { "id": 42 },
            "chat": { "id": -1001 },
            "date": 1_729_600_000,
            "text": text,
            "entities": [{ "type": "bold", "offset": offset, "length": length }],
        }
    });
    let payload = serde_json::to_vec(&body).expect("json body");
    let adapter = TelegramAdapter::new("fuzz-secret-fuzz-secret").expect("adapter");
    let Ok(events) = adapter.adapt(&payload) else {
        return;
    };

    assert_eq!(events.len(), 1);
    let event = &events[0];
    let normalized = event.text().expect("the message carried text");
    assert_eq!(normalized, text);

    match expected_range(text, offset, length) {
        Some((start, end)) => {
            assert_eq!(event.spans().len(), 1);
            let span = &event.spans()[0];
            assert_eq!((span.start, span.end), (start, end));
            assert!(span.slice(normalized).is_some());
        }
        None => assert!(event.spans().is_empty()),
    }
});
