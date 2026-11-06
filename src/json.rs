//! Permissive JSON accessors.
//!
//! Providers change their payloads without warning and send the same fact as a
//! string in one field and as a number in the next. Every accessor here returns
//! `None` for a missing *or* mistyped field, so a surprise in one field never
//! fails the whole batch — the adapter simply carries what it can.

use crate::model::has_graphic;
use serde_json::Value;

/// Borrows any field.
pub(crate) fn child<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|inner| !inner.is_null())
}

/// Borrows a string field.
pub(crate) fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    child(value, key)?.as_str()
}

/// Lends an array field, or an empty slice when it is absent or not an array.
pub(crate) fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    child(value, key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Reads an identifier that may arrive as a string or as a number.
///
/// A string is returned verbatim only when it carries a graphic character;
/// `""`, `"   "`, `"\t\n"` and `"\u{200b}"` are placeholders and yield `None`
/// so the caller can synthesise an id. Numbers are always real — `0` is a
/// legitimate provider id — and are rendered in their JSON form.
pub(crate) fn id(value: &Value, key: &str) -> Option<String> {
    match child(value, key)? {
        Value::String(text) if has_graphic(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Reads an integer that may arrive as a number or as a numeric string.
pub(crate) fn integer(value: &Value, key: &str) -> Option<i64> {
    match child(value, key)? {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|n| i64::try_from(n).ok())),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Reads an unsigned integer, tolerating numeric strings.
pub(crate) fn unsigned(value: &Value, key: &str) -> Option<u64> {
    match child(value, key)? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Narrows an optional `u64` to `u32` without truncating.
pub(crate) fn narrow(value: Option<u64>) -> Option<u32> {
    value.and_then(|n| u32::try_from(n).ok())
}

/// Reads a boolean flag, defaulting to `false`.
pub(crate) fn boolean(value: &Value, key: &str) -> bool {
    child(value, key).and_then(Value::as_bool).unwrap_or(false)
}

/// Converts milliseconds to seconds, ignoring values that overflow.
pub(crate) fn millis_to_seconds(millis: Option<i64>) -> Option<i64> {
    millis.map(|ms| ms.div_euclid(1000))
}

#[cfg(test)]
mod tests {
    use super::id;
    use crate::model::has_graphic;
    use serde_json::json;

    #[test]
    fn a_string_id_is_returned_verbatim() {
        assert_eq!(
            id(&json!({ "id": "wamid.1" }), "id").as_deref(),
            Some("wamid.1")
        );
        // Bytes around a real id are kept, not trimmed.
        assert_eq!(id(&json!({ "id": " a " }), "id").as_deref(), Some(" a "));
        assert_eq!(
            id(&json!({ "id": "\u{1f600}" }), "id").as_deref(),
            Some("\u{1f600}")
        );
    }

    #[test]
    fn empty_and_whitespace_string_ids_are_placeholders() {
        // Valid value, the value just inside the limit, and the value just
        // outside it: "", " ", and surrounded whitespace.
        for placeholder in ["", " ", "\t\n", "\r", "\u{00a0}", "   "] {
            assert_eq!(
                id(&json!({ "id": placeholder }), "id"),
                None,
                "{placeholder:?} must be a placeholder, not a real id"
            );
        }
    }

    #[test]
    fn non_graphic_format_characters_are_placeholders() {
        // `char::is_whitespace` does not catch these, so the category check is
        // what makes them fail closed.
        for placeholder in ["\u{200b}", "\u{feff}", "\u{200e}", "\u{2060}"] {
            assert_eq!(
                id(&json!({ "id": placeholder }), "id"),
                None,
                "{placeholder:?} must be a placeholder, not a real id"
            );
        }
    }

    #[test]
    fn a_string_with_one_graphic_character_survives() {
        // The value just inside the boundary: padding around a real id.
        assert_eq!(
            id(&json!({ "id": " \u{200b}a " }), "id").as_deref(),
            Some(" \u{200b}a ")
        );
        assert!(has_graphic(" \u{200b}\t\nx"));
    }

    #[test]
    fn numeric_ids_are_always_real() {
        // 0 is a legitimate provider id and must stay valid on both sides.
        assert_eq!(
            id(&json!({ "message_id": 0 }), "message_id").as_deref(),
            Some("0")
        );
        assert_eq!(
            id(&json!({ "message_id": 1 }), "message_id").as_deref(),
            Some("1")
        );
        assert_eq!(
            id(&json!({ "message_id": -1001 }), "message_id").as_deref(),
            Some("-1001")
        );
        assert_eq!(
            id(
                &json!({ "message_id": 9_007_199_254_740_993_i64 }),
                "message_id"
            )
            .as_deref(),
            Some("9007199254740993")
        );
    }

    #[test]
    fn missing_null_and_mistyped_fields_are_placeholders() {
        for value in [
            json!({}),
            json!({ "id": null }),
            json!({ "id": true }),
            json!({ "id": [] }),
        ] {
            assert_eq!(id(&value, "id"), None);
        }
    }
}
