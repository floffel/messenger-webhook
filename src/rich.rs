//! Rich text: formatting spans over a message's plain text.
//!
//! [`NormalizedMessage::text`](crate::NormalizedMessage::text) stays plain: the
//! provider's markup is carried as a list of [`TextSpan`]s whose offsets point
//! **into that plain text**, so a consumer that cannot render formatting still
//! gets readable text and a consumer that can render it gets the attributes
//! without re-parsing anything.
//!
//! Two properties are load bearing:
//!
//! * offsets are **UTF-8 byte offsets** into `NormalizedMessage::text`
//!   (`start` inclusive, `end` exclusive), never UTF-16 code units and never
//!   character indices, so `&text[span.start..span.end]` is always the run the
//!   span describes;
//! * a span can only be built through [`TextSpan::new`], which rejects empty,
//!   inverted, out-of-bounds and non-`char`-boundary ranges. A provider entity
//!   that cannot be expressed that way is dropped rather than clamped, so no
//!   span ever describes text the provider did not mark.
//!
//! Providers disagree about the unit. Telegram, the first adapter wired up
//! here, counts UTF-16 code units — an emoji outside the BMP is two of them —
//! so its offsets go through a UTF-16-to-bytes conversion before a span is built.
//!
//! ```
//! use messenger_webhook::{TextFormat, TextSpan};
//!
//! let text = "Hallo **Welt**";
//! let span = TextSpan::new(text, 6, 14, TextFormat::Bold)?;
//! assert_eq!(span.slice(text), Some("**Welt**"));
//! # Ok::<(), messenger_webhook::SpanError>(())
//! ```

use thiserror::Error;

/// Why a [`TextSpan`] could not be built.
///
/// Every variant is terminal: the provider sent offsets that do not describe a
/// run inside the text, and the adapter drops the span instead of guessing.
///
/// ```
/// use messenger_webhook::{SpanError, TextFormat, TextSpan};
///
/// let error = TextSpan::new("abc", 1, 9, TextFormat::Bold).unwrap_err();
/// assert!(matches!(error, SpanError::OutOfBounds { .. }));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SpanError {
    /// `start` is not before `end`, so the span covers nothing.
    #[error("span {start}..{end} is empty or inverted")]
    Empty {
        /// Offending start offset.
        start: usize,
        /// Offending end offset.
        end: usize,
    },

    /// The span ends past the end of the text.
    #[error("span 0..{end} does not fit in {len} bytes of text")]
    OutOfBounds {
        /// Offending end offset.
        end: usize,
        /// Length of the text in bytes.
        len: usize,
    },

    /// An offset falls inside a multi-byte character, so slicing there would
    /// panic.
    #[error("offset {offset} is not a character boundary of the text")]
    NotCharBoundary {
        /// Offending offset.
        offset: usize,
    },
}

/// One formatting attribute of a run of characters.
///
/// The enum covers what Telegram's `MessageEntity` documents; a type this crate
/// does not model arrives as [`TextFormat::Other`] with the provider's own name
/// instead of being turned into a parse error.
///
/// ```
/// use messenger_webhook::TextFormat;
///
/// assert_eq!(TextFormat::Bold.as_str(), "bold");
/// assert_eq!(TextFormat::Other("date_time".into()).as_str(), "date_time");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TextFormat {
    /// Bold run.
    Bold,
    /// Italic run.
    Italic,
    /// Underlined run.
    Underline,
    /// Struck-through run.
    Strikethrough,
    /// Hidden until the reader taps it.
    Spoiler,
    /// Inline monospace run.
    Code,
    /// Monospace block, optionally tagged with a language.
    Pre {
        /// Language tag the provider attached, if any.
        language: Option<String>,
    },
    /// Hyperlink whose target is not the visible text.
    Link {
        /// Target of the link, as the provider sent it.
        url: String,
    },
    /// `@username` mention.
    Mention,
    /// `#tag`.
    Hashtag,
    /// `$TICKER`.
    Cashtag,
    /// `/command`.
    BotCommand,
    /// Bare URL the provider recognised.
    Url,
    /// Bare e-mail address.
    Email,
    /// Bare phone number.
    PhoneNumber,
    /// Mention of a user by id rather than by username.
    TextMention {
        /// Provider user id.
        user_id: String,
    },
    /// Custom (premium) emoji, identified by its provider id.
    CustomEmoji {
        /// Provider id of the emoji set entry.
        id: String,
    },
    /// Quoted block.
    Blockquote,
    /// Quoted block that starts collapsed.
    ExpandableBlockquote,
    /// Anything the provider sent that this crate does not model, verbatim.
    Other(String),
}

