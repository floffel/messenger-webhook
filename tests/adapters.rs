//! The three adapters against real-shaped payloads.

#[cfg(feature = "whatsapp")]
mod whatsapp {
    use messenger_webhook::{Adapter, AttachmentKind, Channel, DeliveryStatus, WhatsAppAdapter};

    #[test]
    fn text_message_is_normalised() {
        let payload = br#"{"object":"whatsapp_business_account","entry":[{"id":"1","changes":[{"field":"messages",
          "value":{"metadata":{"display_phone_number":"1","phone_number_id":"2"},
          "messages":[{"id":"wamid.1","from":"4915112345678","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;

        let messages = WhatsAppAdapter::new("s").adapt(payload).unwrap();
        assert_eq!(messages.len(), 1);
        let m = &messages[0];
        assert_eq!(m.channel, Channel::WhatsApp);
        assert_eq!(m.external_id, "wamid.1");
        assert_eq!(m.sender_id, "4915112345678");
        assert_eq!(m.conversation_id, "4915112345678");
        assert_eq!(m.text.as_deref(), Some("Hallo"));
        assert_eq!(m.timestamp, Some(1_729_600_000));
        assert_eq!(m.provider_type.as_deref(), Some("text"));
        assert!(m.raw.is_some());
    }

    #[test]
    fn media_and_reply_are_normalised() {
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"wamid.2","from":"49","timestamp":"1729600000","type":"image",
             "context":{"id":"wamid.1"},
             "image":{"id":"media-1","mime_type":"image/jpeg","sha256":"abc","caption":"Look"}}]}}]}]}"#;
        let messages = WhatsAppAdapter::new("s").adapt(payload).unwrap();
        let m = &messages[0];
        assert_eq!(m.reply_to.as_deref(), Some("wamid.1"));
        assert_eq!(m.attachments.len(), 1);
        let attachment = &m.attachments[0];
        assert_eq!(attachment.kind, AttachmentKind::Image);
        assert_eq!(attachment.id.as_deref(), Some("media-1"));
        assert_eq!(attachment.mime_type.as_deref(), Some("image/jpeg"));
        assert_eq!(m.text.as_deref(), Some("Look"), "caption becomes text");
    }

    #[test]
    fn status_only_payload_yields_a_status_event() {
        let payload =
            br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages",
          "value":{"statuses":[{"id":"wamid.1","status":"read","timestamp":"1729600000","recipient_id":"49"}]}}]}]}"#;
        let events = WhatsAppAdapter::new("s").adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, Some(DeliveryStatus::Read));
        assert!(events[0].is_status_update());
        assert!(events[0].text.is_none());
    }

    #[test]
    fn malformed_payload_is_an_error() {
        assert!(WhatsAppAdapter::new("s").adapt(b"not json").is_err());
    }

    #[test]
    fn verification_fails_closed_on_empty_secret() {
        let adapter = WhatsAppAdapter::new("");
        let headers = messenger_webhook::Headers::from_iter([("x-hub-signature-256", "sha256=00")]);
        assert!(adapter.verify(&headers, b"{}").is_err());
    }
}

#[cfg(feature = "telegram")]
mod telegram {
    use messenger_webhook::{Adapter, AttachmentKind, Channel, TelegramAdapter};

    #[test]
    fn message_is_normalised() {
        let payload =
            br#"{"update_id":1,"message":{"message_id":7,"from":{"id":42,"first_name":"F"},
          "chat":{"id":-1001,"type":"supergroup"},"date":1729600000,"text":"Hallo"}}"#;
        let messages = TelegramAdapter::new("s").adapt(payload).unwrap();
        assert_eq!(messages.len(), 1);
        let m = &messages[0];
        assert_eq!(m.channel, Channel::Telegram);
        assert_eq!(m.conversation_id, "-1001");
        assert_eq!(m.sender_id, "42");
        assert_eq!(m.timestamp, Some(1_729_600_000));
    }

    #[test]
    fn photo_reply_and_thread_are_normalised() {
        let payload = br#"{"update_id":2,"message":{"message_id":9,"from":{"id":42},
          "chat":{"id":-1001},"date":1729600000,"message_thread_id":5,
          "caption":"Nice","reply_to_message":{"message_id":7},
          "photo":[{"file_id":"small","width":90,"height":90},
                   {"file_id":"large","width":1280,"height":1280,"file_size":90000}]}}"#;
        let messages = TelegramAdapter::new("s").adapt(payload).unwrap();
        let m = &messages[0];
        assert_eq!(m.reply_to.as_deref(), Some("7"));
        assert_eq!(m.thread_id.as_deref(), Some("5"));
        assert_eq!(m.text.as_deref(), Some("Nice"));
        assert_eq!(m.attachments[0].kind, AttachmentKind::Image);
        assert_eq!(m.attachments[0].id.as_deref(), Some("large"));
        assert_eq!(m.attachments[0].size_bytes, Some(90_000));
    }

    #[test]
    fn edit_marks_the_original_message() {
        let payload = br#"{"update_id":3,"edited_message":{"message_id":7,"from":{"id":42},
          "chat":{"id":-1001},"date":1729600000,"edit_date":1729600100,"text":"fixed"}}"#;
        let messages = TelegramAdapter::new("s").adapt(payload).unwrap();
        assert_eq!(messages[0].edit_of.as_deref(), Some("7"));
        assert!(messages[0].is_edit());
        assert_eq!(messages[0].timestamp, Some(1_729_600_100), "edit_date wins");
    }

    #[test]
    fn update_without_message_yields_nothing() {
        let messages = TelegramAdapter::new("s")
            .adapt(br#"{"update_id":2}"#)
            .unwrap();
        assert_eq!(messages, Vec::new());
    }
}

#[cfg(feature = "matrix")]
mod matrix {
    use messenger_webhook::{Adapter, AttachmentKind, Channel, MatrixAdapter};

    #[test]
    fn transaction_normalises_every_event() {
        let payload = br#"{"events":[
          {"type":"m.room.message","event_id":"$1","room_id":"!r:s","sender":"@a:s",
           "origin_server_ts":1729600000000,"content":{"msgtype":"m.text","body":"Hallo"}},
          {"type":"m.room.message","event_id":"$2","room_id":"!r:s","sender":"@a:s",
           "origin_server_ts":1729600001000,"content":{"msgtype":"m.image","body":"cat.jpg",
           "url":"mxc://s/cat","info":{"mimetype":"image/jpeg","size":1234,"w":10,"h":20}}}]}"#;
        let events = MatrixAdapter::new("t").adapt(payload).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].text.as_deref(), Some("Hallo"));
        assert_eq!(events[0].channel, Channel::Matrix);
        assert_eq!(events[0].timestamp, Some(1_729_600_000));
        assert_eq!(events[1].attachments[0].kind, AttachmentKind::Image);
        assert_eq!(events[1].attachments[0].size_bytes, Some(1234));
        assert_eq!(events[1].attachments[0].width, Some(10));
    }

    #[test]
    fn unknown_event_type_is_carried() {
        let payload = br#"{"type":"com.example.custom","event_id":"$3","room_id":"!r:s",
          "sender":"@a:s","content":{"anything":true}}"#;
        let events = MatrixAdapter::new("t").adapt(payload).unwrap();
        assert_eq!(
            events[0].provider_type.as_deref(),
            Some("com.example.custom")
        );
        assert!(events[0].raw.is_some());
    }
}
