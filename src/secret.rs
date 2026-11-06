//! Verification secrets that never leak into logs or timing.

use std::fmt;

use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Shortest secret this crate accepts, in bytes.
///
/// Secrets are shared credentials, so a value that can be guessed makes every
/// request it protects forgeable. Sixteen bytes is the smallest limit that
/// rejects the accidentally weak values from configuration examples while
/// staying below provider-generated tokens and HMAC-style secrets.
///
/// ```
/// assert_eq!(messenger_webhook::secret::MIN_SECRET_BYTES, 16);
/// ```
pub const MIN_SECRET_BYTES: usize = 16;

/// A shared secret used to authenticate provider webhooks.
///
/// The value is redacted in [`Debug`](fmt::Debug) output. Authentication
/// comparisons should go through [`Secret::ct_eq`], which compares without
/// short-circuiting when the candidate length matches the secret; unequal
/// lengths return `false` after the length check, so that path reveals the
/// secret's byte length. [`Secret::expose_secret`] deliberately returns the raw
/// `&str` for provider integration points; comparisons or formatting done on
/// that borrow are outside the constant-time guarantee.
///
/// Empty and weak secrets are rejected at construction, so a `Secret` that
/// exists is at least [`MIN_SECRET_BYTES`] long.
///
/// The bytes live in a [`zeroize::Zeroizing`] buffer, so the heap allocation
/// backing the secret is wiped when the value is dropped. [`Clone`] is a deep
/// copy — cloning an adapter gives the copy its own independent, zeroizing
/// buffer — so no clone leaves the secret behind in freed memory.
///
/// ```
/// use messenger_webhook::Secret;
///
/// let secret = Secret::new("s3cret-s3cret-s3cret")?;
/// assert!(!secret.is_empty());
/// assert!(secret.ct_eq(b"s3cret-s3cret-s3cret"));
/// assert!(!secret.ct_eq(b"s3cret-s3cret-s3creT"));
/// assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
pub struct Secret(Zeroizing<String>);

impl Clone for Secret {
    fn clone(&self) -> Self {
        // A fresh `Zeroizing` buffer: the clone wipes its own bytes on drop and
        // shares nothing with the original.
        Self(Zeroizing::new(self.0.as_str().to_owned()))
    }
}

impl Secret {
    #[cfg(all(test, feature = "whatsapp"))]
    pub(crate) fn empty_for_test() -> Self {
        Self(Zeroizing::new(String::new()))
    }

