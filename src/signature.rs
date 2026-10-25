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
/// Returns [`Error::BadSignature`] if the HMAC construction rejects the key,
/// which cannot happen for HMAC-SHA256 and would still fail closed.
pub fn meta_signature(secret: &Secret, body: &[u8]) -> Result<String> {
    let digest =
        hmac_sha256(secret.expose_secret().as_bytes(), body).ok_or(Error::BadSignature {
            channel: Channel::WhatsApp,
        })?;
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
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
///
/// # Errors
///
/// Returns [`Error::MalformedVerificationHeader`] when the header is not
/// `sha256=` followed by 64 hex digits, and [`Error::BadSignature`] when the
/// digest does not match `body`.
pub fn verify_meta(secret: &Secret, header: &str, body: &[u8]) -> Result<()> {
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
    let actual =
        hmac_sha256(secret.expose_secret().as_bytes(), body).ok_or(Error::BadSignature {
            channel: Channel::WhatsApp,
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
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> Option<[u8; DIGEST_LEN]> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
    mac.update(message);
    mac.finalize().into_bytes().as_slice().try_into().ok()
}

/// Lower-case hex encoding of a digest.
pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

const fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + value - 10) as char,
    }
}

/// Decodes exactly 64 hex digits (either case) into a 32-byte digest.
pub(crate) fn decode_hex(text: &str) -> Option<[u8; DIGEST_LEN]> {
    let bytes = text.as_bytes();
    if bytes.len() != DIGEST_LEN * 2 {
        return None;
    }
    let mut out = [0u8; DIGEST_LEN];
    let (pairs, _) = bytes.as_chunks::<2>();
    for (slot, pair) in out.iter_mut().zip(pairs) {
        *slot = hex_value(pair[0])? * 16 + hex_value(pair[1])?;
    }
    Some(out)
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4231_vectors_hold() {
        // Case 1: 20-byte key of 0x0b.
        let key = [0x0bu8; 20];
        let digest = hmac_sha256(&key, b"Hi There").expect("hmac");
        assert_eq!(
            encode_hex(&digest),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        // Case 3: 20-byte key of 0xaa, 50 bytes of 0xdd.
        let key = [0xaau8; 20];
        let message = [0xddu8; 50];
        let digest = hmac_sha256(&key, &message).expect("hmac");
        assert_eq!(
            encode_hex(&digest),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );

        // Case 6: key longer than the block size (131 bytes of 0xaa).
        let key = [0xaau8; 131];
        let digest = hmac_sha256(
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
        assert_eq!(decode_hex(&"ü".repeat(64)), None, "non-ascii");
    }

    #[test]
    fn secret_comparison_is_constant_time_and_length_aware() {
        let secret = Secret::new("abc");
        assert!(secret.ct_eq(b"abc"));
        assert!(!secret.ct_eq(b"abd"));
        assert!(!secret.ct_eq(b"ab"));
        assert!(!secret.ct_eq(b""));
    }
}
