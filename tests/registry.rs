//! Registry lookup, dispatch and fail-closed handling.

use messenger_webhook::{Channel, Error, Headers, Registry};

#[test]
fn unknown_channel_is_a_typed_error() {
    let registry = Registry::new();
    let error = registry
        .get(&Channel::Other("signal".into()))
        .expect_err("unknown channel");
    assert!(matches!(error, Error::UnknownChannel(_)));
    assert_eq!(
        error.to_string(),
        "no adapter registered for channel signal"
    );

    assert!(registry
        .handle(&Channel::Other("signal".into()), &Headers::new(), b"{}")
        .is_err());
    assert!(registry.adapt(&Channel::Matrix, b"{}").is_err());
    assert!(registry
        .verify(&Channel::Telegram, &Headers::new(), b"{}")
        .is_err());
}

#[test]
fn registration_is_by_channel_and_replaceable() {
    #[cfg(feature = "telegram")]
    {
        use messenger_webhook::TelegramAdapter;

        let mut registry = Registry::new();
        registry.register(TelegramAdapter::new("first"));
        registry.register(TelegramAdapter::new("second"));

        assert_eq!(registry.len(), 1, "same channel replaces, not duplicates");
        assert_eq!(registry.channels(), vec![Channel::Telegram]);

        let headers = Headers::from_iter([("x-telegram-bot-api-secret-token", "second")]);
        assert!(registry.verify(&Channel::Telegram, &headers, b"{}").is_ok());

        let headers = Headers::from_iter([("x-telegram-bot-api-secret-token", "first")]);
        assert!(matches!(
            registry.verify(&Channel::Telegram, &headers, b"{}"),
            Err(Error::BadSignature { .. })
        ));
    }
}

#[test]
#[allow(unused_mut)]
fn channels_are_sorted_and_counted() {
    let mut registry = Registry::new();
    assert_eq!(registry.len(), 0);

    #[cfg(feature = "telegram")]
    registry.register(messenger_webhook::TelegramAdapter::new("s"));
    #[cfg(feature = "whatsapp")]
    registry.register(messenger_webhook::WhatsAppAdapter::new("s"));
    #[cfg(feature = "matrix")]
    registry.register(messenger_webhook::MatrixAdapter::new("s"));

    let channels = registry.channels();
    assert_eq!(channels, {
        let mut expected = vec![
            #[cfg(feature = "matrix")]
            Channel::Matrix,
            #[cfg(feature = "telegram")]
            Channel::Telegram,
            #[cfg(feature = "whatsapp")]
            Channel::WhatsApp,
        ];
        expected.sort();
        expected
    });
    assert_eq!(registry.len(), channels.len());
}

#[test]
fn handle_verifies_before_it_parses() {
    #[cfg(feature = "whatsapp")]
    {
        use messenger_webhook::{signature::meta_signature, Secret, WhatsAppAdapter};

        let secret = Secret::new("app-secret");
        let body = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"wamid.1","from":"49","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;

        let mut registry = Registry::new();
        registry.register(WhatsAppAdapter::new("app-secret"));

        let mut headers = Headers::new();
        headers.insert(
            "x-hub-signature-256",
            meta_signature(&secret, body).unwrap(),
        );
        let events = registry.handle(&Channel::WhatsApp, &headers, body).unwrap();
        assert_eq!(events[0].text.as_deref(), Some("Hallo"));

        // A signature from another secret is rejected as a signature problem.
        let mut forged = Headers::new();
        forged.insert(
            "x-hub-signature-256",
            meta_signature(&Secret::new("other"), body).unwrap(),
        );
        assert!(matches!(
            registry.handle(&Channel::WhatsApp, &forged, body),
            Err(Error::BadSignature { .. })
        ));

        // Missing header.
        assert!(matches!(
            registry.handle(&Channel::WhatsApp, &Headers::new(), body),
            Err(Error::MissingVerificationHeader { .. })
        ));

        // Verified but malformed: the parse error surfaces, not a panic.
        let mut broken = Headers::new();
        broken.insert(
            "x-hub-signature-256",
            meta_signature(&secret, b"nope").unwrap(),
        );
        assert!(matches!(
            registry.handle(&Channel::WhatsApp, &broken, b"nope"),
            Err(Error::Payload(_))
        ));
    }
}

#[test]
fn encode_dispatches_by_channel() {
    #[cfg(feature = "matrix")]
    {
        use messenger_webhook::{MatrixAdapter, NormalizedMessage};

        let mut registry = Registry::new();
        registry.register(MatrixAdapter::new("token"));
        let message = NormalizedMessage::text(Channel::Matrix, "!r:example.org", "hi");
        let payload = registry.encode(&Channel::Matrix, &message).unwrap();
        assert_eq!(payload.channel(), &Channel::Matrix);

        // A channel with no adapter cannot be encoded.
        assert!(matches!(
            registry.encode(&Channel::WhatsApp, &message),
            Err(Error::UnknownChannel(_))
        ));
    }
}

#[test]
fn registry_is_send_sync_and_debug_is_redacted() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Registry>();
    assert_send_sync::<Box<dyn messenger_webhook::Adapter>>();

    #[cfg(feature = "whatsapp")]
    {
        let mut registry = Registry::new();
        registry.register(messenger_webhook::WhatsAppAdapter::new("super-secret"));

        let rendered = format!("{registry:?}");
        assert!(rendered.starts_with("Registry"));
        assert!(!rendered.contains("super-secret"), "no secrets in Debug");

        // The adapter itself must not leak the secret either.
        let adapter = messenger_webhook::WhatsAppAdapter::new("super-secret");
        assert!(!format!("{adapter:?}").contains("super-secret"));
    }
}
