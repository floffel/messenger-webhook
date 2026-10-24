//! Media, reactions, receipts and status branches on both directions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(feature = "whatsapp")]
mod whatsapp {
    use messenger_webhook::{
        Adapter, Attachment, AttachmentKind, Channel, NormalizedMessage, WhatsAppAdapter,
    };

    fn adapter() -> WhatsAppAdapter {
        WhatsAppAdapter::new("secret")
    }

    fn inbound(body: &str) -> Vec<NormalizedMessage> {
        adapter()
            .adapt(format!(r#"{{"entry":[{{"changes":[{{"value":{body}}}]}}]}}"#).as_bytes())
            .unwrap()
    }

    #[test]
    fn every_status_maps_onto_the_neutral_enum() {
        let cases = [
            ("sent", "sent"),
            ("delivered", "delivered"),
            ("read", "read"),
            ("failed", "failed"),
            ("deleted", "deleted"),
            ("queued", "queued"),
        ];
        for (raw, expected) in cases {
            let events = inbound(&format!(
                r#"{{"statuses":[{{"id":"wamid.1","status":"{raw}","recipient_id":"49"}}]}}"#
            ));
            assert_eq!(
                events[0]
                    .status
                    .as_ref()
                    .map(messenger_webhook::DeliveryStatus::as_str),
                Some(expected),
                "status {raw}"
            );
        }
    }

    #[test]
    fn media_kinds_map_onto_the_neutral_enum() {
        let cases = [
            (
                r#""type":"image","image":{"id":"m1","mime_type":"image/png"}"#,
                "image",
            ),
            (r#""type":"video","video":{"id":"m2"}"#, "video"),
            (r#""type":"audio","audio":{"id":"m3"}"#, "audio"),
            (
                r#""type":"audio","audio":{"id":"m4","voice":true}"#,
                "voice",
            ),
            (
                r#""type":"document","document":{"id":"m5","filename":"a.pdf"}"#,
                "document",
            ),
            (
                r#""type":"sticker","sticker":{"id":"m6","animated":true}"#,
                "sticker",
            ),
        ];
        for (fields, expected) in cases {
            let events = inbound(&format!(
                r#"{{"messages":[{{"id":"wamid.1","from":"49",{fields}}}]}}"#
            ));
            let attachment = events[0].first_attachment().expect("attachment");
            assert_eq!(attachment.kind.as_str(), expected);
        }

        // A document keeps its file name; a voice note stays an audio kind.
        let events = inbound(
            r#"{"messages":[{"id":"wamid.1","from":"49","type":"document","document":{"id":"m5","filename":"a.pdf"}}]}"#,
        );
        assert_eq!(
            events[0].first_attachment().unwrap().filename.as_deref(),
            Some("a.pdf")
        );
    }

    #[test]
    fn location_and_contact_cards_keep_their_label() {
        let events = inbound(
            r#"{"messages":[{"id":"w1","from":"49","type":"location","location":{"name":"Ingolstadt"}}]}"#,
        );
        assert_eq!(
            events[0].first_attachment().unwrap().kind.as_str(),
            "location"
        );
        assert_eq!(
            events[0].first_attachment().unwrap().caption.as_deref(),
            Some("Ingolstadt")
        );

        // Address is the fallback when the place has no name.
        let events = inbound(
            r#"{"messages":[{"id":"w2","from":"49","type":"location","location":{"address":"Hauptstr. 1"}}]}"#,
        );
        assert_eq!(
            events[0].first_attachment().unwrap().caption.as_deref(),
            Some("Hauptstr. 1")
        );

        let events = inbound(
            r#"{"messages":[{"id":"w3","from":"49","type":"contacts","contacts":[{"name":{"formatted_name":"Florian"}}]}]}"#,
        );
        assert_eq!(
            events[0].first_attachment().unwrap().kind.as_str(),
            "contact"
        );
        assert_eq!(
            events[0].first_attachment().unwrap().caption.as_deref(),
            Some("Florian")
        );
    }

    #[test]
    fn reactions_need_a_target_but_never_panic() {
        let events = inbound(
            r#"{"messages":[{"id":"w1","from":"49","type":"reaction","reaction":{"message_id":"wamid.1","emoji":"👍"}}]}"#,
        );
        let reaction = events[0].reaction.as_ref().unwrap();
        assert_eq!(reaction.message_id, "wamid.1");
        assert_eq!(reaction.emoji, "👍");
        assert!(!reaction.removed);

        // Removing a reaction: empty emoji.
        let events = inbound(
            r#"{"messages":[{"id":"w2","from":"49","type":"reaction","reaction":{"message_id":"wamid.1","emoji":""}}]}"#,
        );
        assert!(events[0].reaction.as_ref().unwrap().removed);

        // No target: the event is still carried, just without a reaction.
        let events = inbound(
            r#"{"messages":[{"id":"w3","from":"49","type":"reaction","reaction":{"emoji":"👍"}}]}"#,
        );
        assert!(events[0].reaction.is_none());
        assert_eq!(events[0].provider_type.as_deref(), Some("reaction"));
    }

    #[test]
    fn typed_messages_without_their_object_are_carried_as_metadata() {
        // No `location`, `contacts` or `reaction` object: the event still
        // exists, it just has no attachment or reaction.
        let events = inbound(
            r#"{"messages":[
                {"id":"w1","from":"49","type":"location"},
                {"id":"w2","from":"49","type":"contacts"},
                {"id":"w3","from":"49","type":"reaction"}]}"#,
        );
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].attachments.len(), 0);
        assert_eq!(events[1].attachments.len(), 0);
        assert!(events[2].reaction.is_none());
    }

