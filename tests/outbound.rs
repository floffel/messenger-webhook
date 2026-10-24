//! Outbound encoding: provider limits and payload shapes.
//!
//! Every limit has a just-fits value and the value one past it, so the boundary
//! is proven rather than sampled.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use messenger_webhook::{Adapter, Channel, NormalizedMessage};

fn text(channel: Channel, conversation: &str, body: &str) -> NormalizedMessage {
    NormalizedMessage::text(channel, conversation, body)
}

#[cfg(feature = "whatsapp")]
mod whatsapp {
    use messenger_webhook::{
        whatsapp::{MAX_CAPTION_BYTES, MAX_TEXT_BYTES},
        Adapter, Attachment, AttachmentKind, Channel, Error, NormalizedMessage, WhatsAppAdapter,
    };

    fn adapter() -> WhatsAppAdapter {
        WhatsAppAdapter::new("secret")
    }

    #[test]
    fn text_payload_has_the_cloud_api_shape() {
        let message = NormalizedMessage::text(Channel::WhatsApp, "4915112345678", "Hallo");
        let payload = adapter().encode(&message).unwrap();
        assert_eq!(payload.method(), "POST");
        assert_eq!(payload.content_type(), "application/json");

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
                    unit: "bytes",
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
        assert!(matches!(error, Error::MessageTooLong { unit: "bytes", .. }));
    }

