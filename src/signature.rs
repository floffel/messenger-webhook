//! Signature construction and constant-time comparison.
//!
//! Parsing is strict here on purpose: a signature header either has the exact
//! documented shape or the request is rejected. Permissiveness belongs in the
//! payload parser, never in the authentication path.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::{Error, Result};
use crate::model::Channel;
use crate::secret::Secret;

/// Header Meta signs `WhatsApp` webhooks with.
pub const META_SIGNATURE_HEADER: &str = "x-hub-signature-256";

/// Prefix Meta puts in front of the hex digest.
pub const META_SIGNATURE_PREFIX: &str = "sha256=";

/// Length of a SHA-256 digest in bytes.
const DIGEST_LEN: usize = 32;

/// Computes the `X-Hub-Signature-256` value Meta would send for `body`.
///
/// Useful for building signed requests in tests or for mock providers.
///
/// ```
/// use messenger_webhook::{signature::meta_signature, Secret};
///
/// // RFC 4231 test case 2.
/// let value = meta_signature(&Secret::new("Jefe"), b"what do ya want for nothing?")?;
/// assert_eq!(
///     value,
///     "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
/// );
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
///
/// # Errors
///
/// Returns [`Error::MissingSecret`] when `secret` is empty: HMAC accepts a
/// zero-length key, so signing with it would produce a value anyone can forge.
/// Returns [`Error::Hmac`] if the HMAC construction rejects the key, which
/// cannot happen for HMAC-SHA256 and would still fail closed.
pub fn meta_signature(secret: &Secret, body: &[u8]) -> Result<String> {
    let digest = hmac_sha256(secret, body)?;
    Ok(format!("{META_SIGNATURE_PREFIX}{}", encode_hex(&digest)))
}

/// Verifies a Meta `X-Hub-Signature-256` header value against `body`.
///
/// The digest is decoded first and compared in constant time, so a wrong
/// signature leaks neither its content nor how much of it matched.
///
/// ```
/// use messenger_webhook::{signature, Secret};
///
/// let secret = Secret::new("app-secret");
/// let header = signature::meta_signature(&secret, b"{}")?;
/// signature::verify_meta(&secret, &header, b"{}")?;
/// assert!(signature::verify_meta(&secret, &header, b"{\"a\":1}").is_err());
///
/// // An unset secret fails closed instead of verifying with an empty key.
/// assert!(signature::verify_meta(&Secret::new(""), &header, b"{}").is_err());
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
///
/// # Errors
///
/// Returns [`Error::MissingSecret`] when `secret` is empty — before the header
/// is parsed and before anything is hashed. An empty key is a valid HMAC key,
/// so without this check `sha256=HMAC("", body)` would verify for every body,
/// letting an attacker sign arbitrary payloads.
/// Returns [`Error::MalformedVerificationHeader`] when the header is not
/// `sha256=` followed by 64 hex digits, and [`Error::BadSignature`] when the
/// digest does not match `body`. Returns [`Error::Hmac`] if the HMAC
/// construction rejects the key, which cannot happen for HMAC-SHA256 and would
/// still fail closed.
pub fn verify_meta(secret: &Secret, header: &str, body: &[u8]) -> Result<()> {
    let actual = hmac_sha256(secret, body)?;
    let hex = header.strip_prefix(META_SIGNATURE_PREFIX).ok_or_else(|| {
        Error::MalformedVerificationHeader {
            channel: Channel::WhatsApp,
            reason: format!("expected {META_SIGNATURE_PREFIX}<64 hex digits>"),
        }
    })?;
    let expected = decode_hex(hex).ok_or_else(|| Error::MalformedVerificationHeader {
        channel: Channel::WhatsApp,
        reason: "signature digest is not 64 hex digits".to_owned(),
    })?;
    if bool::from(expected.ct_eq(&actual)) {
        Ok(())
    } else {
        Err(Error::BadSignature {
            channel: Channel::WhatsApp,
        })
    }
}

/// HMAC-SHA256 as specified by RFC 2104.
pub(crate) fn hmac_sha256(secret: &Secret, message: &[u8]) -> Result<[u8; DIGEST_LEN]> {
    if secret.is_empty() {
        return Err(Error::MissingSecret {
            channel: Channel::WhatsApp,
        });
    }
    hmac_sha256_bytes(secret.expose_secret().as_bytes(), message).map_err(Error::from)
}

/// HMAC-SHA256 over raw key bytes.
///
/// Construction failure is surfaced as [`Error::Hmac`] rather than being
/// folded into [`Error::BadSignature`], so an internal fault stays
/// distinguishable from a forged request.
fn hmac_sha256_bytes(
    key: &[u8],
    message: &[u8],
) -> std::result::Result<[u8; DIGEST_LEN], hmac::digest::InvalidLength> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().into())
}

/// Lower-case hex encoding of a digest.
pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const_hex::encode(bytes)
}

