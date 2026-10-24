//! Every public accessor, every enum arm, every error message.
//!
//! Doctests prove these compile and work; this file proves they are reachable
//! in one process, which is what the coverage gate measures.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code, unused_imports, unused_mut, unused_variables)]

use messenger_webhook::{
    signature, Attachment, AttachmentKind, Channel, DeliveryStatus, Error, Headers,
    NormalizedMessage, ProviderPayload, Reaction, Registry, Secret,
};

#[test]
fn channel_names_and_display() {
    assert_eq!(Channel::WhatsApp.as_str(), "whatsapp");
    assert_eq!(Channel::Telegram.as_str(), "telegram");
    assert_eq!(Channel::Matrix.as_str(), "matrix");
    assert_eq!(Channel::Other("signal".to_owned()).as_str(), "signal");

    assert_eq!(Channel::WhatsApp.to_string(), "whatsapp");
    assert_eq!(Channel::Telegram.to_string(), "telegram");
    assert_eq!(Channel::Matrix.to_string(), "matrix");
    assert_eq!(Channel::Other("signal".to_owned()).to_string(), "signal");
}

#[test]
fn attachment_kind_names() {
    let kinds = [
        (AttachmentKind::Image, "image"),
        (AttachmentKind::Video, "video"),
        (AttachmentKind::Audio, "audio"),
        (AttachmentKind::Voice, "voice"),
        (AttachmentKind::Document, "document"),
        (AttachmentKind::Sticker, "sticker"),
        (AttachmentKind::Location, "location"),
        (AttachmentKind::Contact, "contact"),
        (AttachmentKind::Other("poll".to_owned()), "poll"),
    ];
    for (kind, name) in kinds {
        assert_eq!(kind.as_str(), name);
    }
}

#[test]
fn delivery_status_names() {
    let states = [
        (DeliveryStatus::Sent, "sent"),
        (DeliveryStatus::Delivered, "delivered"),
        (DeliveryStatus::Read, "read"),
        (DeliveryStatus::Failed, "failed"),
        (DeliveryStatus::Deleted, "deleted"),
        (DeliveryStatus::Unknown("queued".to_owned()), "queued"),
    ];
    for (status, name) in states {
        assert_eq!(status.as_str(), name);
    }
}

#[test]
fn attachment_helpers() {
    let empty = Attachment::default();
    assert_eq!(empty.kind, AttachmentKind::Other("unknown".to_owned()));
    assert!(empty.id.is_none() && empty.url.is_none() && empty.caption.is_none());

    let file = Attachment::new(AttachmentKind::Document);
    assert_eq!(file.kind, AttachmentKind::Document);
    assert_eq!(file.width, None);
}

#[test]
fn normalized_message_helpers() {
    let mut message = NormalizedMessage::text(Channel::Telegram, "42", "hi");
    assert_eq!(message.conversation_id, "42");
    assert_eq!(message.text.as_deref(), Some("hi"));
    assert_eq!(message.external_id.len(), 0);
    assert!(message.first_attachment().is_none());

    message
        .attachments
        .push(Attachment::new(AttachmentKind::Image));
    assert_eq!(
        message.first_attachment().map(|a| &a.kind),
        Some(&AttachmentKind::Image)
    );

    assert!(!message.is_status_update());
    message.status = Some(DeliveryStatus::Delivered);
    assert!(message.is_status_update());

    assert!(!message.is_edit());
    message.edit_of = Some("1".to_owned());
    assert!(message.is_edit());

    let reaction = Reaction {
        message_id: "1".to_owned(),
        emoji: "👍".to_owned(),
        removed: false,
    };
    assert!(!reaction.removed);
}

#[test]
fn provider_payload_accessors() {
    let payload = ProviderPayload::new(
        Channel::Matrix,
        "PUT",
        "application/json",
        br#"{"msgtype":"m.text","body":"hi"}"#.to_vec(),
    );
    assert_eq!(payload.channel(), &Channel::Matrix);
    assert_eq!(payload.method(), "PUT");
    assert_eq!(payload.content_type(), "application/json");
    assert!(payload.body().starts_with(b"{"));
    assert_eq!(payload.json().unwrap()["body"], "hi");
    assert_eq!(payload.clone().into_body(), payload.body());
}

