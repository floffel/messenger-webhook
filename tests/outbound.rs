//! Outbound encoding: provider limits and payload shapes.
//!
//! Every limit has a just-fits value and the value one past it, so the boundary
//! is proven rather than sampled.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code, unused_imports, unused_mut, unused_variables)]

use messenger_webhook::{Adapter, Channel, OutboundMessage};

fn text(channel: Channel, conversation: &str, body: &str) -> OutboundMessage {
    OutboundMessage::text(channel, conversation, body)
}

#[test]
fn outbound_text_has_no_provider_event_id_field() {
    let message = OutboundMessage::text(Channel::Telegram, "-1001", "Hallo");
    assert_eq!(message.conversation_id, "-1001");
    assert_eq!(message.text.as_deref(), Some("Hallo"));
    assert!(message.first_attachment().is_none());
}

#[cfg(feature = "whatsapp")]
mod whatsapp {
    use messenger_webhook::{
        whatsapp::{MAX_CAPTION_BYTES, MAX_TEXT_BYTES},
        Adapter, Attachment, AttachmentKind, Channel, Error, LimitUnit, OutboundMessage,
        RetryPolicy, WhatsAppAdapter,
    };

    fn adapter() -> WhatsAppAdapter {
        WhatsAppAdapter::new("secret-secret-secret").expect("adapter")
    }

    #[test]
    fn text_payload_has_the_cloud_api_shape() {
        let message = OutboundMessage::text(Channel::WhatsApp, "4915112345678", "Hallo");
        let payload = adapter().encode(&message).unwrap();
        assert_eq!(payload.method(), "POST");
        assert_eq!(payload.content_type(), "application/json");
        assert_eq!(payload.retry(), RetryPolicy::UnsafeUnlessKeyPresent);
        assert!(payload.idempotency_key().is_none());

        let body = payload.json().unwrap();
        assert_eq!(body["messaging_product"], "whatsapp");
        assert_eq!(body["recipient_type"], "individual");
        assert_eq!(body["to"], "4915112345678");
        assert_eq!(body["type"], "text");
        assert_eq!(body["text"]["body"], "Hallo");
    }

    #[test]
    fn text_limit_is_counted_in_bytes_not_characters() {
        // Exactly at the limit: 4096 ASCII bytes.
        let message = super::text(Channel::WhatsApp, "49", &"a".repeat(MAX_TEXT_BYTES));
        assert!(adapter().encode(&message).is_ok());

        // One past the limit.
        let message = super::text(Channel::WhatsApp, "49", &"a".repeat(MAX_TEXT_BYTES + 1));
        let error = adapter().encode(&message).unwrap_err();
        assert!(
            matches!(
                error,
                Error::MessageTooLong {
                    limit: MAX_TEXT_BYTES,
                    actual,
                    unit: LimitUnit::Bytes,
                    ..
                } if actual == MAX_TEXT_BYTES + 1
            ),
            "unexpected error: {error:?}"
        );

        // 2048 two-byte characters are exactly 4096 bytes …
        let message = super::text(Channel::WhatsApp, "49", &"ä".repeat(2048));
        assert!(adapter().encode(&message).is_ok());

        // … and 2049 of them are 4098 bytes, even though it is only 2049 chars.
        let message = super::text(Channel::WhatsApp, "49", &"ä".repeat(2049));
        let error = adapter().encode(&message).unwrap_err();
        assert!(matches!(
            error,
            Error::MessageTooLong {
                unit: LimitUnit::Bytes,
                ..
            }
        ));
    }

