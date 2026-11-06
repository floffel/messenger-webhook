//! Every public accessor, every enum arm, every error message.
//!
//! Doctests prove these compile and work; this file proves they are reachable
//! in one process, which is what the coverage gate measures.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code, unused_imports, unused_mut, unused_variables)]

use messenger_webhook::{
    signature, Attachment, AttachmentKind, Channel, DeliveryStatus, Error, Headers, IdempotencyKey,
    NormalizedMessage, OutboundMessage, ProviderPayload, Reaction, Registry, RetryPolicy, Secret,
    MAX_BODY_READ_BYTES, MAX_IDEMPOTENCY_KEY_BYTES,
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
    let mut message = NormalizedMessage::inbound_text(Channel::Telegram, "7", "42", "hi");
    assert_eq!(message.conversation_id, "42");
    assert_eq!(message.text.as_deref(), Some("hi"));
    assert_eq!(message.external_id, "7");
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
fn outbound_message_helpers() {
    let message = OutboundMessage::text(Channel::WhatsApp, "4915112345678", "hi");
    assert_eq!(message.channel, Channel::WhatsApp);
    assert_eq!(message.conversation_id, "4915112345678");
    assert_eq!(message.text.as_deref(), Some("hi"));
    assert!(message.first_attachment().is_none());
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
fn provider_payload_retry_contract_accessors() {
    let unsafe_payload =
        ProviderPayload::new(Channel::WhatsApp, "POST", "application/json", vec![]);
    assert_eq!(unsafe_payload.retry(), RetryPolicy::UnsafeUnlessKeyPresent);
    assert!(unsafe_payload.retry().is_unsafe_without_key());
    assert!(unsafe_payload.idempotency_key().is_none());

    let key = IdempotencyKey::new("txn-1").unwrap();
    let safe_payload = ProviderPayload::with_retry(
        Channel::Matrix,
        "PUT",
        "application/json",
        vec![],
        RetryPolicy::Safe,
        Some(key.clone()),
    );
    assert_eq!(safe_payload.retry().as_str(), "safe");
    assert!(!safe_payload.retry().is_unsafe_without_key());
    assert_eq!(safe_payload.idempotency_key(), Some(&key));
}

#[test]
fn idempotency_key_boundaries() {
    assert!(matches!(
        IdempotencyKey::new(""),
        Err(Error::InvalidIdempotencyKey { .. })
    ));

    let at_limit = IdempotencyKey::new("a".repeat(MAX_IDEMPOTENCY_KEY_BYTES)).unwrap();
    assert_eq!(at_limit.as_str().len(), MAX_IDEMPOTENCY_KEY_BYTES);

    assert!(matches!(
        IdempotencyKey::new("a".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)),
        Err(Error::InvalidIdempotencyKey { .. })
    ));
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
    assert_eq!(
        headers.get_all("x-hub-signature-256").collect::<Vec<_>>(),
        ["two"]
    );
    assert_eq!(headers.get("missing"), None);
    assert_ne!(headers.len(), 0);

    headers.append("X-Hub-Signature-256", "three");
    headers.append("x-hub-signature-256", "four");
    assert_eq!(headers.len(), 3);
    assert_eq!(headers.get("X-Hub-Signature-256"), Some("four"));
    assert_eq!(
        headers.get_all("X-Hub-Signature-256").collect::<Vec<_>>(),
        ["two", "three", "four"]
    );

    headers.insert("X-Hub-Signature-256", "five");
    assert_eq!(headers.len(), 1);
    assert_eq!(headers.get("X-Hub-Signature-256"), Some("five"));

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
fn header_lookup_is_case_insensitive_in_place() {
    // Every ASCII casing of the same name must find the stored entry, with the
    // stored value, whether the query is all lower, all upper or mixed case.
    let headers = Headers::from_iter([("x-telegram-bot-api-secret-token", "secret")]);

    for query in [
        "x-telegram-bot-api-secret-token",
        "X-TELEGRAM-BOT-API-SECRET-TOKEN",
        "X-Telegram-Bot-Api-Secret-Token",
        "x-Telegram-BOT-Api-Secret-Token",
    ] {
        assert_eq!(headers.get(query), Some("secret"), "query {query:?}");
    }

    // A query that differs in more than case is still a miss, and an empty
    // name does not match any stored header.
    assert_eq!(headers.get("x-telegram-bot-api-secret-tokens"), None);
    assert_eq!(headers.get(""), None);

    // Non-ASCII bytes are compared byte-for-byte: a name that only differs by
    // Unicode case folding is not the same header.
    let unicode = Headers::from_iter([("x-über", "v")]);
    assert_eq!(unicode.get("x-über"), Some("v"));
    assert_eq!(unicode.get("X-ÜBER"), None, "ASCII folding only");
}

#[test]
fn headers_debug_redacts_credential_values() {
    let authorization = "Bearer representative-authorization-token";
    let telegram = "representative-telegram-secret-token";
    let meta = "sha256=representative-meta-signature";
    let headers = Headers::from_iter([
        ("Authorization", authorization),
        ("X-Telegram-Bot-Api-Secret-Token", telegram),
        ("X-Hub-Signature-256", meta),
    ]);

    let rendered = format!("{headers:?}");
    for value in [authorization, telegram, meta] {
        assert!(
            !rendered.contains(value),
            "Debug leaked a credential: {rendered}"
        );
    }
    assert!(
        rendered.contains("<redacted>"),
        "Debug should mark values as redacted: {rendered}"
    );
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
fn bounded_body_read_api() {
    assert_eq!(MAX_BODY_READ_BYTES, messenger_webhook::MAX_BODY_BYTES + 1);
    assert_eq!(messenger_webhook::read_body(&b"{}"[..]).unwrap(), b"{}");
}

#[test]
fn every_error_variant_renders_without_panicking() {
    let errors = [
        Error::Payload("not json".to_owned()),
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset",
        )),
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
        Error::DuplicateVerificationHeader {
            channel: Channel::Telegram,
            header: "x-telegram-bot-api-secret-token",
            count: 2,
        },
        Error::MalformedVerificationHeader {
            channel: Channel::WhatsApp,
            reason: "expected sha256=<hex>".to_owned(),
        },
        Error::BadSignature {
            channel: Channel::WhatsApp,
        },
        Error::Hmac {
            source: hmac::digest::InvalidLength,
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
        Error::InvalidIdempotencyKey {
            reason: "empty".to_owned(),
        },
        Error::UnknownChannel(Channel::Other("signal".to_owned())),
        Error::ChannelAlreadyRegistered(Channel::Telegram),
    ];
    for error in errors {
        let rendered = error.to_string();
        assert_ne!(rendered.len(), 0);
        assert!(!error.is_retryable(), "every error here is terminal");
        assert!(format!("{error:?}").contains(char::is_alphabetic));
    }
}

#[test]
fn hmac_construction_error_keeps_its_source_chain() {
    let error = Error::Hmac {
        source: hmac::digest::InvalidLength,
    };

    assert_eq!(error.to_string(), "could not construct HMAC-SHA256");
    assert!(std::error::Error::source(&error).is_some());
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
    assert_eq!(format!("{registry:?}"), "Registry { adapters: {} }");

    #[cfg(feature = "matrix")]
    {
        registry.register(messenger_webhook::MatrixAdapter::new("t"));
        assert!(registry.contains(&Channel::Matrix));
        assert_eq!(registry.len(), 1);
        assert!(!format!("{registry:?}").contains("t\""));

        let error = registry
            .try_register(messenger_webhook::MatrixAdapter::new("u"))
            .expect_err("matrix is already registered");
        assert!(matches!(
            error,
            Error::ChannelAlreadyRegistered(Channel::Matrix)
        ));
    }
}