/// Decodes exactly 64 hex digits (either case) into a 32-byte digest.
pub(crate) fn decode_hex(text: &str) -> Option<[u8; DIGEST_LEN]> {
    let bytes = text.as_bytes();
    if bytes.len() != DIGEST_LEN * 2 {
        return None;
    }
    const_hex::decode_to_array::<_, DIGEST_LEN>(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4231_vectors_hold() {
        // Case 1: 20-byte key of 0x0b.
        let key = [0x0bu8; 20];
        let digest = hmac_sha256_bytes(&key, b"Hi There").expect("hmac");
        assert_eq!(
            encode_hex(&digest),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        // Case 3: 20-byte key of 0xaa, 50 bytes of 0xdd.
        let key = [0xaau8; 20];
        let message = [0xddu8; 50];
        let digest = hmac_sha256_bytes(&key, &message).expect("hmac");
        assert_eq!(
            encode_hex(&digest),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );

        // Case 6: key longer than the block size (131 bytes of 0xaa).
        let key = [0xaau8; 131];
        let digest = hmac_sha256_bytes(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        )
        .expect("hmac");
        assert_eq!(
            encode_hex(&digest),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn hmac_sha256_refuses_empty_secret_at_the_primitive() {
        assert!(matches!(
            hmac_sha256(&Secret::new(""), b"{}"),
            Err(Error::MissingSecret {
                channel: Channel::WhatsApp
            })
        ));

        // HMAC itself accepts the empty key; the guard above is what keeps it
        // out of the crate-facing primitive.
        assert!(hmac_sha256_bytes(b"", b"{}").is_ok());

        let digest = hmac_sha256(&Secret::new("x"), b"{}").expect("one-byte key hashes");
        assert_eq!(digest, hmac_sha256_bytes(b"x", b"{}").expect("raw hmac"));
    }

    #[test]
    fn hex_roundtrip_and_rejects() {
        let digest: [u8; 32] = [0x00u8, 0x0f, 0xa0, 0xff]
            .repeat(8)
            .as_slice()
            .try_into()
            .expect("len");
        let text = encode_hex(&digest);
        assert_eq!(decode_hex(&text), Some(digest));
        assert_eq!(decode_hex(&text.to_uppercase()), Some(digest));
        assert_eq!(decode_hex(""), None);
        assert_eq!(decode_hex(&text[1..]), None, "63 nibbles is too short");
        assert_eq!(
            decode_hex(&format!("{text}0")),
            None,
            "65 nibbles is too long"
        );
        assert_eq!(decode_hex(&"g".repeat(64)), None, "non-hex digit");
        // Wrong *byte* count, rejected at the length check before any byte is
        // examined: "ü" is two UTF-8 bytes, so 64 of them are 128 bytes.
        assert_eq!(decode_hex(&"ü".repeat(64)), None, "128 bytes, not 64");
        // Right byte count, non-ASCII byte: 62 ASCII digits plus "ü" is
        // exactly 64 bytes, so this one has to be rejected by `hex_value`.
        assert_eq!(
            decode_hex(&format!("{}ü", "0".repeat(62))),
            None,
            "64 bytes containing a non-ASCII byte"
        );
        // Same for a single non-ASCII byte in the last position of an
        // otherwise valid digest.
        assert_eq!(
            decode_hex(&format!("{}é", &text[..62])),
            None,
            "valid prefix, multi-byte tail"
        );
    }

    #[test]
    fn secret_comparison_is_constant_time_and_length_aware() {
        let secret = Secret::new("abc");
        assert!(secret.ct_eq(b"abc"));
        assert!(!secret.ct_eq(b"abd"));
        assert!(!secret.ct_eq(b"ab"));
        assert!(!secret.ct_eq(b""));
    }

    #[test]
    fn verify_meta_classifies_malformed_headers_and_bad_signatures() {
        let secret = Secret::new("app-secret");
        let malformed_prefix = "expected sha256=<64 hex digits>".to_owned();
        let malformed_digest = "signature digest is not 64 hex digits".to_owned();
        let cases = [
            (
                "empty header",
                String::new(),
                Some(malformed_prefix.clone()),
            ),
            (
                "missing prefix",
                "a".repeat(64),
                Some(malformed_prefix.clone()),
            ),
            (
                "empty digest",
                "sha256=".to_owned(),
                Some(malformed_digest.clone()),
            ),
            (
                "63 hex digits",
                format!("sha256={}", "a".repeat(63)),
                Some(malformed_digest.clone()),
            ),
            ("64 hex digits", format!("sha256={}", "a".repeat(64)), None),
            (
                "65 hex digits",
                format!("sha256={}", "a".repeat(65)),
                Some(malformed_digest.clone()),
            ),
            (
                "wrong algorithm case",
                format!("SHA256={}", "a".repeat(64)),
                Some(malformed_prefix),
            ),
            (
                "doubled prefix",
                format!("sha256=sha256={}", "a".repeat(64)),
                Some(malformed_digest),
            ),
        ];

        let malformed_reason = |header: &str| match verify_meta(&secret, header, b"{}") {
            Err(Error::MalformedVerificationHeader { channel, reason }) => Some((channel, reason)),
            _ => None,
        };

        for (label, header, expected_reason) in cases {
            let expected = expected_reason.map(|reason| (Channel::WhatsApp, reason));
            assert_eq!(malformed_reason(&header), expected, "{label}");
        }

        let valid_for_other_body =
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
        assert!(matches!(
            verify_meta(&secret, valid_for_other_body, b"{}"),
            Err(Error::BadSignature {
                channel: Channel::WhatsApp
            })
        ));
    }
}