    #[test]
    fn caption_limit_matches_the_text_limit_rules() {
        let mut message = OutboundMessage::text(Channel::WhatsApp, "49", "");
        message.attachments.push(Attachment {
            kind: AttachmentKind::Image,
            url: Some("https://example.org/a.jpg".to_owned()),
            ..Attachment::default()
        });

        message.text = Some("c".repeat(MAX_CAPTION_BYTES));
        assert!(adapter().encode(&message).is_ok());

        message.text = Some("c".repeat(MAX_CAPTION_BYTES + 1));
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::MessageTooLong {
                limit: MAX_CAPTION_BYTES,
                ..
            }
        ));
    }

    #[test]
    fn media_uses_link_and_caption() {
        let mut message = OutboundMessage::text(Channel::WhatsApp, "49", "Look");
        message.attachments.push(Attachment {
            kind: AttachmentKind::Image,
            url: Some("https://example.org/cat.jpg".to_owned()),
            ..Attachment::default()
        });
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["type"], "image");
        assert_eq!(body["image"]["link"], "https://example.org/cat.jpg");
        assert_eq!(body["image"]["caption"], "Look");
    }

    #[test]
    fn document_carries_the_file_name() {
        let mut message = OutboundMessage::text(Channel::WhatsApp, "49", "");
        message.attachments.push(Attachment {
            kind: AttachmentKind::Document,
            id: Some("media-1".to_owned()),
            filename: Some("invoice.pdf".to_owned()),
            ..Attachment::default()
        });
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["type"], "document");
        assert_eq!(body["document"]["id"], "media-1");
        assert_eq!(body["document"]["filename"], "invoice.pdf");
    }

    #[test]
    fn interactive_reply_button_round_trips_the_option_id_and_title() {
        use messenger_webhook::InteractiveReply;

        // The whole path is in-process: `encode` builds the Cloud API body
        // without opening a socket, so a mock transport is just the payload.
        let reply = InteractiveReply::new("confirm-yes", "Yes, please").unwrap();
        let mut message = OutboundMessage::text(Channel::WhatsApp, "4915112345678", "Confirm?");
        message.interactive_reply = Some(reply);

        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["type"], "interactive");
        assert_eq!(body["interactive"]["type"], "button");
        assert_eq!(body["interactive"]["body"]["text"], "Confirm?");
        assert_eq!(body["interactive"]["action"]["buttons"][0]["type"], "reply");
        assert_eq!(
            body["interactive"]["action"]["buttons"][0]["reply"]["id"],
            "confirm-yes"
        );
        assert_eq!(
            body["interactive"]["action"]["buttons"][0]["reply"]["title"],
            "Yes, please"
        );

        // Feeding the encoded option id back through a provider-shaped inbound
        // envelope carries the same id, proving the round trip.
        let inbound = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"wamid.1","from":"4915112345678","type":"interactive",
             "interactive":{"type":"button_reply","button_reply":{"id":"confirm-yes","title":"Yes, please"}}}]}}]}]}"#;
        let events = adapter().adapt(inbound).unwrap();
        let carried = events[0].interactive_reply().unwrap();
        assert_eq!(carried.id(), "confirm-yes");
        assert_eq!(carried.title(), "Yes, please");
        assert_eq!(events[0].text(), Some("Yes, please"));
    }

    #[test]
    fn interactive_reply_needs_body_text_and_no_attachment() {
        use messenger_webhook::InteractiveReply;

        let reply = InteractiveReply::new("confirm", "Confirm").unwrap();

        // A reply button with no body text is refused before the wire.
        let mut empty = OutboundMessage::text(Channel::WhatsApp, "49", "");
        empty.interactive_reply = Some(reply.clone());
        assert!(
            matches!(
                adapter().encode(&empty).unwrap_err(),
                Error::InvalidMessage { .. }
            ),
            "an interactive reply without body text must be rejected"
        );

        // A reply button cannot share a message with media.
        let mut with_media = OutboundMessage::text(Channel::WhatsApp, "49", "Confirm?");
        with_media.interactive_reply = Some(reply);
        with_media.attachments.push(Attachment {
            kind: AttachmentKind::Image,
            url: Some("https://example.org/a.jpg".to_owned()),
            ..Attachment::default()
        });
        assert!(matches!(
            adapter().encode(&with_media).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }

    #[test]
    fn invalid_messages_are_rejected_not_sent() {
        // Empty recipient.
        assert!(matches!(
            adapter()
                .encode(&OutboundMessage::text(Channel::WhatsApp, "", "hi"))
                .unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Recipient is not E.164 digits.
        assert!(matches!(
            adapter()
                .encode(&OutboundMessage::text(Channel::WhatsApp, "+49 151", "hi"))
                .unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Neither text nor attachment.
        let mut empty = OutboundMessage::text(Channel::WhatsApp, "49", "");
        empty.text = None;
        assert!(matches!(
            adapter().encode(&empty).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Attachment without url or id.
        let mut orphan = OutboundMessage::text(Channel::WhatsApp, "49", "");
        orphan
            .attachments
            .push(Attachment::new(AttachmentKind::Image));
        assert!(matches!(
            adapter().encode(&orphan).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // A location cannot be sent through this endpoint.
        let mut location = OutboundMessage::text(Channel::WhatsApp, "49", "");
        location
            .attachments
            .push(Attachment::new(AttachmentKind::Location));
        assert!(matches!(
            adapter().encode(&location).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // The Cloud API documents no edit endpoint.
        let mut edit = OutboundMessage::text(Channel::WhatsApp, "49", "fixed");
        edit.edit_of = Some("wamid.1".to_owned());
        assert!(matches!(
            adapter().encode(&edit).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }

    #[test]
    fn reaction_payload_has_the_cloud_api_shape() {
        let mut message = OutboundMessage::text(Channel::WhatsApp, "4915112345678", "");
        message.text = None;
        message.reaction =
            Some(messenger_webhook::Reaction::add("wamid.target", "\u{1F44D}").expect("reaction"));

        let payload = adapter().encode(&message).unwrap();
        assert_eq!(
            payload.operation(),
            messenger_webhook::ProviderOperation::Reaction
        );
        assert_eq!(payload.method(), "POST");
        assert_eq!(payload.retry(), RetryPolicy::UnsafeUnlessKeyPresent);
        assert!(payload.idempotency_key().is_none());
        // The bot API keeps the event type inside the body, so there is none.
        assert_eq!(payload.event_type(), None);

        let body = payload.json().unwrap();
        assert_eq!(body["messaging_product"], "whatsapp");
        assert_eq!(body["recipient_type"], "individual");
        assert_eq!(body["to"], "4915112345678");
        assert_eq!(body["type"], "reaction");
        assert_eq!(body["reaction"]["message_id"], "wamid.target");
        assert_eq!(body["reaction"]["emoji"], "\u{1F44D}");
    }

    #[test]
    fn reaction_removal_uses_an_empty_emoji() {
        let mut message = OutboundMessage::text(Channel::WhatsApp, "49", "");
        message.text = None;
        message.reaction = Some(messenger_webhook::Reaction::remove("wamid.target"));

        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["type"], "reaction");
        assert_eq!(body["reaction"]["message_id"], "wamid.target");
        assert_eq!(body["reaction"]["emoji"], "");
    }

    #[test]
    fn reaction_key_visibility_boundary() {
        // The valid value: a single emoji.
        let mut message = OutboundMessage::text(Channel::WhatsApp, "49", "");
        message.text = None;
        message.reaction = Some(messenger_webhook::Reaction::add("wamid.1", "\u{1F44D}").unwrap());
        assert!(adapter().encode(&message).is_ok());

        // Just inside the boundary: an emoji plus a variation selector still
        // carries a visible character, so the key is accepted.
        let mut message = OutboundMessage::text(Channel::WhatsApp, "49", "");
        message.text = None;
        message.reaction =
            Some(messenger_webhook::Reaction::add("wamid.1", "\u{2764}\u{FE0F}").unwrap());
        assert!(adapter().encode(&message).is_ok());

        // Just outside it: a whitespace-only key has no visible character and
        // cannot identify a reaction, so it never becomes a payload.
        assert!(messenger_webhook::Reaction::add("wamid.1", "   ").is_err());
    }

    #[test]
    fn reaction_with_incompatible_fields_is_rejected() {
        // A reaction with text.
        let mut with_text = OutboundMessage::text(Channel::WhatsApp, "49", "hi");
        with_text.reaction =
            Some(messenger_webhook::Reaction::add("wamid.1", "\u{1F44D}").unwrap());
        assert!(matches!(
            adapter().encode(&with_text).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // A reaction targeting an empty message id.
        let mut empty_target = OutboundMessage::text(Channel::WhatsApp, "49", "");
        empty_target.text = None;
        empty_target.reaction = Some(messenger_webhook::Reaction::remove("   "));
        assert!(matches!(
            adapter().encode(&empty_target).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // A reaction with an attachment.
        let mut with_media = OutboundMessage::text(Channel::WhatsApp, "49", "");
        with_media.text = None;
        with_media.reaction =
            Some(messenger_webhook::Reaction::add("wamid.1", "\u{1F44D}").unwrap());
        with_media.attachments.push(Attachment {
            kind: AttachmentKind::Image,
            url: Some("https://example.org/a.jpg".to_owned()),
            ..Attachment::default()
        });
        assert!(matches!(
            adapter().encode(&with_media).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }
}

#[cfg(feature = "telegram")]
mod telegram {
    use messenger_webhook::{
        telegram::{MAX_CAPTION_UTF16, MAX_TEXT_UTF16},
        Adapter, Attachment, AttachmentKind, Channel, Error, LimitUnit, OutboundMessage,
        RetryPolicy, TelegramAdapter, TextFormat, TextSpan,
    };

    fn adapter() -> TelegramAdapter {
        TelegramAdapter::new("secret-secret-secret").expect("adapter")
    }

    #[test]
    fn text_payload_has_the_bot_api_shape() {
        let message = OutboundMessage::text(Channel::Telegram, "-1001", "Hallo");
        let payload = adapter().encode(&message).unwrap();
        assert_eq!(payload.retry(), RetryPolicy::UnsafeUnlessKeyPresent);
        assert!(payload.idempotency_key().is_none());
        let body = payload.json().unwrap();
        assert_eq!(body["chat_id"], "-1001");
        assert_eq!(body["text"], "Hallo");
        assert!(body.get("caption").is_none());
    }

    #[test]
    fn text_entities_are_encoded_at_the_boundary() {
        let text = "a😀bc";
        let just_inside = TextSpan::new(text, 1, 5, TextFormat::Bold).expect("inside");
        let at_limit = TextSpan::new(text, 0, text.len(), TextFormat::Italic).expect("at limit");
        let message = OutboundMessage::text(Channel::Telegram, "-1001", text)
            .with_spans([just_inside, at_limit]);

        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["entities"][0]["offset"], 1);
        assert_eq!(body["entities"][0]["length"], 2);
        assert_eq!(body["entities"][1]["offset"], 0);
        assert_eq!(body["entities"][1]["length"], 5);
        assert!(body.get("parse_mode").is_none());

        let past_end = TextSpan::new("a😀bc!", 0, 8, TextFormat::Code).expect("past end fixture");
        let error = adapter()
            .encode(&OutboundMessage::text(Channel::Telegram, "-1001", text).with_spans([past_end]))
            .unwrap_err();
        assert!(
            matches!(&error, Error::InvalidMessage { reason, .. } if reason.contains("span 0..8")),
            "unexpected error: {error:?}"
        );

        let mid_char = TextSpan::new("abc", 1, 3, TextFormat::Code).expect("mid-char fixture");
        let error = adapter()
            .encode(&OutboundMessage::text(Channel::Telegram, "-1001", text).with_spans([mid_char]))
            .unwrap_err();
        assert!(
            matches!(&error, Error::InvalidMessage { reason, .. } if reason.contains("span 1..3")),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn text_limit_is_counted_in_utf16_units() {
        let message = super::text(Channel::Telegram, "1", &"a".repeat(MAX_TEXT_UTF16));
        assert!(adapter().encode(&message).is_ok());

        let message = super::text(Channel::Telegram, "1", &"a".repeat(MAX_TEXT_UTF16 + 1));
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::MessageTooLong {
                limit: MAX_TEXT_UTF16,
                unit: LimitUnit::Utf16Units,
                ..
            }
        ));

        // An emoji outside the BMP is two UTF-16 units: 2048 of them are 4096
        // units and still fit, 2049 do not.
        let message = super::text(Channel::Telegram, "1", &"😀".repeat(2048));
        assert!(adapter().encode(&message).is_ok());

        let message = super::text(Channel::Telegram, "1", &"😀".repeat(2049));
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::MessageTooLong { .. }
        ));
    }

    #[test]
    fn caption_limit_is_1024_units() {
        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        message.attachments.push(Attachment {
            kind: AttachmentKind::Image,
            id: Some("AgAC-file".to_owned()),
            ..Attachment::default()
        });

        message.text = Some("c".repeat(MAX_CAPTION_UTF16));
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["caption"], "c".repeat(MAX_CAPTION_UTF16));

        message.text = Some("c".repeat(MAX_CAPTION_UTF16 + 1));
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::MessageTooLong {
                limit: MAX_CAPTION_UTF16,
                ..
            }
        ));
    }

    #[test]
    fn reply_and_thread_are_encoded_as_numbers() {
        let mut message = OutboundMessage::text(Channel::Telegram, "-1001", "hi");
        message.thread_id = Some("5".to_owned());
        message.reply_to = Some("7".to_owned());
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["message_thread_id"], 5);
        assert_eq!(body["reply_parameters"]["message_id"], 7);
    }

    #[test]
    fn non_numeric_ids_are_rejected() {
        let mut message = OutboundMessage::text(Channel::Telegram, "-1001", "hi");
        message.thread_id = Some("topic-5".to_owned());
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        let mut message = OutboundMessage::text(Channel::Telegram, "-1001", "hi");
        message.reply_to = Some("not-an-id".to_owned());
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }

    #[test]
    fn media_prefers_the_file_id_and_falls_back_to_a_url() {
        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        let mut photo = Attachment::new(AttachmentKind::Image);
        photo.id = Some("AgAC-file".to_owned());
        photo.url = Some("https://example.org/cat.jpg".to_owned());
        message.attachments.push(photo);
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["photo"], "AgAC-file");

        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        let mut voice = Attachment::new(AttachmentKind::Voice);
        voice.url = Some("https://example.org/a.ogg".to_owned());
        message.attachments.push(voice);
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["voice"], "https://example.org/a.ogg");
    }

    #[test]
    fn location_coordinates_are_encoded_at_the_boundaries() {
        let cases = [(0.0, 0.0), (-90.0, -180.0), (90.0, 180.0)];
        for (latitude, longitude) in cases {
            let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
            message.text = None;
            let mut location = Attachment::new(AttachmentKind::Location);
            location.latitude = serde_json::Number::from_f64(latitude);
            location.longitude = serde_json::Number::from_f64(longitude);
            message.attachments.push(location);

            let body = adapter().encode(&message).unwrap().json().unwrap();
            assert_eq!(body["latitude"], latitude);
            assert_eq!(body["longitude"], longitude);
        }

        for (latitude, longitude, expected) in [
            (-90.1, 0.0, "latitude"),
            (90.1, 0.0, "latitude"),
            (0.0, -180.1, "longitude"),
            (0.0, 180.1, "longitude"),
        ] {
            let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
            message.text = None;
            let mut location = Attachment::new(AttachmentKind::Location);
            location.latitude = serde_json::Number::from_f64(latitude);
            location.longitude = serde_json::Number::from_f64(longitude);
            message.attachments.push(location);

            let error = adapter().encode(&message).unwrap_err();
            assert!(
                matches!(
                    &error,
                    Error::InvalidMessage { reason, .. } if reason.contains(expected)
                ),
                "unexpected error: {error:?}"
            );
        }

        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        let mut location = Attachment::new(AttachmentKind::Location);
        location.longitude = serde_json::Number::from_f64(11.4);
        message.attachments.push(location);
        let error = adapter().encode(&message).unwrap_err();
        assert!(
            matches!(
                &error,
                Error::InvalidMessage { reason, .. } if reason.contains("latitude")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn contact_fields_are_encoded_and_required_fields_are_checked() {
        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        let mut contact = Attachment::new(AttachmentKind::Contact);
        contact.phone_number = Some("+4915112345678".to_owned());
        contact.first_name = Some("Florian".to_owned());
        contact.last_name = Some("Loeffel".to_owned());
        message.attachments.push(contact);

        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["phone_number"], "+4915112345678");
        assert_eq!(body["first_name"], "Florian");
        assert_eq!(body["last_name"], "Loeffel");

        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        let mut contact = Attachment::new(AttachmentKind::Contact);
        contact.first_name = Some("Florian".to_owned());
        message.attachments.push(contact);
        let error = adapter().encode(&message).unwrap_err();
        assert!(
            matches!(
                &error,
                Error::InvalidMessage { reason, .. } if reason.contains("phone number")
            ),
            "unexpected error: {error:?}"
        );

        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        let mut contact = Attachment::new(AttachmentKind::Contact);
        contact.phone_number = Some("+4915112345678".to_owned());
        message.attachments.push(contact);
        let error = adapter().encode(&message).unwrap_err();
        assert!(
            matches!(
                &error,
                Error::InvalidMessage { reason, .. } if reason.contains("first name")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn unsupported_messages_are_rejected() {
        let mut location = OutboundMessage::text(Channel::Telegram, "1", "");
        location
            .attachments
            .push(Attachment::new(AttachmentKind::Location));
        assert!(matches!(
            adapter().encode(&location).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        assert!(matches!(
            adapter()
                .encode(&OutboundMessage::text(Channel::Telegram, "", "hi"))
                .unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }

    #[test]
    fn edit_with_attachment_is_rejected_before_building_a_send() {
        let mut edit = OutboundMessage::text(Channel::Telegram, "1", "fixed");
        edit.edit_of = Some("7".to_owned());
        let mut photo = Attachment::new(AttachmentKind::Image);
        photo.id = Some("AgAC-file".to_owned());
        edit.attachments.push(photo);

        let error = adapter().encode(&edit).unwrap_err();
        assert!(
            matches!(
                &error,
                Error::InvalidMessage { reason, .. }
                    if reason.contains("editMessageMedia")
                        && reason.contains("editMessageCaption")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn edit_payload_uses_edit_message_text() {
        let mut edit = OutboundMessage::text(Channel::Telegram, "1", "fixed");
        edit.edit_of = Some("7".to_owned());

        let payload = adapter().encode(&edit).unwrap();
        assert_eq!(
            payload.operation(),
            messenger_webhook::ProviderOperation::Edit
        );
        assert_eq!(payload.method(), "POST");
        assert_eq!(payload.event_type(), None);

        let body = payload.json().unwrap();
        assert_eq!(body["chat_id"], "1");
        assert_eq!(body["message_id"], 7);
        assert_eq!(body["text"], "fixed");
    }

    #[test]
    fn edit_text_limit_is_counted_in_utf16_units() {
        // The valid value: a short replacement.
        let mut edit = OutboundMessage::text(Channel::Telegram, "1", "fixed");
        edit.edit_of = Some("7".to_owned());
        assert!(adapter().encode(&edit).is_ok());

        // Just inside the limit: exactly 4096 UTF-16 units.
        let mut at_limit =
            OutboundMessage::text(Channel::Telegram, "1", "a".repeat(MAX_TEXT_UTF16));
        at_limit.edit_of = Some("7".to_owned());
        assert!(adapter().encode(&at_limit).is_ok());

        // Just outside it.
        let mut over =
            OutboundMessage::text(Channel::Telegram, "1", "a".repeat(MAX_TEXT_UTF16 + 1));
        over.edit_of = Some("7".to_owned());
        assert!(matches!(
            adapter().encode(&over).unwrap_err(),
            Error::MessageTooLong {
                limit: MAX_TEXT_UTF16,
                unit: LimitUnit::Utf16Units,
                ..
            }
        ));
    }

    #[test]
    fn edit_rejects_a_non_integer_message_id() {
        let mut edit = OutboundMessage::text(Channel::Telegram, "1", "fixed");
        edit.edit_of = Some("not-an-id".to_owned());
        assert!(matches!(
            adapter().encode(&edit).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }

    #[test]
    fn reaction_payload_uses_set_message_reaction() {
        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        message.reaction =
            Some(messenger_webhook::Reaction::add("7", "\u{1F44D}").expect("reaction"));

        let payload = adapter().encode(&message).unwrap();
        assert_eq!(
            payload.operation(),
            messenger_webhook::ProviderOperation::Reaction
        );
        assert_eq!(payload.method(), "POST");

        let body = payload.json().unwrap();
        assert_eq!(body["chat_id"], "1");
        assert_eq!(body["message_id"], 7);
        assert_eq!(body["reaction"][0]["type"], "emoji");
        assert_eq!(body["reaction"][0]["emoji"], "\u{1F44D}");
    }

    #[test]
    fn reaction_removal_uses_an_empty_reaction_list() {
        let mut message = OutboundMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        message.reaction = Some(messenger_webhook::Reaction::remove("7"));

        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["message_id"], 7);
        assert_eq!(body["reaction"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn reaction_with_incompatible_fields_is_rejected() {
        // A reaction with text.
        let mut with_text = OutboundMessage::text(Channel::Telegram, "1", "hi");
        with_text.reaction = Some(messenger_webhook::Reaction::add("7", "\u{1F44D}").unwrap());
        assert!(matches!(
            adapter().encode(&with_text).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // A reaction with an empty target id.
        let mut empty_target = OutboundMessage::text(Channel::Telegram, "1", "");
        empty_target.text = None;
        empty_target.reaction = Some(messenger_webhook::Reaction::remove("   "));
        assert!(matches!(
            adapter().encode(&empty_target).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // A non-integer target id is rejected before the request is built.
        let mut bad_id = OutboundMessage::text(Channel::Telegram, "1", "");
        bad_id.text = None;
        bad_id.reaction = Some(messenger_webhook::Reaction::add("nope", "\u{1F44D}").unwrap());
        assert!(matches!(
            adapter().encode(&bad_id).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }
}

#[cfg(feature = "matrix")]
mod matrix {
    use messenger_webhook::{
        Adapter, Attachment, AttachmentKind, Channel, Error, MatrixAdapter, OutboundMessage,
        RetryPolicy,
    };

    fn adapter() -> MatrixAdapter {
        MatrixAdapter::new("hs-token-hs-token").expect("adapter")
    }

    #[test]
    fn text_payload_is_room_content() {
        let message = OutboundMessage::text(Channel::Matrix, "!r:example.org", "Hallo");
        let payload = adapter().encode(&message).unwrap();
        assert_eq!(payload.method(), "PUT");
        assert_eq!(payload.retry(), RetryPolicy::Safe);
        assert!(payload.idempotency_key().is_some());

        let content = payload.json().unwrap();
        assert_eq!(content["msgtype"], "m.text");
        assert_eq!(content["body"], "Hallo");
        assert!(content.get("url").is_none());
    }

    #[test]
    fn matrix_payloads_carry_distinct_transaction_keys() {
        let message = OutboundMessage::text(Channel::Matrix, "!r:example.org", "Hallo");
        let first = adapter().encode(&message).unwrap();
        let second = adapter().encode(&message).unwrap();

        assert_eq!(first.retry(), RetryPolicy::Safe);
        assert_eq!(second.retry(), RetryPolicy::Safe);
        assert!(first.idempotency_key().is_some());
        assert!(second.idempotency_key().is_some());
        assert_ne!(first.idempotency_key(), second.idempotency_key());
    }

    #[test]
    fn there_is_no_documented_body_limit() {
        // Matrix has no message length limit; 10 000 characters are fine.
        let message = super::text(Channel::Matrix, "!r:s", &"x".repeat(10_000));
        assert!(adapter().encode(&message).is_ok());
    }

    #[test]
    fn media_content_carries_url_and_info() {
        let mut message = OutboundMessage::text(Channel::Matrix, "!r:s", "");
        let mut image = Attachment::new(AttachmentKind::Image);
        image.url = Some("mxc://example.org/cat".to_owned());
        image.mime_type = Some("image/jpeg".to_owned());
        image.size_bytes = Some(1234);
        image.width = Some(10);
        image.height = Some(20);
        image.filename = Some("cat.jpg".to_owned());
        message.attachments.push(image);

        let content = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(content["msgtype"], "m.image");
        assert_eq!(content["body"], "cat.jpg");
        assert_eq!(content["url"], "mxc://example.org/cat");
        assert_eq!(content["info"]["mimetype"], "image/jpeg");
        assert_eq!(content["info"]["size"], 1234);
        assert_eq!(content["info"]["w"], 10);
        assert_eq!(content["info"]["h"], 20);
    }

    #[test]
    fn reply_thread_and_edit_relations() {
        let mut reply = OutboundMessage::text(Channel::Matrix, "!r:s", "hi");
        reply.reply_to = Some("$1".to_owned());
        let content = adapter().encode(&reply).unwrap().json().unwrap();
        assert_eq!(content["m.relates_to"]["m.in_reply_to"]["event_id"], "$1");

        let mut threaded = OutboundMessage::text(Channel::Matrix, "!r:s", "hi");
        threaded.thread_id = Some("$root".to_owned());
        threaded.reply_to = Some("$1".to_owned());
        let content = adapter().encode(&threaded).unwrap().json().unwrap();
        assert_eq!(content["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(content["m.relates_to"]["event_id"], "$root");
        assert_eq!(content["m.relates_to"]["m.in_reply_to"]["event_id"], "$1");

        let mut edit = OutboundMessage::text(Channel::Matrix, "!r:s", "fixed");
        edit.edit_of = Some("$1".to_owned());
        let content = adapter().encode(&edit).unwrap().json().unwrap();
        assert_eq!(content["m.relates_to"]["rel_type"], "m.replace");
        assert_eq!(content["m.relates_to"]["event_id"], "$1");
        assert_eq!(content["body"], "* fixed");
        assert_eq!(content["m.new_content"]["body"], "fixed");
        assert_eq!(content["m.new_content"]["msgtype"], "m.text");
    }

    #[test]
    fn edit_replacement_keeps_full_media_content() {
        let mut edit = OutboundMessage::text(Channel::Matrix, "!r:s", "");
        edit.edit_of = Some("$1".to_owned());
        let mut image = Attachment::new(AttachmentKind::Image);
        image.url = Some("mxc://example.org/cat".to_owned());
        image.mime_type = Some("image/jpeg".to_owned());
        image.size_bytes = Some(1234);
        image.width = Some(10);
        image.height = Some(20);
        image.filename = Some("cat.jpg".to_owned());
        edit.attachments.push(image);

        let content = adapter().encode(&edit).unwrap().json().unwrap();
        assert_eq!(content["body"], "* cat.jpg");
        assert_eq!(content["m.relates_to"]["rel_type"], "m.replace");
        assert_eq!(content["m.relates_to"]["event_id"], "$1");
        assert_eq!(content["m.new_content"]["msgtype"], "m.image");
        assert_eq!(content["m.new_content"]["body"], "cat.jpg");
        assert_eq!(content["m.new_content"]["url"], "mxc://example.org/cat");
        assert_eq!(content["m.new_content"]["info"]["mimetype"], "image/jpeg");
        assert_eq!(content["m.new_content"]["info"]["size"], 1234);
        assert_eq!(content["m.new_content"]["info"]["w"], 10);
        assert_eq!(content["m.new_content"]["info"]["h"], 20);
        assert!(content["m.new_content"].get("m.relates_to").is_none());
        assert!(content["m.new_content"].get("m.new_content").is_none());
    }

    #[test]
    fn reaction_payload_is_an_annotation_event() {
        let mut message = OutboundMessage::text(Channel::Matrix, "!r:s", "");
        message.text = None;
        message.reaction =
            Some(messenger_webhook::Reaction::add("$target", "\u{1F44D}").expect("reaction"));

        let payload = adapter().encode(&message).unwrap();
        assert_eq!(
            payload.operation(),
            messenger_webhook::ProviderOperation::Reaction
        );
        assert_eq!(payload.method(), "PUT");
        assert_eq!(payload.event_type(), Some("m.reaction"));
        assert_eq!(payload.retry(), RetryPolicy::Safe);
        assert!(payload.idempotency_key().is_some());

        let content = payload.json().unwrap();
        assert_eq!(content["m.relates_to"]["rel_type"], "m.annotation");
        assert_eq!(content["m.relates_to"]["event_id"], "$target");
        assert_eq!(content["m.relates_to"]["key"], "\u{1F44D}");
    }

    #[test]
    fn send_and_edit_report_their_event_type_and_operation() {
        let send = adapter()
            .encode(&OutboundMessage::text(Channel::Matrix, "!r:s", "hi"))
            .unwrap();
        assert_eq!(
            send.operation(),
            messenger_webhook::ProviderOperation::Message
        );
        assert_eq!(send.event_type(), Some("m.room.message"));

        let mut edit = OutboundMessage::text(Channel::Matrix, "!r:s", "fixed");
        edit.edit_of = Some("$1".to_owned());
        let edit = adapter().encode(&edit).unwrap();
        assert_eq!(edit.operation(), messenger_webhook::ProviderOperation::Edit);
        assert_eq!(edit.event_type(), Some("m.room.message"));
    }

    #[test]
    fn reaction_removal_is_rejected_as_a_redaction() {
        let mut message = OutboundMessage::text(Channel::Matrix, "!r:s", "");
        message.text = None;
        message.reaction = Some(messenger_webhook::Reaction::remove("$target"));
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }

    #[test]
    fn reaction_with_incompatible_fields_is_rejected() {
        // A reaction with text.
        let mut with_text = OutboundMessage::text(Channel::Matrix, "!r:s", "hi");
        with_text.reaction = Some(messenger_webhook::Reaction::add("$1", "\u{1F44D}").unwrap());
        assert!(matches!(
            adapter().encode(&with_text).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // A reaction with an empty target id (an add, so the removal check does
        // not shadow the empty-target check).
        let mut empty_target = OutboundMessage::text(Channel::Matrix, "!r:s", "");
        empty_target.text = None;
        empty_target.reaction = Some(messenger_webhook::Reaction::add("", "\u{1F44D}").unwrap());
        assert!(matches!(
            adapter().encode(&empty_target).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }

    #[test]
    fn invalid_messages_are_rejected() {
        // Not a room id.
        assert!(matches!(
            adapter()
                .encode(&OutboundMessage::text(Channel::Matrix, "@user:s", "hi"))
                .unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Media url is not a complete mxc://server/media URI.
        for url in ["https://example.org/cat.jpg", "mxc://", "mxc:///cat"] {
            let mut message = OutboundMessage::text(Channel::Matrix, "!r:s", "");
            let mut image = Attachment::new(AttachmentKind::Image);
            image.url = Some(url.to_owned());
            message.attachments.push(image);
            assert!(
                matches!(
                    adapter().encode(&message).unwrap_err(),
                    Error::InvalidMessage { .. }
                ),
                "{url}"
            );
        }

        // Location cannot be an m.room.message.
        let mut location = OutboundMessage::text(Channel::Matrix, "!r:s", "");
        location
            .attachments
            .push(Attachment::new(AttachmentKind::Location));
        assert!(matches!(
            adapter().encode(&location).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Empty message.
        let mut empty = OutboundMessage::text(Channel::Matrix, "!r:s", "");
        empty.text = None;
        assert!(matches!(
            adapter().encode(&empty).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }
}

#[test]
fn payload_type_is_channel_typed() {
    // The payload carries the channel it was built for, so a caller can assert
    // it is routing to the right endpoint.
    #[cfg(feature = "telegram")]
    {
        use messenger_webhook::TelegramAdapter;
        let payload = TelegramAdapter::new("secret-secret-secret")
            .expect("adapter")
            .encode(&OutboundMessage::text(Channel::Telegram, "1", "hi"))
            .unwrap();
        assert_eq!(payload.channel(), &Channel::Telegram);
    }

    #[cfg(feature = "whatsapp")]
    {
        use messenger_webhook::WhatsAppAdapter;
        let payload = WhatsAppAdapter::new("secret-secret-secret")
            .expect("adapter")
            .encode(&OutboundMessage::text(Channel::WhatsApp, "49", "hi"))
            .unwrap();
        assert_eq!(payload.channel(), &Channel::WhatsApp);
    }

    #[cfg(feature = "matrix")]
    {
        use messenger_webhook::MatrixAdapter;
        let payload = MatrixAdapter::new("secret-secret-secret")
            .expect("adapter")
            .encode(&OutboundMessage::text(Channel::Matrix, "!r:s", "hi"))
            .unwrap();
        assert_eq!(payload.channel(), &Channel::Matrix);
    }

    let _: fn(messenger_webhook::Channel, &str, &str) -> OutboundMessage = text;
}
