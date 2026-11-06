//! Verification secrets that never leak into logs or timing.

use std::fmt;

use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// A shared secret used to authenticate provider webhooks.
///
/// The value is redacted in [`Debug`](fmt::Debug) output and can only be
/// compared in constant time through [`Secret::ct_eq`]. An empty secret is
/// accepted as a value, but every adapter refuses to verify with it.
///
/// The bytes live in a [`zeroize::Zeroizing`] buffer, so the heap allocation
/// backing the secret is wiped when the value is dropped. [`Clone`] is a deep
/// copy — cloning an adapter gives the copy its own independent, zeroizing
/// buffer — so no clone leaves the secret behind in freed memory.
///
/// ```
/// use messenger_webhook::Secret;
///
/// let secret = Secret::new("s3cret");
/// assert!(!secret.is_empty());
/// assert!(secret.ct_eq(b"s3cret"));
/// assert!(!secret.ct_eq(b"s3creT"));
/// assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
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
    /// Wraps a raw secret value.
    ///
    /// ```
    /// use messenger_webhook::Secret;
    ///
    /// assert!(Secret::new("").is_empty());
    /// ```
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// Whether the secret is empty — which makes every `verify` call fail
    /// closed with [`crate::Error::MissingSecret`].
    ///
    /// ```
    /// use messenger_webhook::Secret;
    ///
    /// assert!(Secret::new("").is_empty());
    /// assert!(!Secret::new("x").is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrows the raw value. Treat the result as a credential: never log it.
    ///
    /// ```
    /// use messenger_webhook::Secret;
    ///
    /// assert_eq!(Secret::new("abc").expose_secret(), "abc");
    /// ```
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }

    /// Compares a candidate in constant time.
    ///
    /// ```
    /// use messenger_webhook::Secret;
    ///
    /// let secret = Secret::new("token");
    /// assert!(secret.ct_eq(b"token"));
    /// assert!(!secret.ct_eq(b"toke"));
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
    use super::Secret;

    #[test]
    fn clone_is_an_independent_deep_copy() {
        let original = Secret::new("app-secret");
        let clone = original.clone();

        // Both share the same bytes...
        assert!(clone.ct_eq(b"app-secret"));
        assert_eq!(clone.expose_secret(), "app-secret");
        // ...and dropping one leaves the other valid.
        drop(original);
        assert!(clone.ct_eq(b"app-secret"));
        assert_eq!(format!("{clone:?}"), "Secret(<redacted>)");
    }

    #[test]
    fn clone_wipes_its_own_bytes_on_drop() {
        // `Secret` owns a `Zeroizing<String>`, so the allocation is zeroed when
        // the last owner is dropped. This pins that guarantee in place: a
        // future change that reverts to a bare `String` no longer wipes.
        fn assert_zeroizing(_: &zeroize::Zeroizing<String>) {}

        let secret = Secret::new(String::from("app-secret"));
        // The inner buffer is a `Zeroizing`, so cloning and dropping both wipe.
        let clone = secret.clone();
        assert_zeroizing(&secret.0);
        assert_zeroizing(&clone.0);
        assert!(secret.ct_eq(b"app-secret"));
    }

    #[test]
    fn secret_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Secret>();
    }
}
