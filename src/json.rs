//! Permissive JSON accessors.
//!
//! Providers change their payloads without warning and send the same fact as a
//! string in one field and as a number in the next. Every accessor here returns
//! `None` for a missing *or* mistyped field, so a surprise in one field never
//! fails the whole batch — the adapter simply carries what it can.

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
pub(crate) fn id(value: &Value, key: &str) -> Option<String> {
    match child(value, key)? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
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
