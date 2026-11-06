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
//! * offsets are **UTF-8 byte offsets** into [`NormalizedMessage::text`](crate::NormalizedMessage::text)
//!   (`start` inclusive, `end` exclusive), never UTF-16 code units and never
//!   character indices, so `&text[span.start()..span.end()]` is always the run
//!   the span describes;
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
    #[error("span {start}..{end} does not fit in {len} bytes of text")]
    OutOfBounds {
        /// Offending start offset.
        start: usize,
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
/// assert_eq!(span.start(), 6);
/// assert_eq!(span.end(), 12);
/// assert_eq!(span.format(), &TextFormat::Italic);
/// assert_eq!(span.slice(text), Some("italic"));
/// # Ok::<(), messenger_webhook::SpanError>(())
/// ```
///
/// Spans cannot be built with a struct literal outside this crate:
///
/// ```compile_fail
/// use messenger_webhook::{TextFormat, TextSpan};
///
/// let _ = TextSpan {
///     start: 0,
///     end: 1,
///     format: TextFormat::Bold,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct TextSpan {
    start: usize,
    end: usize,
    format: TextFormat,
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
    /// assert_eq!(TextSpan::new(text, 0, 2, TextFormat::Bold)?.end(), 2);
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
                start,
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

    /// Byte offset of the first byte of the run, inclusive.
    ///
    /// ```
    /// use messenger_webhook::{TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// assert_eq!(span.start(), 6);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Byte offset one past the last byte of the run, exclusive.
    ///
    /// ```
    /// use messenger_webhook::{TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// assert_eq!(span.end(), 10);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub const fn end(&self) -> usize {
        self.end
    }

    /// Attribute that applies to the run.
    ///
    /// ```
    /// use messenger_webhook::{TextFormat, TextSpan};
    ///
    /// let span = TextSpan::new("Hallo Welt", 6, 10, TextFormat::Bold)?;
    /// assert_eq!(span.format(), &TextFormat::Bold);
    /// # Ok::<(), messenger_webhook::SpanError>(())
    /// ```
    #[must_use]
    pub const fn format(&self) -> &TextFormat {
        &self.format
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
/// Zero-length and inverted ranges also yield `None`, so a successful result is
/// always a non-empty byte range that [`TextSpan::new`] accepts.
///
/// Crate-private: the Telegram adapter is the only caller, and its tests
/// exercise the conversion through a real update body. The batch form returns
/// one slot per input range in provider order.
///
/// `string-offsets` 0.2.1 maps UTF-8 bytes, code points and lines *to* UTF-16
/// code units, but this adapter needs the inverse for Telegram's UTF-16 ranges
/// into a Rust UTF-8 `str`, and it has no mid-surrogate error to map onto this
/// crate's drop-the-entity behavior. Keep the inverse conversion in one local
/// helper with the boundary semantics `TextSpan` requires.
// Only the Telegram adapter counts units today; a Matrix or WhatsApp entity
// source would need the same helper, so it stays in one place.
#[cfg_attr(not(feature = "telegram"), allow(dead_code))]
pub(crate) fn utf16_to_byte_ranges<I>(text: &str, ranges: I) -> Vec<Option<(usize, usize)>>
where
    I: IntoIterator<Item = Option<(u64, u64)>>,
{
    let ranges = ranges.into_iter().collect::<Vec<_>>();
    if ranges.is_empty() {
        return Vec::new();
    }

    let mut boundaries = Vec::with_capacity(ranges.len().saturating_mul(2));
    for &(offset, length) in ranges.iter().flatten() {
        if let Some(end_unit) = offset.checked_add(length) {
            if end_unit <= offset {
                continue;
            }
            boundaries.push(offset);
            boundaries.push(end_unit);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut positions = Vec::with_capacity(boundaries.len());
    let mut next_boundary = 0;
    if boundaries.first() == Some(&0) {
        positions.push((0_u64, 0_usize));
        next_boundary = 1;
    }

    let mut units = 0_u64;
    for (byte, character) in text.char_indices() {
        units += if character.len_utf16() == 2 { 2 } else { 1 };
        let next_byte = byte + character.len_utf8();
        while let Some(&boundary) = boundaries.get(next_boundary) {
            if boundary > units {
                break;
            }
            if boundary == units {
                positions.push((boundary, next_byte));
            }
            next_boundary += 1;
        }
        if next_boundary == boundaries.len() {
            break;
        }
    }

    let byte_at = |unit| {
        positions
            .binary_search_by_key(&unit, |&(boundary, _)| boundary)
            .ok()
            .map(|index| positions[index].1)
    };

    ranges
        .into_iter()
        .map(|range| {
            let (offset, length) = range?;
            let end_unit = offset.checked_add(length)?;
            if end_unit <= offset {
                return None;
            }
            Some((byte_at(offset)?, byte_at(end_unit)?))
        })
        .collect()
}

/// Converts byte ranges into Telegram's UTF-16 code unit offsets.
///
/// Every successful range is a non-empty, char-boundary range of `text`, so the
/// returned pair is safe to put into a `MessageEntity`. A range that does not
/// slice `text` yields `None`; callers reject it instead of clamping it.
#[cfg_attr(not(feature = "telegram"), allow(dead_code))]
pub(crate) fn byte_to_utf16_ranges<I>(text: &str, ranges: I) -> Vec<Option<(u64, u64)>>
where
    I: IntoIterator<Item = Option<(usize, usize)>>,
{
    let mut offsets = Vec::new();
    let mut units = 0_u64;
    for (byte, character) in text.char_indices() {
        offsets.push((byte, units));
        units = units.saturating_add(u64::try_from(character.len_utf16()).unwrap_or(u64::MAX));
    }
    offsets.push((text.len(), units));

    ranges
        .into_iter()
        .map(|range| {
            let (start, end) = range?;
            if start >= end {
                return None;
            }
            let start_unit = offsets
                .binary_search_by_key(&start, |&(byte, _)| byte)
                .ok()
                .map(|index| offsets[index].1)?;
            let end_unit = offsets
                .binary_search_by_key(&end, |&(byte, _)| byte)
                .ok()
                .map(|index| offsets[index].1)?;
            (end_unit > start_unit).then_some((start_unit, end_unit - start_unit))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{byte_to_utf16_ranges, utf16_to_byte_ranges, SpanError, TextFormat, TextSpan};

    fn byte_to_utf16_range(text: &str, start: usize, end: usize) -> Option<(u64, u64)> {
        byte_to_utf16_ranges(text, [Some((start, end))])
            .into_iter()
            .next()
            .flatten()
    }

    fn utf16_to_byte_range(text: &str, offset: u64, length: u64) -> Option<(usize, usize)> {
        utf16_to_byte_ranges(text, [Some((offset, length))])
            .into_iter()
            .next()
            .flatten()
    }

    #[test]
    fn ascii_and_empty_text() {
        assert_eq!(utf16_to_byte_range("abc", 0, 1), Some((0, 1)));
        assert_eq!(utf16_to_byte_range("abc", 0, 3), Some((0, 3)));
        assert_eq!(utf16_to_byte_range("", 0, 0), None);
        assert_eq!(utf16_to_byte_range("", 0, 1), None);
        assert_eq!(utf16_to_byte_range("abc", 2, 9), None);
    }

    #[test]
    fn zero_length_ranges_are_rejected() {
        assert_eq!(utf16_to_byte_range("abc", 0, 0), None);
        assert_eq!(utf16_to_byte_range("abc", 1, 0), None);
        assert_eq!(utf16_to_byte_range("abc", 3, 0), None);
        assert_eq!(utf16_to_byte_range("abc", 4, 0), None);
    }

    #[test]
    fn astral_characters_count_twice() {
        // "a😀b": the emoji is 1 char, 2 UTF-16 units, 4 bytes.
        let text = "a😀b";
        assert_eq!(utf16_to_byte_range("😀b", 0, 2), Some((0, 4)));
        assert_eq!(utf16_to_byte_range(text, 1, 2), Some((1, 5)));
        assert_eq!(utf16_to_byte_range(text, 0, 4), Some((0, 6)));
        // The end unit can split the surrogate pair even when the start does
        // not; that range has no byte equivalent either.
        assert_eq!(utf16_to_byte_range("😀", 0, 1), None);
        assert_eq!(utf16_to_byte_range(text, 1, 1), None);
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

    fn utf16_units(text: &str) -> u64 {
        u64::try_from(text.encode_utf16().count()).unwrap_or(u64::MAX)
    }

    proptest::proptest! {
        #[test]
        fn every_byte_boundary_pair_round_trips(
            text in proptest::prelude::any::<String>(),
            a in proptest::prelude::any::<usize>(),
            b in proptest::prelude::any::<usize>(),
        ) {
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
                .collect();
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let lo = lo % boundaries.len();
            let hi = hi % boundaries.len();
            let start = boundaries[lo.min(hi)];
            let end = boundaries[lo.max(hi)];
            proptest::prop_assume!(start < end);

            let offset = utf16_units(&text[..start]);
            let length = utf16_units(&text[start..end]);
            proptest::prop_assert_eq!(
                utf16_to_byte_range(&text, offset, length),
                Some((start, end))
            );
        }

        #[test]
        fn successful_ranges_are_usable_spans(
            text in proptest::prelude::any::<String>(),
            offset in proptest::prelude::any::<u64>(),
            length in proptest::prelude::any::<u64>(),
        ) {
            if let Some((start, end)) = utf16_to_byte_range(&text, offset, length) {
                proptest::prop_assert!(start < end);
                proptest::prop_assert!(text.is_char_boundary(start));
                proptest::prop_assert!(text.is_char_boundary(end));
                proptest::prop_assert!(text.get(start..end).is_some());
                proptest::prop_assert!(
                    TextSpan::new(&text, start, end, TextFormat::Bold).is_ok()
                );
            }
        }

        #[test]
        fn every_char_boundary_pair_encodes_as_utf16(
            text in proptest::prelude::any::<String>(),
            a in proptest::prelude::any::<usize>(),
            b in proptest::prelude::any::<usize>(),
        ) {
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
                .collect();
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let lo = lo % boundaries.len();
            let hi = hi % boundaries.len();
            let start = boundaries[lo.min(hi)];
            let end = boundaries[lo.max(hi)];
            proptest::prop_assume!(start < end);

            let offset = utf16_units(&text[..start]);
            let length = utf16_units(&text[start..end]);
            proptest::prop_assert_eq!(
                byte_to_utf16_range(&text, start, end),
                Some((offset, length))
            );
        }
    }

    #[test]
    fn batched_conversion_matches_single_conversion() {
        let text = "a😀b€";
        assert_eq!(
            utf16_to_byte_ranges(
                text,
                [
                    Some((0, 5)),
                    Some((1, 2)),
                    Some((1, 4)),
                    Some((3, 1)),
                    Some((2, 1)),
                    Some((5, 1)),
                    Some((u64::MAX, 2)),
                    None,
                ],
            ),
            vec![
                Some((0, 9)),
                Some((1, 5)),
                Some((1, 9)),
                Some((5, 6)),
                None,
                None,
                None,
                None,
            ]
        );
    }

    #[test]
    fn batched_conversion_single_range_keeps_mid_surrogate_contract() {
        assert_eq!(
            utf16_to_byte_ranges("a😀b", [Some((1, 2))]),
            vec![Some((1, 5))]
        );
        assert_eq!(utf16_to_byte_ranges("a😀b", [Some((2, 1))]), vec![None]);
        assert_eq!(utf16_to_byte_ranges("a😀b", [Some((0, 5))]), vec![None]);
    }

    #[test]
    fn batched_conversion_handles_empty_and_single_ranges() {
        assert_eq!(
            utf16_to_byte_ranges("abc", [None::<(u64, u64)>; 0]),
            Vec::<Option<(usize, usize)>>::new()
        );
        assert_eq!(
            utf16_to_byte_ranges("abc", [Some((0, 3))]),
            vec![Some((0, 3))]
        );
    }

    #[test]
    fn batched_conversion_accepts_zero_and_duplicate_boundaries() {
        assert_eq!(
            utf16_to_byte_ranges(
                "😀x",
                [
                    Some((0, 0)),
                    Some((0, 2)),
                    Some((1, 1)),
                    Some((0, 3)),
                    Some((3, 0)),
                ],
            ),
            vec![None, Some((0, 4)), None, Some((0, 5)), None]
        );
        assert_eq!(
            utf16_to_byte_ranges("a😀b", [Some((1, 2)), Some((1, 1)), Some((2, 1))]),
            vec![Some((1, 5)), None, None]
        );
    }

    #[test]
    fn byte_ranges_encode_as_utf16_units() {
        let text = "a😀b€";
        assert_eq!(byte_to_utf16_range(text, 0, 1), Some((0, 1)));
        assert_eq!(byte_to_utf16_range(text, 1, 5), Some((1, 2)));
        assert_eq!(byte_to_utf16_range(text, 0, 6), Some((0, 4)));
        assert_eq!(byte_to_utf16_range(text, 5, 6), Some((3, 1)));
        assert_eq!(byte_to_utf16_range(text, 0, text.len()), Some((0, 5)));
    }

    #[test]
    fn byte_ranges_reject_ranges_that_do_not_slice_text() {
        let text = "a😀b";
        assert_eq!(byte_to_utf16_range(text, 0, 0), None);
        assert_eq!(byte_to_utf16_range(text, 3, 1), None);
        assert_eq!(byte_to_utf16_range(text, 0, text.len() + 1), None);
        assert_eq!(byte_to_utf16_range(text, 1, 3), None);
        assert_eq!(byte_to_utf16_range(text, 3, 5), None);
    }

    #[test]
    fn byte_to_utf16_ranges_preserve_provider_order_and_none_slots() {
        let text = "a😀b€";
        assert_eq!(
            byte_to_utf16_ranges(
                text,
                [Some((0, 6)), Some((1, 5)), Some((5, 6)), Some((1, 3)), None,],
            ),
            vec![Some((0, 4)), Some((1, 2)), Some((3, 1)), None, None]
        );
        assert_eq!(
            byte_to_utf16_ranges("abc", [None::<(usize, usize)>; 0]),
            Vec::<Option<(u64, u64)>>::new()
        );
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
            SpanError::OutOfBounds {
                start: 2,
                end: 9,
                len: 3
            }
            .to_string(),
            "span 2..9 does not fit in 3 bytes of text"
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
