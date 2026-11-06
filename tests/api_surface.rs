//! Every public accessor, every enum arm, every error message.
//!
//! Doctests prove these compile and work; this file proves they are reachable
//! in one process, which is what the coverage gate measures.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code, unused_imports, unused_mut, unused_variables)]

use std::collections::{BTreeSet, HashSet};

use messenger_webhook::{
    signature, Attachment, AttachmentKind, Channel, ChannelName, DeliveryStatus, Error, Headers,
    IdempotencyKey, InteractiveReply, LimitUnit, NormalizedMessage, OutboundMessage,
    ProviderMethod, ProviderPayload, Reaction, Registry, RetryPolicy, Secret, MAX_BODY_BYTES,
    MAX_BODY_READ_BYTES, MAX_CHANNEL_NAME_BYTES, MAX_IDEMPOTENCY_KEY_BYTES,
    MAX_INTERACTIVE_ID_BYTES, MAX_INTERACTIVE_TITLE_CHARS,
};
use std::str::FromStr;

#[test]
fn channel_names_and_display() {
    let signal = Channel::other("signal").unwrap();

    assert_eq!(Channel::WhatsApp.as_str(), "whatsapp");
    assert_eq!(Channel::Messenger.as_str(), "messenger");
    assert_eq!(Channel::Telegram.as_str(), "telegram");
    assert_eq!(Channel::Matrix.as_str(), "matrix");
    assert_eq!(signal.as_str(), "signal");
    assert_eq!(Channel::other("rss-feed").unwrap().as_str(), "rss-feed");
    assert_eq!(Channel::other(" Signal ").unwrap(), signal);
    assert_eq!(MAX_CHANNEL_NAME_BYTES, 255);

    assert_eq!(Channel::WhatsApp.to_string(), "whatsapp");
    assert_eq!(Channel::Messenger.to_string(), "messenger");
    assert_eq!(Channel::Telegram.to_string(), "telegram");
    assert_eq!(Channel::Matrix.to_string(), "matrix");
    assert_eq!(signal.to_string(), "signal");

    assert_eq!(Channel::from_str("whatsapp").unwrap(), Channel::WhatsApp);
    assert_eq!(Channel::from_str("messenger").unwrap(), Channel::Messenger);
    assert_eq!(Channel::from_str("telegram").unwrap(), Channel::Telegram);
    assert_eq!(Channel::from_str("matrix").unwrap(), Channel::Matrix);
    assert_eq!(
        Channel::from_str("signal").unwrap(),
        Channel::other("signal").unwrap()
    );
}