    #[test]
    fn caption_limit_matches_the_text_limit_rules() {
        let mut message = NormalizedMessage::text(Channel::WhatsApp, "49", "");
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
        let mut message = NormalizedMessage::text(Channel::WhatsApp, "49", "Look");
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
        let mut message = NormalizedMessage::text(Channel::WhatsApp, "49", "");
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
    fn invalid_messages_are_rejected_not_sent() {
        // Empty recipient.
        assert!(matches!(
            adapter()
                .encode(&NormalizedMessage::text(Channel::WhatsApp, "", "hi"))
                .unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Recipient is not E.164 digits.
        assert!(matches!(
            adapter()
                .encode(&NormalizedMessage::text(Channel::WhatsApp, "+49 151", "hi"))
                .unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Neither text nor attachment.
        let mut empty = NormalizedMessage::text(Channel::WhatsApp, "49", "");
        empty.text = None;
        assert!(matches!(
            adapter().encode(&empty).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Attachment without url or id.
        let mut orphan = NormalizedMessage::text(Channel::WhatsApp, "49", "");
        orphan
            .attachments
            .push(Attachment::new(AttachmentKind::Image));
        assert!(matches!(
            adapter().encode(&orphan).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // A location cannot be sent through this endpoint.
        let mut location = NormalizedMessage::text(Channel::WhatsApp, "49", "");
        location
            .attachments
            .push(Attachment::new(AttachmentKind::Location));
        assert!(matches!(
            adapter().encode(&location).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Edits need the original message id, not a send.
        let mut edit = NormalizedMessage::text(Channel::WhatsApp, "49", "fixed");
        edit.edit_of = Some("wamid.1".to_owned());
        assert!(matches!(
            adapter().encode(&edit).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }
}

#[cfg(feature = "telegram")]
mod telegram {
    use messenger_webhook::{
        telegram::{MAX_CAPTION_UTF16, MAX_TEXT_UTF16},
        Adapter, Attachment, AttachmentKind, Channel, Error, NormalizedMessage, TelegramAdapter,
    };

    fn adapter() -> TelegramAdapter {
        TelegramAdapter::new("secret")
    }

    #[test]
    fn text_payload_has_the_bot_api_shape() {
        let message = NormalizedMessage::text(Channel::Telegram, "-1001", "Hallo");
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["chat_id"], "-1001");
        assert_eq!(body["text"], "Hallo");
        assert!(body.get("caption").is_none());
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
                unit: "UTF-16 units",
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
        let mut message = NormalizedMessage::text(Channel::Telegram, "1", "");
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
        let mut message = NormalizedMessage::text(Channel::Telegram, "-1001", "hi");
        message.thread_id = Some("5".to_owned());
        message.reply_to = Some("7".to_owned());
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["message_thread_id"], 5);
        assert_eq!(body["reply_parameters"]["message_id"], 7);
    }

    #[test]
    fn non_numeric_ids_are_rejected() {
        let mut message = NormalizedMessage::text(Channel::Telegram, "-1001", "hi");
        message.thread_id = Some("topic-5".to_owned());
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        let mut message = NormalizedMessage::text(Channel::Telegram, "-1001", "hi");
        message.reply_to = Some("not-an-id".to_owned());
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }

    #[test]
    fn media_prefers_the_file_id_and_falls_back_to_a_url() {
        let mut message = NormalizedMessage::text(Channel::Telegram, "1", "");
        let mut photo = Attachment::new(AttachmentKind::Image);
        photo.id = Some("AgAC-file".to_owned());
        photo.url = Some("https://example.org/cat.jpg".to_owned());
        message.attachments.push(photo);
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["photo"], "AgAC-file");

        let mut message = NormalizedMessage::text(Channel::Telegram, "1", "");
        let mut voice = Attachment::new(AttachmentKind::Voice);
        voice.url = Some("https://example.org/a.ogg".to_owned());
        message.attachments.push(voice);
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["voice"], "https://example.org/a.ogg");
    }

    #[test]
    fn unsupported_messages_are_rejected() {
        let mut edit = NormalizedMessage::text(Channel::Telegram, "1", "fixed");
        edit.edit_of = Some("7".to_owned());
        assert!(matches!(
            adapter().encode(&edit).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        let mut location = NormalizedMessage::text(Channel::Telegram, "1", "");
        location
            .attachments
            .push(Attachment::new(AttachmentKind::Location));
        assert!(matches!(
            adapter().encode(&location).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        assert!(matches!(
            adapter()
                .encode(&NormalizedMessage::text(Channel::Telegram, "", "hi"))
                .unwrap_err(),
            Error::InvalidMessage { .. }
        ));
    }
}

#[cfg(feature = "matrix")]
mod matrix {
    use messenger_webhook::{
        Adapter, Attachment, AttachmentKind, Channel, Error, MatrixAdapter, NormalizedMessage,
    };

    fn adapter() -> MatrixAdapter {
        MatrixAdapter::new("hs-token")
    }

    #[test]
    fn text_payload_is_room_content() {
        let message = NormalizedMessage::text(Channel::Matrix, "!r:example.org", "Hallo");
        let payload = adapter().encode(&message).unwrap();
        assert_eq!(payload.method(), "PUT");

        let content = payload.json().unwrap();
        assert_eq!(content["msgtype"], "m.text");
        assert_eq!(content["body"], "Hallo");
        assert!(content.get("url").is_none());
    }

    #[test]
    fn there_is_no_documented_body_limit() {
        // Matrix has no message length limit; 10 000 characters are fine.
        let message = super::text(Channel::Matrix, "!r:s", &"x".repeat(10_000));
        assert!(adapter().encode(&message).is_ok());
    }

    #[test]
    fn media_content_carries_url_and_info() {
        let mut message = NormalizedMessage::text(Channel::Matrix, "!r:s", "");
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
        let mut reply = NormalizedMessage::text(Channel::Matrix, "!r:s", "hi");
        reply.reply_to = Some("$1".to_owned());
        let content = adapter().encode(&reply).unwrap().json().unwrap();
        assert_eq!(content["m.relates_to"]["m.in_reply_to"]["event_id"], "$1");

        let mut threaded = NormalizedMessage::text(Channel::Matrix, "!r:s", "hi");
        threaded.thread_id = Some("$root".to_owned());
        threaded.reply_to = Some("$1".to_owned());
        let content = adapter().encode(&threaded).unwrap().json().unwrap();
        assert_eq!(content["m.relates_to"]["rel_type"], "m.thread");
        assert_eq!(content["m.relates_to"]["event_id"], "$root");
        assert_eq!(content["m.relates_to"]["m.in_reply_to"]["event_id"], "$1");

        let mut edit = NormalizedMessage::text(Channel::Matrix, "!r:s", "fixed");
        edit.edit_of = Some("$1".to_owned());
        let content = adapter().encode(&edit).unwrap().json().unwrap();
        assert_eq!(content["m.relates_to"]["rel_type"], "m.replace");
        assert_eq!(content["m.relates_to"]["event_id"], "$1");
        assert_eq!(content["m.new_content"]["body"], "fixed");
        assert_eq!(content["m.new_content"]["msgtype"], "m.text");
    }

    #[test]
    fn invalid_messages_are_rejected() {
        // Not a room id.
        assert!(matches!(
            adapter()
                .encode(&NormalizedMessage::text(Channel::Matrix, "@user:s", "hi"))
                .unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Media url is not an mxc:// uri.
        let mut message = NormalizedMessage::text(Channel::Matrix, "!r:s", "");
        let mut image = Attachment::new(AttachmentKind::Image);
        image.url = Some("https://example.org/cat.jpg".to_owned());
        message.attachments.push(image);
        assert!(matches!(
            adapter().encode(&message).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Location cannot be an m.room.message.
        let mut location = NormalizedMessage::text(Channel::Matrix, "!r:s", "");
        location
            .attachments
            .push(Attachment::new(AttachmentKind::Location));
        assert!(matches!(
            adapter().encode(&location).unwrap_err(),
            Error::InvalidMessage { .. }
        ));

        // Empty message.
        let mut empty = NormalizedMessage::text(Channel::Matrix, "!r:s", "");
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
        let payload = TelegramAdapter::new("s")
            .encode(&NormalizedMessage::text(Channel::Telegram, "1", "hi"))
            .unwrap();
        assert_eq!(payload.channel(), &Channel::Telegram);
    }

    #[cfg(feature = "whatsapp")]
    {
        use messenger_webhook::WhatsAppAdapter;
        let payload = WhatsAppAdapter::new("s")
            .encode(&NormalizedMessage::text(Channel::WhatsApp, "49", "hi"))
            .unwrap();
        assert_eq!(payload.channel(), &Channel::WhatsApp);
    }

    #[cfg(feature = "matrix")]
    {
        use messenger_webhook::MatrixAdapter;
        let payload = MatrixAdapter::new("s")
            .encode(&NormalizedMessage::text(Channel::Matrix, "!r:s", "hi"))
            .unwrap();
        assert_eq!(payload.channel(), &Channel::Matrix);
    }

    let _: fn(messenger_webhook::Channel, &str, &str) -> NormalizedMessage = text;
}
