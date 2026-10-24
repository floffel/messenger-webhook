//! HTTP headers, decoupled from any web framework.
//!
//! Adapters only need a handful of headers and must not force a caller onto a
//! particular server stack, so the verification API takes this small type
//! instead of `http::HeaderMap` or an axum extractor.

/// A case-insensitive set of request headers.
///
/// ```
/// use messenger_webhook::Headers;
///
/// let mut headers = Headers::new();
/// headers.insert("X-Hub-Signature-256", "sha256=abcd");
/// assert_eq!(headers.get("x-hub-signature-256"), Some("sha256=abcd"));
/// assert_eq!(headers.len(), 1);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    /// Creates an empty header set.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// assert!(Headers::new().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a header, lower-casing its name and replacing an existing entry.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// let mut headers = Headers::new();
    /// headers.insert("X-Telegram-Bot-Api-Secret-Token", "abc");
    /// headers.insert("x-telegram-bot-api-secret-token", "def");
    /// assert_eq!(headers.get("X-Telegram-Bot-Api-Secret-Token"), Some("def"));
    /// ```
    pub fn insert(&mut self, name: impl AsRef<str>, value: impl Into<String>) -> &mut Self {
        let name = name.as_ref().to_ascii_lowercase();
        let value = value.into();
        if let Some(entry) = self.entries.iter_mut().find(|(n, _)| *n == name) {
            entry.1 = value;
        } else {
            self.entries.push((name, value));
        }
        self
    }

    /// Looks a header up by name, case-insensitively.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// assert_eq!(Headers::new().get("authorization"), None);
    /// ```
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            let lower = name.to_ascii_lowercase();
            return self.lookup(&lower);
        }
        self.lookup(name)
    }

    /// Number of distinct headers.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// let headers = Headers::from_iter([("a", "1"), ("b", "2")]);
    /// assert_eq!(headers.len(), 2);
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the set is empty.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// assert!(Headers::new().is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn lookup(&self, lower: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == lower)
            .map(|(_, value)| value.as_str())
    }
}

impl<'a> FromIterator<(&'a str, &'a str)> for Headers {
    fn from_iter<T: IntoIterator<Item = (&'a str, &'a str)>>(iter: T) -> Self {
        let mut headers = Self::new();
        for (name, value) in iter {
            headers.insert(name, value);
        }
        headers
    }
}

impl<'a> From<&'a [(&'a str, &'a str)]> for Headers {
    fn from(pairs: &'a [(&'a str, &'a str)]) -> Self {
        pairs.iter().copied().collect()
    }
}