#[test]
fn channel_identity_follows_the_canonical_name() {
    let mut hashed = HashSet::new();
    hashed.insert(Channel::WhatsApp);
    assert!(!hashed.insert(Channel::WhatsApp));

    let sorted = BTreeSet::from([
        Channel::other("z-custom").unwrap(),
        Channel::Messenger,
        Channel::Telegram,
        Channel::Matrix,
        Channel::WhatsApp,
    ]);
    assert_eq!(
        sorted
            .into_iter()
            .map(|channel| channel.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["matrix", "messenger", "telegram", "whatsapp", "z-custom"]
    );
}

#[test]
fn custom_channel_names_are_bounded_and_unreserved() {
    assert!(Channel::other("").is_err());
    assert!(Channel::other("   ").is_err());
    assert!(Channel::other("a".repeat(MAX_CHANNEL_NAME_BYTES)).is_ok());
    assert!(Channel::other("a".repeat(MAX_CHANNEL_NAME_BYTES + 1)).is_err());
    assert!(Channel::other("has space").is_err());
    assert!(Channel::other("whatsapp").is_err());
    assert!(Channel::other("messenger").is_err());
    assert!(Channel::other("telegram").is_err());
    assert!(Channel::other("matrix").is_err());

    assert_eq!(Channel::other("UPPER").unwrap().as_str(), "upper");

    for name in ["a", "rss-feed", "sms_v2"] {
        assert_eq!(Channel::other(name).unwrap().as_str(), name);
    }
}

#[test]
fn channel_name_is_opaque() {
    let channel = Channel::other("signal").unwrap();
    let Channel::Other(name) = channel else {
        panic!("Channel::other must produce Channel::Other");
    };

    let name: &ChannelName = &name;
    assert_eq!(name.as_str(), "signal");
    assert_eq!(name.to_string(), "signal");
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
        assert_eq!(AttachmentKind::from_str(name).unwrap(), kind);
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
        assert_eq!(DeliveryStatus::from_str(name).unwrap(), status);
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

    let mut location = Attachment::new(AttachmentKind::Location);
    location.latitude = serde_json::Number::from_f64(48.7);
    location.longitude = serde_json::Number::from_f64(11.4);
    assert_eq!(
        location
            .latitude
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(48.7)
    );

    let mut contact = Attachment::new(AttachmentKind::Contact);
    contact.phone_number = Some("+4915112345678".to_owned());
    contact.first_name = Some("Florian".to_owned());
    assert_eq!(contact.phone_number.as_deref(), Some("+4915112345678"));
}

#[test]
fn normalized_message_helpers() {
    let mut message = NormalizedMessage::inbound_text(Channel::Telegram, "7", "42", "hi");
    assert_eq!(message.conversation_id, "42");
    assert_eq!(message.text(), Some("hi"));
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

    let reaction = Reaction::add("1", "👍").unwrap();
    assert_eq!(reaction.message_id(), "1");
    assert_eq!(reaction.emoji(), "👍");
    assert!(!reaction.removed());
    assert_eq!(reaction.added_reactions(), ["👍"]);
    assert_eq!(reaction.removed_reactions(), [] as [String; 0]);
    assert!(Reaction::add("1", "").is_err());

    let reply = InteractiveReply::new("yes", "Yes").unwrap();
    assert_eq!(reply.id(), "yes");
    assert_eq!(reply.title(), "Yes");
    message.interactive_reply = Some(reply.clone());
    assert_eq!(
        message.interactive_reply().map(InteractiveReply::id),
        Some("yes")
    );
    let message = NormalizedMessage::inbound_text(Channel::WhatsApp, "8", "42", "Yes")
        .with_interactive_reply(reply);
    assert_eq!(
        message.interactive_reply().map(InteractiveReply::title),
        Some("Yes")
    );
}

#[test]
fn interactive_reply_boundaries() {
    assert!(InteractiveReply::new("id", "Title").is_ok());

    // Id: empty and non-graphic values are rejected, the byte ceiling holds.
    for placeholder in ["", " ", "\t\n", "\u{200b}"] {
        assert!(
            matches!(
                InteractiveReply::new(placeholder, "Title"),
                Err(Error::InvalidInteractiveReply { .. })
            ),
            "{placeholder:?} must not be a reply id"
        );
    }
    assert!(InteractiveReply::new("a".repeat(MAX_INTERACTIVE_ID_BYTES), "Title").is_ok());
    assert!(InteractiveReply::new("a".repeat(MAX_INTERACTIVE_ID_BYTES + 1), "Title").is_err());

    // Title: empty is rejected, the character ceiling holds on the boundary.
    assert!(InteractiveReply::new("id", "").is_err());
    assert!(InteractiveReply::new("id", "a".repeat(MAX_INTERACTIVE_TITLE_CHARS)).is_ok());
    assert!(InteractiveReply::new("id", "a".repeat(MAX_INTERACTIVE_TITLE_CHARS + 1)).is_err());
    assert_eq!(MAX_INTERACTIVE_ID_BYTES, 256);
    assert_eq!(MAX_INTERACTIVE_TITLE_CHARS, 20);
}

#[test]
fn reaction_add_and_remove_have_one_canonical_representation() {
    let added = Reaction::add("1", "👍").unwrap();
    assert_eq!(added.message_id(), "1");
    assert_eq!(added.emoji(), "👍");
    assert!(!added.removed());
    assert_eq!(added.added_reactions(), ["👍"]);
    assert_eq!(added.removed_reactions(), [] as [String; 0]);

    let removed = Reaction::remove("1");
    assert_eq!(removed.message_id(), "1");
    assert_eq!(removed.emoji(), "");
    assert!(removed.removed());
    assert_eq!(removed.added_reactions(), [] as [String; 0]);
    assert_eq!(removed.removed_reactions(), [] as [String; 0]);

    for key in ["", " ", "\t\n", "\u{200b}"] {
        assert!(
            matches!(
                Reaction::add("1", key),
                Err(Error::InvalidReactionKey { .. })
            ),
            "{key:?} must not be an added reaction key"
        );
    }
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
        ProviderMethod::Put,
        "application/json",
        br#"{"msgtype":"m.text","body":"hi"}"#.to_vec(),
    )
    .unwrap();
    assert_eq!(payload.channel(), &Channel::Matrix);
    assert_eq!(payload.method(), "PUT");
    assert_eq!(payload.content_type(), "application/json");
    assert!(payload.body().starts_with(b"{"));
    assert_eq!(payload.json().unwrap()["body"], "hi");
    assert_eq!(payload.clone().into_body(), payload.body());
}