impl TextFormat {
    /// Stable lower-case name, used in logs and in equality checks against a
    /// provider's own entity type.
    ///
    /// ```
    /// use messenger_webhook::TextFormat;
    ///
    /// assert_eq!(TextFormat::Pre { language: None }.as_str(), "pre");
    /// assert_eq!(TextFormat::Link { url: "https://example.com".into() }.as_str(), "link");
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Bold => "bold",
            Self::Italic => "italic",
            Self::Underline => "underline",
            Self::Strikethrough => "strikethrough",
            Self::Spoiler => "spoiler",
            Self::Code => "code",
            Self::Pre { .. } => "pre",
            Self::Link { .. } => "link",
            Self::Mention => "mention",
            Self::Hashtag => "hashtag",
            Self::Cashtag => "cashtag",
            Self::BotCommand => "bot_command",
            Self::Url => "url",
            Self::Email => "email",
            Self::PhoneNumber => "phone_number",
            Self::TextMention { .. } => "text_mention",
            Self::CustomEmoji { .. } => "custom_emoji",
            Self::Blockquote => "blockquote",
            Self::ExpandableBlockquote => "expandable_blockquote",
            Self::Other(name) => name,
        }
    }
}

/// A formatted run inside [`NormalizedMessage::text`](crate::NormalizedMessage::text).
///
/// ```
/// use messenger_webhook::{TextFormat, TextSpan};
///
/// let text = "plain italic plain";
/// let span = TextSpan::new(text, 6, 12, TextFormat::Italic)?;
/// assert_eq!(span.start, 6);
/// assert_eq!(span.end, 12);
/// assert_eq!(span.slice(text), Some("italic"));
/// # Ok::<(), messenger_webhook::SpanError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextSpan {
    /// Byte offset of the first byte of the run, inclusive.
    pub start: usize,
    /// Byte offset one past the last byte of the run, exclusive.
    pub end: usize,
    /// Attribute that applies to the run.
    pub format: TextFormat,
}

impl TextSpan {
    /// Builds a span, rejecting every range that would not slice `text`.
    ///
    /// # Errors
    ///
    /// [`SpanError::Empty`] when `start >= end`, [`SpanError::OutOfBounds`]
    /// when `end` is past the text and [`SpanError::NotCharBoundary`] when an
    /// offset splits a multi-byte character. Callers drop such a span instead
    /// of repairing it: a span is a claim about the provider's markup, and a
    /// repaired claim is a wrong one.
    ///
    /// ```
    /// use messenger_webhook::{SpanError, TextFormat, TextSpan};
    ///
    /// let text = "ä";  // two bytes
    /// assert!(TextSpan::new(text, 0, 1, TextFormat::Bold).is_err());
    /// assert_eq!(TextSpan::new(text, 0, 2, TextFormat::Bold)?.end, 2);
    /// # Ok::<(), SpanError>(())
    /// ```
    pub fn new(
        text: &str,
        start: usize,
        end: usize,
        format: TextFormat,
    ) -> Result<Self, SpanError> {
        if start >= end {
            return Err(SpanError::Empty { start, end });
        }
        if end > text.len() {
            return Err(SpanError::OutOfBounds {
                end,
                len: text.len(),
            });
        }
        // `start < end <= len`, so only `start` can still be inside a character
        // once `end` is checked below.
        if !text.is_char_boundary(start) {
            return Err(SpanError::NotCharBoundary { offset: start });
        }
        if !text.is_char_boundary(end) {
            return Err(SpanError::NotCharBoundary { offset: end });
        }
        Ok(Self { start, end, format })
    }

    /// The run this span describes, borrowed from the text it was built for.
    ///
    /// Returns `None` when the span does not belong to `text` — the caller
    /// handed in the wrong string, and guessing a run would be worse than
    /// saying so.
    ///
    /// ```
    /// use messenger_webhook::{TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// assert_eq!(span.slice("Hallo Welt"), Some("Welt"));
    /// assert_eq!(span.slice("servus"), None);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn slice<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.get(self.start..self.end)
    }

    /// Whether the run contains the byte at `offset`.
    ///
    /// Used by renderers that walk the text once and ask which attributes apply
    /// at the current position.
    ///
    /// ```
    /// use messenger_webhook::{TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// assert!(span.covers(6));
    /// assert!(!span.covers(10));
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub fn covers(&self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }
}

