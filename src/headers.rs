//! HTTP headers, decoupled from any web framework.
//!
//! Adapters only need a handful of headers and must not force a caller onto a
//! particular server stack, so the verification API takes this small type
//! instead of `http::HeaderMap` or an axum extractor.

use std::fmt;

/// A case-insensitive collection of request headers.
///
/// Header values are redacted in [`Debug`](fmt::Debug) output, so logging a
/// header set cannot expose credentials. Repeated fields are preserved when
/// built with [`Headers::from_pairs`], [`Headers::append`] or the iterator and
/// slice constructors; use [`Headers::insert`] to replace an existing field.
///
/// ```
/// use messenger_webhook::Headers;
///
/// let mut headers = Headers::new();
/// headers.insert("X-Hub-Signature-256", "sha256=abcd");
/// assert_eq!(headers.get("x-hub-signature-256"), Some("sha256=abcd"));
/// assert_eq!(headers.len(), 1);
///
/// let rendered = format!("{headers:?}");
/// assert!(rendered.contains("x-hub-signature-256"));
/// assert!(!rendered.contains("sha256=abcd"));
/// assert!(rendered.contains("<redacted>"));
/// ```
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

const REDACTED: &str = "<redacted>";

struct RedactedEntries<'a>(&'a [(String, String)]);

impl fmt::Debug for RedactedEntries<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| (name, REDACTED)))
            .finish()
    }
}

impl fmt::Debug for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Headers")
            .field("entries", &RedactedEntries(&self.entries))
            .finish()
    }
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

    /// Inserts a header, lower-casing its name and replacing existing entries.
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
        if let Some(index) = self.entries.iter().position(|(stored, _)| *stored == name) {
            self.entries[index].1 = value;
            let mut seen = false;
            self.entries.retain(|(stored, _)| {
                if *stored == name {
                    let keep = !seen;
                    seen = true;
                    keep
                } else {
                    true
                }
            });
            return self;
        }
        self.entries.push((name, value));
        self
    }

    /// Appends a header, lower-casing its name and preserving repeated fields.
    ///
    /// Use this when mapping a framework's header container into [`Headers`]:
    /// HTTP allows a field to repeat, and the verification path has to see each
    /// occurrence to enforce headers such as `X-Hub-Signature-256` exactly.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// let mut headers = Headers::new();
    /// headers.append("X-Hub-Signature-256", "sha256=one");
    /// headers.append("x-hub-signature-256", "sha256=two");
    /// assert_eq!(headers.len(), 2);
    /// assert_eq!(
    ///     headers.get_all("X-HUB-SIGNATURE-256").collect::<Vec<_>>(),
    ///     ["sha256=one", "sha256=two"]
    /// );
    /// ```
    pub fn append(&mut self, name: impl AsRef<str>, value: impl Into<String>) -> &mut Self {
        self.entries
            .push((name.as_ref().to_ascii_lowercase(), value.into()));
        self
    }

    /// Builds a header set from pairs whose values may be owned or borrowed.
    ///
    /// Repeated names are preserved. Use [`Headers::insert`] if you need
    /// last-write-wins normalisation instead.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// let headers = Headers::from_pairs([
    ///     ("Authorization", "Bearer t".to_owned()),
    ///     ("authorization", "Bearer u".to_owned()),
    /// ]);
    /// assert_eq!(headers.len(), 2);
    /// assert_eq!(
    ///     headers.get_all("Authorization").collect::<Vec<_>>(),
    ///     ["Bearer t", "Bearer u"]
    /// );
    /// ```
    #[must_use]
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut headers = Self::new();
        for (name, value) in pairs {
            headers.append(name.into(), value);
        }
        headers
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
        self.lookup(name)
    }

    /// Looks up every value stored for `name`, case-insensitively.
    ///
    /// Values are returned in insertion order. A name that was never inserted
    /// yields an empty iterator. This is the accessor verification uses when a
    /// header has to appear exactly once.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// let headers = Headers::from_iter([("X-Test", "one"), ("x-test", "two")]);
    ///
    /// assert_eq!(headers.get("X-Test"), Some("two"));
    /// assert_eq!(
    ///     headers.get_all("x-TEST").collect::<Vec<_>>(),
    ///     ["one", "two"]
    /// );
    /// assert_eq!(headers.get_all("missing").count(), 0);
    /// ```
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.entries.iter().filter_map(move |(stored, value)| {
            stored.eq_ignore_ascii_case(name).then_some(value.as_str())
        })
    }

    #[cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
    pub(crate) fn exactly_one(
        &self,
        channel: crate::Channel,
        name: &'static str,
    ) -> crate::Result<&str> {
        let mut values = self.get_all(name);
        let Some(value) = values.next() else {
            return Err(crate::Error::MissingVerificationHeader {
                channel,
                header: name,
            });
        };
        let count = 1 + values.count();
        if count > 1 {
            return Err(crate::Error::DuplicateVerificationHeader {
                channel,
                header: name,
                count,
            });
        }
        Ok(value)
    }

    /// Number of stored header fields.
    ///
    /// Repeated names count once per stored value.
    ///
    /// ```
    /// use messenger_webhook::Headers;
    ///
    /// let headers = Headers::from_iter([("a", "1"), ("b", "2"), ("a", "3")]);
    /// assert_eq!(headers.len(), 3);
    /// assert_eq!(headers.get_all("a").count(), 2);
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

    fn lookup(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|(stored, _)| stored.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

impl<'a> FromIterator<(&'a str, &'a str)> for Headers {
    fn from_iter<T: IntoIterator<Item = (&'a str, &'a str)>>(iter: T) -> Self {
        let mut headers = Self::new();
        for (name, value) in iter {
            headers.append(name, value);
        }
        headers
    }
}

impl<'a> From<&'a [(&'a str, &'a str)]> for Headers {
    fn from(pairs: &'a [(&'a str, &'a str)]) -> Self {
        pairs.iter().copied().collect()
    }
}