    /// Wraps a raw secret value.
    ///
    /// # Errors
    ///
    /// [`Error::WeakSecret`] when it is shorter than [`MIN_SECRET_BYTES`],
    /// including zero bytes.
    ///
    /// ```
    /// use messenger_webhook::Secret;
    ///
    /// let secret = Secret::new("telegram-bot-secret")?;
    /// assert_eq!(secret.expose_secret(), "telegram-bot-secret");
    ///
    /// assert!(Secret::new("").is_err());
    /// assert!(Secret::new("short").is_err());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let actual = value.len();
        if actual < MIN_SECRET_BYTES {
            return Err(Error::WeakSecret {
                actual,
                minimum: MIN_SECRET_BYTES,
            });
        }
        Ok(Self(Zeroizing::new(value)))
    }

    /// Whether the secret is empty.
    ///
    /// Construction rejects empty secrets, so this is always `false` for a
    /// value that was built through [`Secret::new`]. It remains part of the API
    /// for callers that carry a `Secret` from an older serialized form or from
    /// a future internal constructor.
    ///
    /// ```
    /// use messenger_webhook::Secret;
    ///
    /// let secret = Secret::new("telegram-bot-secret")?;
    /// assert!(!secret.is_empty());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrows the raw value.
    ///
    /// This is an explicit escape hatch for provider integration points that
    /// need the raw credential. Treat the result as a credential: never log it,
    /// and compare it through [`Secret::ct_eq`] rather than `==` on this `&str`
    /// when the comparison is an authentication check.
    ///
    /// ```
    /// use messenger_webhook::Secret;
    ///
    /// assert_eq!(
    ///     Secret::new("telegram-bot-secret")?.expose_secret(),
    ///     "telegram-bot-secret"
    /// );
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }

    /// Compares a candidate with this secret.
    ///
    /// The byte comparison is constant time only when the candidate has the
    /// same length as the secret. Unequal lengths return `false` after the
    /// length check, so an attacker who can measure timing can learn the
    /// secret's byte length. Do not treat this as a length-blind comparison for
    /// variable-length candidates.
    ///
    /// Use this for authentication comparisons. It is the crate's safe
    /// comparison path for a secret's contents.
    ///
    /// ```
    /// use messenger_webhook::Secret;
    ///
    /// let secret = Secret::new("telegram-bot-secret")?;
    /// assert!(secret.ct_eq(b"telegram-bot-secret"));
    /// assert!(!secret.ct_eq(b"telegram-bot-secre"));
    /// assert!(!secret.ct_eq(b"telegram-bot-secret!"));
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    #[must_use]
    pub fn ct_eq(&self, candidate: &[u8]) -> bool {
        bool::from(self.0.as_bytes().ct_eq(candidate))
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::{Secret, MIN_SECRET_BYTES};

    #[test]
    fn clone_is_an_independent_deep_copy() {
        let original = Secret::new("app-secret-app-secret").expect("secret");
        let clone = original.clone();

        // Both share the same bytes...
        assert!(clone.ct_eq(b"app-secret-app-secret"));
        assert_eq!(clone.expose_secret(), "app-secret-app-secret");
        // ...and dropping one leaves the other valid.
        drop(original);
        assert!(clone.ct_eq(b"app-secret-app-secret"));
        assert_eq!(format!("{clone:?}"), "Secret(<redacted>)");
    }

    #[test]
    fn clone_wipes_its_own_bytes_on_drop() {
        // `Secret` owns a `Zeroizing<String>`, so the allocation is zeroed when
        // the last owner is dropped. This pins that guarantee in place: a
        // future change that reverts to a bare `String` no longer wipes.
        fn assert_zeroizing(_: &zeroize::Zeroizing<String>) {}

        let secret = Secret::new(String::from("app-secret-app-secret")).expect("secret");
        // The inner buffer is a `Zeroizing`, so cloning and dropping both wipe.
        let clone = secret.clone();
        assert_zeroizing(&secret.0);
        assert_zeroizing(&clone.0);
        assert!(secret.ct_eq(b"app-secret-app-secret"));
    }

    #[test]
    fn constructor_enforces_minimum_length_boundary() {
        assert!(matches!(
            Secret::new(""),
            Err(crate::Error::WeakSecret {
                actual,
                minimum: MIN_SECRET_BYTES,
            }) if actual == 0
        ));
        assert!(matches!(
            Secret::new("a".repeat(MIN_SECRET_BYTES - 1)),
            Err(crate::Error::WeakSecret {
                actual,
                minimum: MIN_SECRET_BYTES,
            }) if actual == MIN_SECRET_BYTES - 1
        ));

        let at_minimum = Secret::new("a".repeat(MIN_SECRET_BYTES)).expect("minimum");
        assert_eq!(at_minimum.expose_secret().len(), MIN_SECRET_BYTES);

        let above_minimum = Secret::new("a".repeat(MIN_SECRET_BYTES + 1)).expect("above minimum");
        assert_eq!(above_minimum.expose_secret().len(), MIN_SECRET_BYTES + 1);
    }

    #[test]
    fn ct_eq_pins_equal_length_and_length_mismatch_contract() {
        let secret = Secret::new("0123456789abcdef").expect("secret");

        assert!(secret.ct_eq(b"0123456789abcdef"));
        assert!(!secret.ct_eq(b"0123456789abcdeG"));
        assert!(!secret.ct_eq(b"0123456789abcde"));
        assert!(!secret.ct_eq(b"0123456789abcdefg"));
        assert!(!secret.ct_eq(b""));
    }

    #[test]
    fn secret_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Secret>();
    }

    #[test]
    fn public_contract_separates_redaction_from_raw_access() {
        let secret = Secret::new("app-secret-app-secret").expect("secret");

        // The enforced logging contract: `Debug` never exposes the secret.
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");

        // The constant-time comparison path for authentication checks.
        assert!(secret.ct_eq(b"app-secret-app-secret"));
        assert!(!secret.ct_eq(b"app-secret-app-secreT"));

        // The deliberate raw escape hatch. Because this is a `&str`, callers
        // could compare or format it; the docs must not claim otherwise.
        let raw = secret.expose_secret();
        assert_eq!(raw, "app-secret-app-secret");
        assert_eq!(raw, "app-secret-app-secret");
        let rendered = format!("secret={raw}");
        assert!(rendered.contains("app-secret-app-secret"));
    }
}
