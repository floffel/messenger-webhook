//! Identifiers for events the provider did not number.
//!
//! Every provider occasionally sends an object without its own id — a `WhatsApp`
//! status without `id`, a Matrix event without `event_id`, a Telegram update
//! whose `message_id` never made it onto the wire. Dropping those events loses
//! real traffic, so they are surfaced with a **synthesised** id instead: a
//! digest over the channel, the event role, the object's position in the
//! webhook envelope and the raw object.
//!
//! The digest is stable — the same webhook body always yields the same id, so a
//! caller's dedupe ledger keeps working across a provider retry — and it is
//! prefixed with [`SYNTHETIC_ID_PREFIX`], so an application can tell an invented
//! key from a provider-assigned one. The envelope position matters: two
//! byte-identical objects in different arrays, or at different indexes in the
//! same array, are distinct events and receive distinct ids.

use std::io::{self, Write};

use serde_json::Value;
use serde_json_canonicalizer::to_writer as canonical_json;
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
///
/// `discriminator` identifies the object's stable position in the webhook
/// envelope. It is only used when synthesis is needed.
pub(crate) fn identify(
    value: &Value,
    key: &str,
    channel: &Channel,
    role: &str,
    discriminator: &str,
) -> Result<String> {
    match crate::json::id(value, key) {
        Some(id) => Ok(retag_provider_id(id)),
        None => synthesize(channel.as_str(), role, discriminator, value, MAX_BODY_BYTES),
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
///
/// `discriminator` is the object's stable position in its webhook envelope. It
/// keeps two byte-identical objects at different positions distinct while a
/// provider retry of the same envelope keeps them stable.
pub(crate) fn synthesize(
    channel: &str,
    role: &str,
    discriminator: &str,
    raw: &Value,
    max_bytes: usize,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(channel.as_bytes());
    hasher.update([0]);
    hasher.update(role.as_bytes());
    hasher.update([0]);
    hasher.update(discriminator.as_bytes());
    hasher.update([0]);
    // Hash RFC 8785 JCS bytes rather than `Value::to_string`: the output is
    // stable even when a downstream consumer enables serde_json's
    // `preserve_order`, and `1` and `1.0` canonicalise to the same number.
    // Stream those bytes straight into the hasher through a bounded writer so
    // the only allocation left is the final digest's 64-character hex string.
    {
        let mut writer = DigestWriter::new(&mut hasher, max_bytes);
        canonical_json(raw, &mut writer).map_err(|error| {
            writer.take_overflow().map_or_else(
                || Error::from(error),
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
    use crate::MAX_BODY_BYTES;
    use serde_json::{json, Value};

    const PATH: &str = "/entry/0/changes/0/value/messages/0";

    #[test]
    fn a_provider_id_wins_over_synthesis() {
        let value = json!({ "id": "wamid.1" });
        assert_eq!(
            identify(&value, "id", &Channel::WhatsApp, "message", PATH).expect("provider id"),
            "wamid.1"
        );
    }

    #[test]
    fn a_provider_id_that_impersonates_the_synthetic_prefix_is_retagged() {
        let value = json!({ "id": "synthetic:x" });
        let id = identify(&value, "id", &Channel::WhatsApp, "message", PATH).expect("retagged id");
        assert_eq!(id, format!("{PROVIDER_ID_PREFIX}synthetic:x"));
        assert!(!id.starts_with(SYNTHETIC_ID_PREFIX));
    }

    #[test]
    fn an_empty_or_mistyped_id_is_synthesised() {
        for value in [json!({}), json!({ "id": "" }), json!({ "id": null })] {
            let id =
                identify(&value, "id", &Channel::WhatsApp, "message", PATH).expect("synthesised");
            assert!(
                id.starts_with(SYNTHETIC_ID_PREFIX),
                "expected a synthesised id, got {id:?}"
            );
        }
    }

    #[test]
    fn the_digest_ignores_key_order_and_tracks_content() {
        let first =
            synthesize("whatsapp", "message", PATH, &json!({ "a": 1, "b": 2 }), 64).expect("id");
        let reordered =
            synthesize("whatsapp", "message", PATH, &json!({ "b": 2, "a": 1 }), 64).expect("id");
        let changed =
            synthesize("whatsapp", "message", PATH, &json!({ "a": 1, "b": 3 }), 64).expect("id");
        assert_eq!(first, reordered, "key order must not change the id");
        assert_ne!(first, changed, "content must change the id");
    }

    #[test]
    fn jcs_equivalence_pins_numbers_and_nested_order() {
        let integer = synthesize(
            "whatsapp",
            "message",
            PATH,
            &json!({ "n": 1 }),
            MAX_BODY_BYTES,
        )
        .expect("id");
        let decimal = synthesize(
            "whatsapp",
            "message",
            PATH,
            &json!({ "n": 1.0 }),
            MAX_BODY_BYTES,
        )
        .expect("id");
        let nested = synthesize(
            "whatsapp",
            "message",
            PATH,
            &json!({ "outer": { "b": 1.0, "a": [3, 2, 1] } }),
            MAX_BODY_BYTES,
        )
        .expect("id");
        let nested_reordered = synthesize(
            "whatsapp",
            "message",
            PATH,
            &json!({ "outer": { "a": [3, 2, 1], "b": 1 } }),
            MAX_BODY_BYTES,
        )
        .expect("id");
        assert_eq!(integer, decimal, "1 and 1.0 are the same JCS number");
        assert_eq!(nested, nested_reordered, "JCS sorts nested object keys");
    }

    #[test]
    fn channel_and_role_separate_otherwise_identical_bodies() {
        let body = json!({ "timestamp": 1 });
        let message = synthesize("whatsapp", "message", PATH, &body, 64).expect("id");
        let status = synthesize("whatsapp", "status", PATH, &body, 64).expect("id");
        let matrix = synthesize("matrix", "message", PATH, &body, 64).expect("id");
        assert_ne!(message, status);
        assert_ne!(message, matrix);
    }

    #[test]
    fn the_envelope_discriminator_separates_identical_bodies() {
        let body = json!({ "type": "text", "text": { "body": "same" } });
        let first = synthesize(
            "whatsapp",
            "message",
            "/entry/0/changes/0/value/messages/0",
            &body,
            64,
        )
        .expect("id");
        let second = synthesize(
            "whatsapp",
            "message",
            "/entry/0/changes/0/value/messages/1",
            &body,
            64,
        )
        .expect("id");
        let replay = synthesize(
            "whatsapp",
            "message",
            "/entry/0/changes/0/value/messages/0",
            &body,
            64,
        )
        .expect("id");

        assert_ne!(first, second, "different positions are different events");
        assert_eq!(first, replay, "the same position is stable across replays");
    }

    #[test]
    fn the_digest_stream_is_bounded_at_the_rendered_json_length() {
        let raw = Value::String("x".repeat(10));
        let rendered = raw.to_string();
        assert_eq!(rendered.len(), 12);

        assert!(synthesize("whatsapp", "message", PATH, &raw, rendered.len()).is_ok());

        let error = synthesize("whatsapp", "message", PATH, &raw, rendered.len() - 1)
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
