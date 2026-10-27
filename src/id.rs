//! Identifiers for events the provider did not number.
//!
//! Every provider occasionally sends an object without its own id — a `WhatsApp`
//! status without `id`, a Matrix event without `event_id`, a Telegram update
//! whose `message_id` never made it onto the wire. Dropping those events loses
//! real traffic, so they are surfaced with a **synthesised** id instead: a
//! digest over the channel, the event role and the raw object.
//!
//! The digest is stable — the same webhook body always yields the same id, so a
//! caller's dedupe ledger keeps working across a provider retry — and it is
//! prefixed with [`SYNTHETIC_ID_PREFIX`], so an application can tell an invented
//! key from a provider-assigned one.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model::{Channel, SYNTHETIC_ID_PREFIX};
use crate::signature::encode_hex;

/// Returns the provider's id, or a stable synthesised one when the field is
/// missing, empty or not an id at all.
pub(crate) fn identify(value: &Value, key: &str, channel: &Channel, role: &str) -> String {
    crate::json::id(value, key).unwrap_or_else(|| synthesize(channel.as_str(), role, value))
}

/// Builds the deterministic id for an event without a provider id.
///
/// `role` separates events that share a body — a `messages[]` object and a
/// `statuses[]` object never carry the same fields, but channel and role
/// together make a collision impossible even if they did.
pub(crate) fn synthesize(channel: &str, role: &str, raw: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(channel.as_bytes());
    hasher.update([0]);
    hasher.update(role.as_bytes());
    hasher.update([0]);
    // `Value` renders deterministically (object keys are ordered), so the same
    // parsed body hashes to the same digest no matter how the provider ordered
    // its JSON. `Display` on `Value` is infallible, so there is no error path
    // to swallow here.
    hasher.update(raw.to_string().as_bytes());
    format!(
        "{SYNTHETIC_ID_PREFIX}{}",
        encode_hex(hasher.finalize().as_slice())
    )
}

#[cfg(test)]
mod tests {
    use super::{identify, synthesize};
    use crate::model::{Channel, SYNTHETIC_ID_PREFIX};
    use serde_json::json;

    #[test]
    fn a_provider_id_wins_over_synthesis() {
        let value = json!({ "id": "wamid.1" });
        assert_eq!(
            identify(&value, "id", &Channel::WhatsApp, "message"),
            "wamid.1"
        );
    }

    #[test]
    fn an_empty_or_mistyped_id_is_synthesised() {
        for value in [json!({}), json!({ "id": "" }), json!({ "id": null })] {
            let id = identify(&value, "id", &Channel::WhatsApp, "message");
            assert!(
                id.starts_with(SYNTHETIC_ID_PREFIX),
                "expected a synthesised id, got {id:?}"
            );
        }
    }

    #[test]
    fn the_digest_ignores_key_order_and_tracks_content() {
        let first = synthesize("whatsapp", "message", &json!({ "a": 1, "b": 2 }));
        let reordered = synthesize("whatsapp", "message", &json!({ "b": 2, "a": 1 }));
        let changed = synthesize("whatsapp", "message", &json!({ "a": 1, "b": 3 }));
        assert_eq!(first, reordered, "key order must not change the id");
        assert_ne!(first, changed, "content must change the id");
    }

    #[test]
    fn channel_and_role_separate_otherwise_identical_bodies() {
        let body = json!({ "timestamp": 1 });
        let message = synthesize("whatsapp", "message", &body);
        let status = synthesize("whatsapp", "status", &body);
        let matrix = synthesize("matrix", "message", &body);
        assert_ne!(message, status);
        assert_ne!(message, matrix);
    }
}
