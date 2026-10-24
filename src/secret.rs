//! Verification secrets that never leak into logs or timing.

use std::fmt;

use subtle::ConstantTimeEq;

/// A shared secret used to authenticate provider webhooks.
///
/// The value is redacted in [`Debug`](fmt::Debug) output and can only be
/// compared in constant time through [`Secret::ct_eq`]. An empty secret is
/// accepted as a value, but every adapter refuses to verify with it.
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
#[derive(Clone)]
pub struct Secret(String);

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
        Self(value.into())
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
        &self.0
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