    #[test]
    fn changes_without_a_value_are_skipped() {
        let events = adapter()
            .adapt(br#"{"entry":[{"changes":[{"field":"messages"}]}]}"#)
            .unwrap();
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn every_media_kind_can_be_encoded() {
        let cases = [
            (AttachmentKind::Image, "image"),
            (AttachmentKind::Video, "video"),
            (AttachmentKind::Audio, "audio"),
            (AttachmentKind::Voice, "audio"),
            (AttachmentKind::Document, "document"),
            (AttachmentKind::Sticker, "sticker"),
        ];
        for (kind, field) in cases {
            let mut message = NormalizedMessage::text(Channel::WhatsApp, "49", "");
            message.text = None;
            let mut attachment = Attachment::new(kind.clone());
            attachment.url = Some(format!("https://example.org/{field}"));
            message.attachments.push(attachment);

            let body = adapter().encode(&message).unwrap().json().unwrap();
            assert_eq!(body["type"], field, "{kind:?}");
            assert_eq!(body[field]["link"], format!("https://example.org/{field}"));
        }
    }

    #[test]
    fn media_without_a_reference_is_rejected() {
        let mut message = NormalizedMessage::text(Channel::WhatsApp, "49", "");
        message.text = None;
        message
            .attachments
            .push(Attachment::new(AttachmentKind::Image));
        assert!(adapter().encode(&message).is_err());
    }
}

#[cfg(feature = "telegram")]
mod telegram {
    use messenger_webhook::{
        Adapter, Attachment, AttachmentKind, Channel, NormalizedMessage, TelegramAdapter,
    };

    fn adapter() -> TelegramAdapter {
        TelegramAdapter::new("secret")
    }

    #[test]
    fn every_media_kind_maps_onto_a_send_method() {
        let cases = [
            (AttachmentKind::Image, "photo"),
            (AttachmentKind::Video, "video"),
            (AttachmentKind::Audio, "audio"),
            (AttachmentKind::Voice, "voice"),
            (AttachmentKind::Document, "document"),
            (AttachmentKind::Sticker, "sticker"),
        ];
        for (kind, field) in cases {
            let mut message = NormalizedMessage::text(Channel::Telegram, "1", "");
            message.text = None;
            let mut attachment = Attachment::new(kind.clone());
            attachment.id = Some("file-id".to_owned());
            message.attachments.push(attachment);

            let body = adapter().encode(&message).unwrap().json().unwrap();
            assert_eq!(body[field], "file-id", "{kind:?}");
        }
    }

    #[test]
    fn media_without_a_reference_is_rejected() {
        let mut message = NormalizedMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        message
            .attachments
            .push(Attachment::new(AttachmentKind::Image));
        assert!(adapter().encode(&message).is_err());
    }

    #[test]
    fn caption_is_used_when_text_accompanies_media() {
        let mut message = NormalizedMessage::text(Channel::Telegram, "1", "Look");
        let mut attachment = Attachment::new(AttachmentKind::Video);
        attachment.id = Some("v1".to_owned());
        message.attachments.push(attachment);
        let body = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(body["video"], "v1");
        assert_eq!(body["caption"], "Look");
        assert!(body.get("text").is_none());
    }

    #[test]
    fn empty_messages_are_rejected() {
        let mut message = NormalizedMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        assert!(adapter().encode(&message).is_err());
    }

    #[test]
    fn location_cannot_be_sent_but_is_normalised_inbound() {
        let mut message = NormalizedMessage::text(Channel::Telegram, "1", "");
        message.text = None;
        message
            .attachments
            .push(Attachment::new(AttachmentKind::Location));
        assert!(adapter().encode(&message).is_err());

        let payload = br#"{"message":{"message_id":1,"chat":{"id":2},"location":{"latitude":48.7,"longitude":11.4,"name":"Ingolstadt"}}}"#;
        let events = adapter().adapt(payload).unwrap();
        assert_eq!(events[0].attachments[0].kind.as_str(), "location");
        assert_eq!(
            events[0].attachments[0].caption.as_deref(),
            Some("Ingolstadt")
        );
    }
}

#[cfg(feature = "matrix")]
mod matrix {
    use messenger_webhook::{
        Adapter, Attachment, AttachmentKind, Channel, MatrixAdapter, NormalizedMessage,
    };

    fn adapter() -> MatrixAdapter {
        MatrixAdapter::new("secret")
    }

    fn event(content: &str) -> Vec<NormalizedMessage> {
        let payload = format!(
            r#"{{"type":"m.room.message","event_id":"$1","room_id":"!r:s","sender":"@a:s","content":{content}}}"#
        );
        adapter().adapt(payload.as_bytes()).unwrap()
    }

    #[test]
    fn every_msgtype_maps_onto_a_kind() {
        let cases = [
            (
                r#"{"msgtype":"m.image","body":"a.png","url":"mxc://s/a"}"#,
                "image",
            ),
            (
                r#"{"msgtype":"m.video","body":"a.mp4","url":"mxc://s/a"}"#,
                "video",
            ),
            (
                r#"{"msgtype":"m.audio","body":"a.ogg","url":"mxc://s/a"}"#,
                "audio",
            ),
            (
                r#"{"msgtype":"m.file","body":"a.pdf","url":"mxc://s/a"}"#,
                "document",
            ),
            (
                r#"{"msgtype":"m.location","body":"here","geo_uri":"geo:48.7,11.4"}"#,
                "location",
            ),
            (r#"{"msgtype":"m.unknown_v9","body":"?"}"#, "m.unknown_v9"),
        ];
        for (content, expected) in cases {
            let events = event(content);
            assert_eq!(
                events[0].attachments[0].kind.as_str(),
                expected,
                "{content}"
            );
        }

        // A location keeps its geo uri as caption, not as a file name.
        let events = event(r#"{"msgtype":"m.location","body":"here","geo_uri":"geo:48.7,11.4"}"#);
        assert_eq!(events[0].attachments[0].filename, None);
        assert_eq!(
            events[0].attachments[0].caption.as_deref(),
            Some("geo:48.7,11.4")
        );

        // An unrecognised msgtype is carried, never fatal.
        let events = event(r#"{"msgtype":"m.unknown_v9","body":"?"}"#);
        assert_eq!(events.len(), 1);
        assert!(events[0].raw.is_some());
    }

    #[test]
    fn media_with_info_reports_every_dimension() {
        let events = event(
            r#"{"msgtype":"m.video","body":"a.mp4","url":"mxc://s/a",
               "info":{"mimetype":"video/mp4","size":900,"w":640,"h":480,"duration":12}}"#,
        );
        let attachment = &events[0].attachments[0];
        assert_eq!(attachment.mime_type.as_deref(), Some("video/mp4"));
        assert_eq!(attachment.size_bytes, Some(900));
        assert_eq!(attachment.width, Some(640));
        assert_eq!(attachment.height, Some(480));
        assert_eq!(attachment.duration_seconds, Some(12));
    }

    #[test]
    fn reaction_without_a_target_is_carried_without_a_reaction() {
        let payload = br#"{"type":"m.reaction","event_id":"$2","room_id":"!r:s","sender":"@a:s",
          "content":{"m.relates_to":{"rel_type":"m.annotation","key":"\ud83d\udc4d"}}}"#;
        let events = adapter().adapt(payload).unwrap();
        assert!(events[0].reaction.is_none());
        assert_eq!(events[0].provider_type.as_deref(), Some("m.reaction"));
    }

    #[test]
    fn receipt_shapes_that_are_not_read_receipts_do_not_panic() {
        // content is not an object.
        let events = adapter()
            .adapt(br#"{"type":"m.receipt","event_id":"$1","room_id":"!r:s","content":"nope"}"#)
            .unwrap();
        assert!(events[0].status.is_none());

        // content is an empty object.
        let events = adapter()
            .adapt(br#"{"type":"m.receipt","event_id":"$1","room_id":"!r:s","content":{}}"#)
            .unwrap();
        assert!(events[0].status.is_none());

        // An unknown receipt kind is carried verbatim.
        let events = adapter()
            .adapt(
                br#"{"type":"m.receipt","event_id":"$1","room_id":"!r:s",
                     "content":{"$m":{"m.fully_read":{"@b:s":{"ts":1}}}}}"#,
            )
            .unwrap();
        assert_eq!(
            events[0]
                .status
                .as_ref()
                .map(messenger_webhook::DeliveryStatus::as_str),
            Some("m.fully_read")
        );

        // An empty kind object.
        let events = adapter()
            .adapt(br#"{"type":"m.receipt","event_id":"$1","room_id":"!r:s","content":{"$m":{}}}"#)
            .unwrap();
        assert_eq!(
            events[0]
                .status
                .as_ref()
                .map(messenger_webhook::DeliveryStatus::as_str),
            Some("unknown")
        );

        // A receipt value that is not an object.
        let events = adapter()
            .adapt(
                br#"{"type":"m.receipt","event_id":"$1","room_id":"!r:s","content":{"$m":"garbage"}}"#,
            )
            .unwrap();
        assert_eq!(
            events[0]
                .status
                .as_ref()
                .map(messenger_webhook::DeliveryStatus::as_str),
            Some("unknown")
        );
    }

    #[test]
    fn encode_covers_every_media_kind_and_fallback() {
        let cases = [
            (AttachmentKind::Image, "m.image"),
            (AttachmentKind::Video, "m.video"),
            (AttachmentKind::Audio, "m.audio"),
            (AttachmentKind::Voice, "m.audio"),
            (AttachmentKind::Document, "m.file"),
            (AttachmentKind::Sticker, "m.sticker"),
        ];
        for (kind, msgtype) in cases {
            let mut message = NormalizedMessage::text(Channel::Matrix, "!r:s", "");
            message.text = None;
            let mut attachment = Attachment::new(kind.clone());
            attachment.url = Some("mxc://s/x".to_owned());
            message.attachments.push(attachment);

            let content = adapter().encode(&message).unwrap().json().unwrap();
            assert_eq!(content["msgtype"], msgtype, "{kind:?}");
            assert_eq!(
                content["body"],
                kind.as_str(),
                "falls back to the kind name"
            );
        }

        // With text but no filename or caption, the text becomes the body.
        let mut message = NormalizedMessage::text(Channel::Matrix, "!r:s", "Look");
        let mut attachment = Attachment::new(AttachmentKind::Image);
        attachment.url = Some("mxc://s/x".to_owned());
        attachment.duration_seconds = Some(3);
        message.attachments.push(attachment);
        let content = adapter().encode(&message).unwrap().json().unwrap();
        assert_eq!(content["body"], "Look");
        assert_eq!(content["info"]["duration"], 3);
    }

    #[test]
    fn encode_rejects_media_without_a_reference() {
        let mut message = NormalizedMessage::text(Channel::Matrix, "!r:s", "");
        message.text = None;
        message
            .attachments
            .push(Attachment::new(AttachmentKind::Image));
        assert!(adapter().encode(&message).is_err());
    }
}