#[test]
fn provider_payload_retry_contract_accessors() {
    let unsafe_payload = ProviderPayload::new(
        Channel::WhatsApp,
        ProviderMethod::Post,
        "application/json",
        b"{}".to_vec(),
    )
    .unwrap();
    assert_eq!(unsafe_payload.retry(), RetryPolicy::UnsafeUnlessKeyPresent);
    assert!(unsafe_payload.retry().is_unsafe_without_key());
    assert!(unsafe_payload.idempotency_key().is_none());

    let key = IdempotencyKey::new("txn-1").unwrap();
    let safe_payload = ProviderPayload::with_retry(
        Channel::Matrix,
        ProviderMethod::Put,
        "application/json",
        b"{}".to_vec(),
        RetryPolicy::Safe,
        Some(key.clone()),
    )
    .unwrap();
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
fn provider_payload_rejects_non_json_content_type_and_body() {
    assert!(matches!(
        ProviderPayload::new(
            Channel::Telegram,
            ProviderMethod::Post,
            "application/json",
            Vec::new(),
        ),
        Err(Error::Json { .. })
    ));
    assert!(matches!(
        ProviderPayload::new(
            Channel::Telegram,
            ProviderMethod::Post,
            "text/plain",
            b"nope".to_vec(),
        ),
        Err(Error::Payload(_))
    ));
    assert!(matches!(
        ProviderPayload::new(
            Channel::Telegram,
            ProviderMethod::Post,
            "application/json",
            b"nope".to_vec(),
        ),
        Err(Error::Json { .. })
    ));
    assert!(matches!(
        ProviderPayload::new(
            Channel::Telegram,
            ProviderMethod::Post,
            "application/json",
            b"[1]".to_vec(),
        ),
        Err(Error::Payload(_))
    ));
}

#[test]
fn provider_payload_body_limit_is_exact() {
    let json_prefix = br#"{"x":""#.len();
    let mut at_limit = br#"{"x":""#.to_vec();
    at_limit.extend(std::iter::repeat_n(b'a', MAX_BODY_BYTES - json_prefix - 2));
    at_limit.extend_from_slice(br#""}"#);
    assert_eq!(at_limit.len(), MAX_BODY_BYTES);

    assert!(ProviderPayload::new(
        Channel::Telegram,
        ProviderMethod::Post,
        "application/json",
        at_limit,
    )
    .is_ok());

    let over_limit = vec![b' '; MAX_BODY_BYTES + 1];
    assert!(matches!(
        ProviderPayload::new(
            Channel::Telegram,
            ProviderMethod::Post,
            "application/json",
            over_limit,
        ),
        Err(Error::BodyTooLarge {
            limit: MAX_BODY_BYTES,
            actual,
        }) if actual == MAX_BODY_BYTES + 1
    ));
}

#[test]
fn headers_api() {
    let mut headers = Headers::new();
    assert_eq!(headers.len(), 0);
    assert_eq!(headers.len(), 0);

    headers.insert("X-Hub-Signature-256", "one").unwrap();
    headers.insert("x-hub-signature-256", "two").unwrap();
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

    headers.insert("X-Hub-Signature-256", "five").unwrap();
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
    let secret = Secret::new("secret-secret-secret").expect("secret");
    // A custom `is_empty` is not the `assert_is_empty` lint's target.
    assert!(!secret.is_empty());
    assert_eq!(secret.expose_secret(), "secret-secret-secret");
    assert!(secret.verify(b"secret-secret-secret").unwrap());
    assert!(!secret.verify(b"toke").unwrap());
    assert!(!secret.verify(b"").unwrap());
    assert!(matches!(
        Secret::new(""),
        Err(Error::WeakSecret {
            actual: 0,
            minimum: messenger_webhook::secret::MIN_SECRET_BYTES,
        })
    ));
    assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    assert!(!format!("{secret:?}").contains("secret-secret-secret"));
}

#[test]
fn bounded_body_read_api() {
    assert_eq!(MAX_BODY_READ_BYTES, messenger_webhook::MAX_BODY_BYTES + 1);
    assert_eq!(messenger_webhook::read_body(&b"{}"[..]).unwrap(), b"{}");
}

#[test]
fn every_error_variant_renders_without_panicking() {
    let errors = terminal_error_variants()
        .into_iter()
        .map(|error| (error, false, false))
        .chain([
            (
                Error::Transport {
                    channel: Channel::Messenger,
                    reason: "dns failure".to_owned(),
                },
                true,
                false,
            ),
            (
                Error::Ambiguous {
                    channel: Channel::Messenger,
                    idempotency_key: Some(IdempotencyKey::new("txn-1").unwrap()),
                    reason: "read timeout".to_owned(),
                },
                false,
                true,
            ),
        ]);
    for (error, retryable, ambiguous) in errors {
        let rendered = error.to_string();
        assert_ne!(rendered.len(), 0);
        assert_eq!(error.is_retryable(), retryable, "{error:?}");
        assert_eq!(error.is_ambiguous(), ambiguous, "{error:?}");
        assert!(!(error.is_retryable() && error.is_ambiguous()), "{error:?}");
        assert!(format!("{error:?}").contains(char::is_alphabetic));
    }
}

fn terminal_error_variants() -> Vec<Error> {
    vec![
        Error::Payload("not json".to_owned()),
        Error::Json {
            source: serde_json::from_slice::<serde_json::Value>(b"not json").unwrap_err(),
        },
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
        Error::WeakSecret {
            actual: 15,
            minimum: 16,
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
        Error::MalformedVerificationRequest {
            channel: Channel::WhatsApp,
            reason: "hub.mode is not subscribe".to_owned(),
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
            unit: LimitUnit::Utf16Units,
        },
        Error::InvalidIdempotencyKey {
            reason: "empty".to_owned(),
        },
        Error::InvalidHeader {
            reason: "header name is empty".to_owned(),
        },
        Error::InvalidChannelName {
            reason: "empty".to_owned(),
        },
        Error::InvalidReactionKey {
            reason: "empty".to_owned(),
        },
        Error::InvalidInteractiveReply {
            reason: "empty".to_owned(),
        },
        Error::UnknownChannel(Channel::other("signal").unwrap()),
        Error::ChannelAlreadyRegistered(Channel::Telegram),
    ]
}

#[test]
fn ambiguous_error_exposes_its_idempotency_key() {
    let key = IdempotencyKey::new("txn-1").unwrap();
    let ambiguous = Error::Ambiguous {
        channel: Channel::Matrix,
        idempotency_key: Some(key.clone()),
        reason: "timeout".to_owned(),
    };
    let transport = Error::Transport {
        channel: Channel::Matrix,
        reason: "dns failure".to_owned(),
    };

    assert_eq!(ambiguous.idempotency_key(), Some(&key));
    assert!(ambiguous.to_string().contains("txn-1"));
    assert!(transport.idempotency_key().is_none());
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
    let secret = Secret::new("app-secret-app-secret").expect("secret");
    let previous = Secret::new("previous-secret-previous").expect("secret");
    let header = signature::meta_signature(&secret, b"body").unwrap();
    let previous_header = signature::meta_signature(&previous, b"body").unwrap();
    assert!(header.starts_with(signature::META_SIGNATURE_PREFIX));
    assert_eq!(signature::META_SIGNATURE_HEADER, "x-hub-signature-256");
    assert!(signature::verify_meta(&secret, &header, b"body").is_ok());
    assert!(signature::verify_meta(&secret, &header, b"other-secret-value").is_err());
    assert!(signature::verify_meta_any(
        &[previous.clone(), secret.clone()],
        &previous_header,
        b"body"
    )
    .is_ok());
}

#[test]
#[cfg(feature = "whatsapp")]
fn whatsapp_adapter_accepts_a_rotation_window() {
    use messenger_webhook::{Adapter, Headers, WhatsAppAdapter};

    let previous = Secret::new("previous-secret-previous").expect("secret");
    let current = Secret::new("current-secret-current").expect("secret");
    let adapter = WhatsAppAdapter::with_secret_rotation([previous.clone(), current.clone()])
        .expect("rotation adapter");

    for secret in [&previous, &current] {
        let headers = Headers::from_pairs([(
            "x-hub-signature-256",
            signature::meta_signature(secret, b"body").unwrap(),
        )]);
        assert!(adapter.verify(&headers, b"body").is_ok());
    }

    let forged = Headers::from_pairs([(
        "x-hub-signature-256",
        signature::meta_signature(
            &Secret::new("forged-secret-forged").expect("secret"),
            b"body",
        )
        .unwrap(),
    )]);
    assert!(matches!(
        adapter.verify(&forged, b"body"),
        Err(Error::BadSignature {
            channel: Channel::WhatsApp
        })
    ));
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
        registry.register(
            messenger_webhook::MatrixAdapter::new("matrix-secret-value").expect("adapter"),
        );
        assert!(registry.contains(&Channel::Matrix));
        assert_eq!(registry.len(), 1);
        assert!(!format!("{registry:?}").contains("t\""));

        let error = registry
            .try_register(
                messenger_webhook::MatrixAdapter::new("matrix-secret-other").expect("adapter"),
            )
            .expect_err("matrix is already registered");
        assert!(matches!(
            error,
            Error::ChannelAlreadyRegistered(Channel::Matrix)
        ));
    }
}