#[test]
fn provider_payload_rejects_non_json() {
    let payload = ProviderPayload::new(Channel::Telegram, "POST", "text/plain", b"nope".to_vec());
    assert!(matches!(payload.json(), Err(Error::Payload(_))));
}

#[test]
fn headers_api() {
    let mut headers = Headers::new();
    assert_eq!(headers.len(), 0);
    assert_eq!(headers.len(), 0);

    headers.insert("X-Hub-Signature-256", "one");
    headers.insert("x-hub-signature-256", "two");
    assert_eq!(headers.len(), 1, "same header, case-insensitively");
    assert_eq!(headers.get("X-Hub-Signature-256"), Some("two"));
    assert_eq!(headers.get("x-hub-signature-256"), Some("two"));
    assert_eq!(headers.get("missing"), None);
    assert_ne!(headers.len(), 0);

    let owned = Headers::from_pairs([("Authorization", "Bearer t".to_owned())]);
    assert_eq!(owned.get("authorization"), Some("Bearer t"));

    let collected = Headers::from_iter([("a", "1"), ("b", "2")]);
    assert_eq!(collected.len(), 2);

    let slice: &[(&str, &str)] = &[("c", "3")];
    let from_slice = Headers::from(slice);
    assert_eq!(from_slice.get("c"), Some("3"));

    assert_eq!(Headers::default(), Headers::new());
}

#[test]
fn secret_api() {
    let secret = Secret::new("token");
    // A custom `is_empty` is not the `assert_is_empty` lint's target.
    assert!(!secret.is_empty());
    assert_eq!(secret.expose_secret(), "token");
    assert!(secret.ct_eq(b"token"));
    assert!(!secret.ct_eq(b"toke"));
    assert!(!secret.ct_eq(b""));
    assert!(Secret::new("").is_empty());
    assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    assert!(!format!("{secret:?}").contains("token"));
}

#[test]
fn every_error_variant_renders_without_panicking() {
    let errors = [
        Error::Payload("not json".to_owned()),
        Error::BodyTooLarge {
            limit: 10,
            actual: 11,
        },
        Error::MissingSecret {
            channel: Channel::Telegram,
        },
        Error::MissingVerificationHeader {
            channel: Channel::Matrix,
            header: "authorization",
        },
        Error::MalformedVerificationHeader {
            channel: Channel::WhatsApp,
            reason: "expected sha256=<hex>".to_owned(),
        },
        Error::BadSignature {
            channel: Channel::WhatsApp,
        },
        Error::InvalidMessage {
            channel: Channel::Matrix,
            reason: "no text".to_owned(),
        },
        Error::MessageTooLong {
            channel: Channel::Telegram,
            limit: 4096,
            actual: 5000,
            unit: "UTF-16 units",
        },
        Error::UnknownChannel(Channel::Other("signal".to_owned())),
    ];
    for error in errors {
        let rendered = error.to_string();
        assert_ne!(rendered.len(), 0);
        assert!(!error.is_retryable(), "every error here is terminal");
        assert!(format!("{error:?}").contains(char::is_alphabetic));
    }
}

#[test]
fn signature_helpers_are_usable_from_outside() {
    let secret = Secret::new("app-secret");
    let header = signature::meta_signature(&secret, b"body").unwrap();
    assert!(header.starts_with(signature::META_SIGNATURE_PREFIX));
    assert_eq!(signature::META_SIGNATURE_HEADER, "x-hub-signature-256");
    assert!(signature::verify_meta(&secret, &header, b"body").is_ok());
    assert!(signature::verify_meta(&secret, &header, b"other").is_err());
}

#[test]
fn registry_accessors() {
    let mut registry = Registry::new();
    assert_eq!(registry.len(), 0);
    assert_eq!(registry.len(), 0);
    assert!(!registry.contains(&Channel::Matrix));
    assert_eq!(registry.channels().len(), 0);
    assert_eq!(format!("{registry:?}"), "Registry { channels: [] }");

    #[cfg(feature = "matrix")]
    {
        registry.register(messenger_webhook::MatrixAdapter::new("t"));
        assert!(registry.contains(&Channel::Matrix));
        assert_eq!(registry.len(), 1);
        assert!(!format!("{registry:?}").contains("t\""));
    }
}
