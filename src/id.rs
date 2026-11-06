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

use std::io::{self, Write};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::{Channel, SYNTHETIC_ID_PREFIX};
use crate::signature::encode_hex;
use crate::MAX_BODY_BYTES;

/// Prefix used when a provider id would otherwise impersonate this crate's
/// synthetic-id namespace.
const PROVIDER_ID_PREFIX: &str = "provider:";

/// Returns the provider's id, or a stable synthesised one when the field is
/// missing, empty or not an id at all.
pub(crate) fn identify(value: &Value, key: &str, channel: &Channel, role: &str) -> Result<String> {
    match crate::json::id(value, key) {
        Some(id) => Ok(retag_provider_id(id)),
        None => synthesize(channel.as_str(), role, value, MAX_BODY_BYTES),
    }
}

/// Guards the synthetic-id namespace from provider ids that impersonate it.
///
/// A provider is free to send an id that already looks like `synthetic:...`.
/// Carrying it verbatim would make `has_synthetic_id()` lie, so such a value is
/// moved under the `provider:` namespace while keeping the original content.
fn retag_provider_id(id: String) -> String {
    if id.starts_with(SYNTHETIC_ID_PREFIX) {
        format!("{PROVIDER_ID_PREFIX}{id}")
    } else {
        id
    }
}

/// Builds the deterministic id for an event without a provider id.
///
/// `role` separates events that share a body — a `messages[]` object and a
/// `statuses[]` object never carry the same fields, but channel and role
/// together make a collision impossible even if they did.
pub(crate) fn synthesize(
    channel: &str,
    role: &str,
    raw: &Value,
    max_bytes: usize,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(channel.as_bytes());
    hasher.update([0]);
    hasher.update(role.as_bytes());
    hasher.update([0]);
    // `Value` renders deterministically (object keys are ordered), so the same
    // parsed body hashes to the same digest no matter how the provider ordered
    // its JSON. Stream that rendering straight into the hasher: the only
    // allocation left is the bounded 32-byte digest's final hex string.
    {
        let mut writer = DigestWriter::new(&mut hasher, max_bytes);
        serde_json::to_writer(&mut writer, raw).map_err(|error| {
            writer.take_overflow().map_or_else(
                || Error::Payload(error.to_string()),
                |actual| Error::BodyTooLarge {
                    limit: max_bytes,
                    actual,
                },
            )
        })?;
    }
    Ok(format!(
        "{SYNTHETIC_ID_PREFIX}{}",
        encode_hex(hasher.finalize().as_slice())
    ))
}

/// A `Write` adapter that hashes JSON bytes and stops at a hard input cap.
#[derive(Debug)]
struct DigestWriter<'a> {
    hasher: &'a mut Sha256,
    limit: usize,
    written: usize,
    overflow: Option<usize>,
}

impl<'a> DigestWriter<'a> {
    fn new(hasher: &'a mut Sha256, limit: usize) -> Self {
        Self {
            hasher,
            limit,
            written: 0,
            overflow: None,
        }
    }

    fn take_overflow(&mut self) -> Option<usize> {
        self.overflow.take()
    }
}

impl Write for DigestWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let total = self.written.saturating_add(buf.len());
        if total > self.limit {
            self.overflow = Some(total);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "digest input exceeds limit",
            ));
        }
        self.hasher.update(buf);
        self.written = total;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{identify, synthesize, PROVIDER_ID_PREFIX};
    use crate::error::Error;
    use crate::model::{Channel, SYNTHETIC_ID_PREFIX};
    use serde_json::{json, Value};

    #[test]
    fn a_provider_id_wins_over_synthesis() {
        let value = json!({ "id": "wamid.1" });
        assert_eq!(
            identify(&value, "id", &Channel::WhatsApp, "message").expect("provider id"),
            "wamid.1"
        );
    }

    #[test]
    fn a_provider_id_that_impersonates_the_synthetic_prefix_is_retagged() {
        let value = json!({ "id": "synthetic:x" });
        let id = identify(&value, "id", &Channel::WhatsApp, "message").expect("retagged id");
        assert_eq!(id, format!("{PROVIDER_ID_PREFIX}synthetic:x"));
        assert!(!id.starts_with(SYNTHETIC_ID_PREFIX));
    }

    #[test]
    fn an_empty_or_mistyped_id_is_synthesised() {
        for value in [json!({}), json!({ "id": "" }), json!({ "id": null })] {
            let id = identify(&value, "id", &Channel::WhatsApp, "message").expect("synthesised");
            assert!(
                id.starts_with(SYNTHETIC_ID_PREFIX),
                "expected a synthesised id, got {id:?}"
            );
        }
    }

    #[test]
    fn the_digest_ignores_key_order_and_tracks_content() {
        let first = synthesize("whatsapp", "message", &json!({ "a": 1, "b": 2 }), 64).expect("id");
        let reordered =
            synthesize("whatsapp", "message", &json!({ "b": 2, "a": 1 }), 64).expect("id");
        let changed =
            synthesize("whatsapp", "message", &json!({ "a": 1, "b": 3 }), 64).expect("id");
        assert_eq!(first, reordered, "key order must not change the id");
        assert_ne!(first, changed, "content must change the id");
    }

    #[test]
    fn channel_and_role_separate_otherwise_identical_bodies() {
        let body = json!({ "timestamp": 1 });
        let message = synthesize("whatsapp", "message", &body, 64).expect("id");
        let status = synthesize("whatsapp", "status", &body, 64).expect("id");
        let matrix = synthesize("matrix", "message", &body, 64).expect("id");
        assert_ne!(message, status);
        assert_ne!(message, matrix);
    }

    #[test]
    fn the_digest_stream_is_bounded_at_the_rendered_json_length() {
        let raw = Value::String("x".repeat(10));
        let rendered = raw.to_string();
        assert_eq!(rendered.len(), 12);

        assert!(synthesize("whatsapp", "message", &raw, rendered.len()).is_ok());

        let error = synthesize("whatsapp", "message", &raw, rendered.len() - 1)
            .expect_err("one byte below the rendered length");
        assert!(matches!(
            error,
            Error::BodyTooLarge {
                limit,
                actual
            } if limit == rendered.len() - 1 && actual == rendered.len()
        ));
    }
}
