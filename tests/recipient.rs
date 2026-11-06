//! The shared outbound recipient validator: every adapter inherits the same
//! byte ceiling and per-channel shape rule, and each boundary is proven from
//! both sides (empty, one, max, max + 1).

#![cfg(any(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(feature = "matrix")]
use messenger_webhook::MatrixAdapter;
#[cfg(feature = "telegram")]
use messenger_webhook::TelegramAdapter;
#[cfg(any(feature = "whatsapp", feature = "matrix"))]
use messenger_webhook::MAX_RECIPIENT_BYTES;
use messenger_webhook::{Adapter, Error, OutboundMessage};
#[cfg(feature = "whatsapp")]
use messenger_webhook::{Channel, WhatsAppAdapter};

#[cfg(feature = "whatsapp")]
fn whatsapp() -> WhatsAppAdapter {
    WhatsAppAdapter::new("secret")
}

#[cfg(feature = "telegram")]
fn telegram() -> TelegramAdapter {
    TelegramAdapter::new("secret")
}

#[cfg(feature = "matrix")]
fn matrix() -> MatrixAdapter {
    MatrixAdapter::new("secret")
}

/// Encodes `id` through `adapter` and reports whether it was accepted.
fn accepts(adapter: &dyn Adapter, id: &str) -> bool {
    let message = OutboundMessage::text(adapter.channel(), id, "hi");
    match adapter.encode(&message) {
        Ok(_) => true,
        Err(Error::InvalidMessage { .. }) => false,
        Err(other) => panic!("unexpected error for {id:?}: {other:?}"),
    }
}

#[cfg(feature = "whatsapp")]
#[test]
fn whatsapp_recipient_boundaries() {
    let adapter = whatsapp();

    // The empty and whitespace-only values are rejected.
    assert!(!accepts(&adapter, ""));
    assert!(!accepts(&adapter, "   "));
    assert!(!accepts(&adapter, "\t\n"));

    // Just inside the ceiling (1 byte, then exactly the ceiling).
    assert!(accepts(&adapter, "1"));
    assert!(accepts(&adapter, &"4".repeat(MAX_RECIPIENT_BYTES)));
    // One past: rejected, not silently truncated.
    assert!(!accepts(&adapter, &"4".repeat(MAX_RECIPIENT_BYTES + 1)));

    // WhatsApp is E.164 digits only.
    assert!(accepts(&adapter, "4915112345678"));
    assert!(!accepts(&adapter, "+4915112345678"));
    assert!(!accepts(&adapter, "49 151"));
    assert!(!accepts(&adapter, "49\n15"));
    assert!(!accepts(&adapter, "49\u{0}15"));
    assert!(!accepts(&adapter, "49\u{200b}15"));
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_recipient_boundaries() {
    let adapter = telegram();

    assert!(!accepts(&adapter, ""));
    assert!(!accepts(&adapter, "   "));

    assert!(accepts(&adapter, "1"));

    // Telegram chat ids are signed integers, so a leading `-` is legal and the
    // shape boundary is `i64`, not the byte ceiling (which it can never reach).
    assert!(accepts(&adapter, "-1001"));
    assert!(accepts(&adapter, "9223372036854775807"));
    assert!(accepts(&adapter, "-9223372036854775808"));
    assert!(!accepts(&adapter, "9223372036854775808"));
    assert!(!accepts(&adapter, "chat-1001"));
    // Surrounding whitespace is trimmed by design; interior controls are not.
    assert!(accepts(&adapter, " -1001 "));
    assert!(!accepts(&adapter, "-10\n01"));
    assert!(!accepts(&adapter, "-10\u{0}01"));
    assert!(!accepts(&adapter, "-10\u{200b}01"));
}

#[cfg(feature = "matrix")]
#[test]
fn matrix_recipient_boundaries() {
    let adapter = matrix();

    assert!(!accepts(&adapter, ""));
    assert!(!accepts(&adapter, "   "));

    assert!(accepts(&adapter, "!r:s"));
    // Matrix has no documented room-id ceiling, but the crate-wide cap still
    // applies so an unbounded id cannot be embedded in a request body.
    let room_at_limit = format!("!{}:s", "a".repeat(MAX_RECIPIENT_BYTES - 3));
    assert_eq!(room_at_limit.len(), MAX_RECIPIENT_BYTES);
    assert!(accepts(&adapter, &room_at_limit));
    let room_over_limit = format!("!{}:s", "a".repeat(MAX_RECIPIENT_BYTES - 2));
    assert!(!accepts(&adapter, &room_over_limit));

    // Matrix room ids start with `!`.
    assert!(!accepts(&adapter, "r:example.org"));
    assert!(!accepts(&adapter, "!r:ex\nample.org"));
    assert!(!accepts(&adapter, "!r:ex\u{0}ample.org"));
    assert!(!accepts(&adapter, "!r:ex\u{200b}ample.org"));
}

/// A recipient that is only format characters is not "non-empty": it names
/// nothing, so every channel rejects it.
#[cfg(all(feature = "whatsapp", feature = "telegram", feature = "matrix"))]
#[test]
fn a_format_only_recipient_is_rejected_everywhere() {
    for adapter in [
        &whatsapp() as &dyn Adapter,
        &telegram() as &dyn Adapter,
        &matrix() as &dyn Adapter,
    ] {
        assert!(!accepts(adapter, "\u{200b}"));
        assert!(!accepts(adapter, "\u{feff}"));
    }
}

/// The channel in the error is the one the adapter speaks, so a caller can log
/// it without guessing which rule was violated.
#[cfg(feature = "whatsapp")]
#[test]
fn the_rejection_carries_the_channel() {
    let error = whatsapp()
        .encode(&OutboundMessage::text(Channel::WhatsApp, "", "hi"))
        .unwrap_err();
    match error {
        Error::InvalidMessage { channel, reason } => {
            assert_eq!(channel, Channel::WhatsApp);
            assert_ne!(reason, "");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}