/// Converts a provider range counted in UTF-16 code units into byte offsets.
///
/// Telegram's `MessageEntity.offset` and `.length` are UTF-16 code units, a
/// unit no Rust string has: an emoji outside the BMP is two units and four
/// bytes. A range that starts or ends inside a surrogate pair has no byte
/// equivalent and yields `None`, as does one that runs past the end of `text`.
///
/// Crate-private: the Telegram adapter is the only caller, and its tests
/// exercise the conversion through a real update body.
// Only the Telegram adapter counts units today; a Matrix or WhatsApp entity
// source would need the same helper, so it stays in one place.
#[cfg_attr(not(feature = "telegram"), allow(dead_code))]
pub(crate) fn utf16_to_byte_range(text: &str, offset: u64, length: u64) -> Option<(usize, usize)> {
    let end_unit = offset.checked_add(length)?;
    let mut units = 0_u64;
    let mut start = (offset == 0).then_some(0_usize);
    let mut end = (end_unit == 0).then_some(0_usize);
    if start.is_some() && end.is_some() {
        return Some((0, 0));
    }
    for (byte, character) in text.char_indices() {
        units += u64::try_from(character.len_utf16()).ok()?;
        let next = byte + character.len_utf8();
        // A range boundary is only usable when it lands exactly on the end of a
        // character; landing mid-surrogate leaves the slot `None`.
        if units == offset {
            start = Some(next);
        }
        if units == end_unit {
            end = Some(next);
        }
        if start.is_some() && end.is_some() {
            break;
        }
    }
    Some((start?, end?))
}

#[cfg(test)]
mod tests {
    use super::{utf16_to_byte_range, SpanError, TextFormat, TextSpan};

    #[test]
    fn ascii_and_empty_text() {
        assert_eq!(utf16_to_byte_range("abc", 0, 3), Some((0, 3)));
        assert_eq!(utf16_to_byte_range("", 0, 0), Some((0, 0)));
        assert_eq!(utf16_to_byte_range("", 0, 1), None);
        assert_eq!(utf16_to_byte_range("abc", 2, 9), None);
    }

    #[test]
    fn astral_characters_count_twice() {
        // "a😀b": the emoji is 1 char, 2 UTF-16 units, 4 bytes.
        let text = "a😀b";
        assert_eq!(utf16_to_byte_range(text, 1, 2), Some((1, 5)));
        assert_eq!(utf16_to_byte_range(text, 0, 4), Some((0, 6)));
        // A boundary inside the surrogate pair is unrepresentable.
        assert_eq!(utf16_to_byte_range(text, 2, 1), None);
    }

    #[test]
    fn offset_past_the_end_is_rejected() {
        assert_eq!(utf16_to_byte_range("ab", 0, 3), None);
        assert_eq!(utf16_to_byte_range("ab", 3, 0), None);
    }

    #[test]
    fn overflowing_unit_sum_is_rejected() {
        assert_eq!(utf16_to_byte_range("ab", u64::MAX, 2), None);
    }

    #[test]
    fn span_constructor_rejects_every_bad_range() {
        assert!(matches!(
            TextSpan::new("abc", 2, 2, TextFormat::Bold),
            Err(SpanError::Empty { .. })
        ));
        assert!(matches!(
            TextSpan::new("abc", 2, 1, TextFormat::Bold),
            Err(SpanError::Empty { .. })
        ));
        assert!(matches!(
            TextSpan::new("abc", 0, 4, TextFormat::Bold),
            Err(SpanError::OutOfBounds { .. })
        ));
        assert!(matches!(
            TextSpan::new("äb", 1, 3, TextFormat::Bold),
            Err(SpanError::NotCharBoundary { .. })
        ));
    }

    #[test]
    fn span_error_messages_name_the_numbers() {
        assert_eq!(
            SpanError::Empty { start: 2, end: 2 }.to_string(),
            "span 2..2 is empty or inverted"
        );
        assert_eq!(
            SpanError::OutOfBounds { end: 9, len: 3 }.to_string(),
            "span 0..9 does not fit in 3 bytes of text"
        );
        assert_eq!(
            SpanError::NotCharBoundary { offset: 1 }.to_string(),
            "offset 1 is not a character boundary of the text"
        );
    }

    #[test]
    fn covers_uses_half_open_range() {
        let span = TextSpan::new("abcdef", 2, 4, TextFormat::Code).expect("span");
        assert!(!span.covers(1));
        assert!(span.covers(2));
        assert!(span.covers(3));
        assert!(!span.covers(4));
    }
}
