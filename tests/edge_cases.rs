//! The just-valid / just-invalid matrix for every public entry point:
//! empty, oversized, missing header, wrong secret shape, duplicate keys,
//! unknown types, 20 recipients, unicode, NUL and wrong JSON types.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code, unused_imports, unused_mut, unused_variables)]

use messenger_webhook::{read_body, Adapter, Error, Headers, MAX_BODY_BYTES, MAX_BODY_READ_BYTES};

#[cfg(feature = "whatsapp")]
fn whatsapp() -> messenger_webhook::WhatsAppAdapter {
    messenger_webhook::WhatsAppAdapter::new("app-secret")
}

#[cfg(feature = "telegram")]
fn telegram() -> messenger_webhook::TelegramAdapter {
    messenger_webhook::TelegramAdapter::new("bot-secret")
}

#[cfg(feature = "matrix")]
fn matrix() -> messenger_webhook::MatrixAdapter {
    messenger_webhook::MatrixAdapter::new("hs-token")
}

/// Every adapter that is compiled into this build.
#[allow(clippy::vec_init_then_push, unused_mut)]
fn all_adapters() -> Vec<Box<dyn Adapter>> {
    let mut adapters: Vec<Box<dyn Adapter>> = Vec::new();
    #[cfg(feature = "whatsapp")]
    adapters.push(Box::new(whatsapp()));
    #[cfg(feature = "telegram")]
    adapters.push(Box::new(telegram()));
    #[cfg(feature = "matrix")]
    adapters.push(Box::new(matrix()));
    adapters
}

#[test]
fn empty_body_is_a_payload_error_not_a_panic() {
    for adapter in all_adapters() {
        assert!(matches!(adapter.adapt(b""), Err(Error::Payload(_))));
        assert!(matches!(adapter.adapt(b"   "), Err(Error::Payload(_))));
    }
}

#[test]
fn body_size_is_checked_before_hashing() {
    let at_limit = vec![b' '; MAX_BODY_BYTES];
    let over_limit = vec![b' '; MAX_BODY_BYTES + 1];

    for adapter in all_adapters() {
        // At the limit: the size gate passes, the JSON gate rejects.
        assert!(matches!(adapter.adapt(&at_limit), Err(Error::Payload(_))));
        assert!(matches!(
            adapter.verify(&Headers::new(), &at_limit),
            Err(Error::MissingVerificationHeader { .. })
        ));

        // One byte over: rejected before any header or hash handling.
        assert!(matches!(
            adapter.adapt(&over_limit),
            Err(Error::BodyTooLarge {
                limit: MAX_BODY_BYTES,
                actual,
            }) if actual == MAX_BODY_BYTES + 1
        ));
        assert!(matches!(
            adapter.verify(&Headers::new(), &over_limit),
            Err(Error::BodyTooLarge { .. })
        ));
    }
}

#[test]
fn read_body_accepts_empty_and_exactly_the_limit() {
    assert_eq!(read_body(&b""[..]).unwrap(), b"");

    let at_limit = vec![b' '; MAX_BODY_BYTES];
    assert_eq!(read_body(&at_limit[..]).unwrap(), at_limit);
}

#[test]
fn read_body_rejects_one_over_the_limit_without_reading_further() {
    struct CountingReader {
        bytes: Vec<u8>,
        read: usize,
    }

    impl std::io::Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let remaining = self.bytes.len().saturating_sub(self.read);
            let read = remaining.min(buffer.len());
            buffer[..read].copy_from_slice(&self.bytes[self.read..self.read + read]);
            self.read += read;
            Ok(read)
        }
    }

    let mut reader = CountingReader {
        bytes: vec![b' '; MAX_BODY_READ_BYTES + 4096],
        read: 0,
    };
    let error = read_body(&mut reader).unwrap_err();

    assert!(matches!(
        error,
        Error::BodyTooLarge {
            limit: MAX_BODY_BYTES,
            actual: MAX_BODY_READ_BYTES,
        }
    ));
    assert_eq!(
        reader.read, MAX_BODY_READ_BYTES,
        "read_body must not consume bytes after the sentinel"
    );
}

#[test]
fn read_body_reports_io_failures() {
    struct Broken;

    impl std::io::Read for Broken {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset",
            ))
        }
    }

    assert!(matches!(read_body(Broken), Err(Error::Io(_))));
}

#[test]
fn read_body_retries_interrupted_reads() {
    struct InterruptedOnce {
        calls: usize,
    }

    impl std::io::Read for InterruptedOnce {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "try again",
                ))
            } else {
                buffer[0] = b'{';
                Ok(1)
            }
        }
    }

    let mut reader = InterruptedOnce { calls: 0 };
    let body = std::io::Read::take(&mut reader, 1);
    assert_eq!(read_body(body).unwrap(), b"{");
}

#[test]
fn root_must_be_an_object() {
    for payload in [&b"[]"[..], b"\"text\"", b"null", b"42", b"true"] {
        for adapter in all_adapters() {
            assert!(
                matches!(adapter.adapt(payload), Err(Error::Payload(_))),
                "{} accepted {payload:?}",
                adapter.channel()
            );
        }
    }
}

#[test]
fn deeply_nested_json_fails_instead_of_overflowing_the_stack() {
    let deep = "[".repeat(200);
    for adapter in all_adapters() {
        assert!(matches!(
            adapter.adapt(deep.as_bytes()),
            Err(Error::Payload(_))
        ));
    }
}

#[cfg(feature = "whatsapp")]
mod whatsapp_edges {
    use super::{whatsapp, Error, Headers};
    use messenger_webhook::{signature::meta_signature, Adapter, Channel, Secret, WhatsAppAdapter};

    fn signed(body: &[u8]) -> Headers {
        Headers::from_pairs([(
            "x-hub-signature-256",
            meta_signature(&Secret::new("app-secret"), body).unwrap(),
        )])
    }

    #[test]
    fn missing_header_is_reported_by_name() {
        let error = whatsapp().verify(&Headers::new(), b"{}").unwrap_err();
        assert!(matches!(
            &error,
            Error::MissingVerificationHeader { header, .. } if *header == "x-hub-signature-256"
        ));
        assert!(error.to_string().contains("x-hub-signature-256"));
    }

    #[test]
    fn empty_secret_fails_closed_even_with_a_valid_looking_header() {
        let adapter = WhatsAppAdapter::new("");
        let headers =
            Headers::from_pairs([("x-hub-signature-256", format!("sha256={}", "0".repeat(64)))]);
        assert!(matches!(
            adapter.verify(&headers, b"{}"),
            Err(Error::MissingSecret { .. })
        ));
    }

    #[test]
    fn duplicate_signature_headers_fail_closed() {
        let body = b"{}";
        let valid = meta_signature(&Secret::new("app-secret"), body).unwrap();

        let single = Headers::from_pairs([("x-hub-signature-256", valid.clone())]);
        assert!(whatsapp().verify(&single, body).is_ok());

        for values in [
            [valid.clone(), valid.clone()],
            [valid.clone(), "sha256=00".to_owned()],
            ["sha256=00".to_owned(), valid.clone()],
        ] {
            let headers = Headers::from_pairs([
                ("x-hub-signature-256", values[0].as_str()),
                ("X-Hub-Signature-256", values[1].as_str()),
            ]);
            assert!(matches!(
                whatsapp().verify(&headers, body),
                Err(Error::DuplicateVerificationHeader {
                    channel: Channel::WhatsApp,
                    header: "x-hub-signature-256",
                    count: 2,
                })
            ));
        }
    }

    /// A zero-length HMAC key is legal, so an unset secret would otherwise
    /// verify `sha256=HMAC("", body)` for *any* body — an attacker forges it.
    #[test]
    fn empty_secret_rejects_a_correctly_computed_empty_key_hmac() {
        // HMAC-SHA256(key = "", message = "{}"), computed independently:
        //   python3 -c "import hmac,hashlib;print(hmac.new(b'',b'{}',hashlib.sha256).hexdigest())"
        const FORGED: &str =
            "sha256=22f8eea909400af98adf3681a9f31923ef6b7fcba4abb553d92823a3e9d5c25e";

        // The public verifier refuses before it hashes anything.
        assert!(matches!(
            messenger_webhook::signature::verify_meta(&Secret::new(""), FORGED, b"{}"),
            Err(Error::MissingSecret { .. })
        ));

        // ... and it refuses with a header that is not even well formed, i.e.
        // the empty-secret check runs before parsing.
        assert!(matches!(
            messenger_webhook::signature::verify_meta(&Secret::new(""), "sha256=nothex", b"{}"),
            Err(Error::MissingSecret { .. })
        ));

        // The signer refuses too, so a mock/caller cannot mint an empty-key
        // signature in the first place.
        assert!(matches!(
            meta_signature(&Secret::new(""), b"{}"),
            Err(Error::MissingSecret { .. })
        ));

        // Boundary pair: the shortest non-empty secret is a real key and
        // produces a signature that verifies only for its own body.
        let shortest = Secret::new("x");
        let header = meta_signature(&shortest, b"{}").expect("one-byte key signs");
        assert_ne!(header, FORGED);
        assert!(messenger_webhook::signature::verify_meta(&shortest, &header, b"{}").is_ok());
        assert!(matches!(
            messenger_webhook::signature::verify_meta(&shortest, FORGED, b"{}"),
            Err(Error::BadSignature { .. })
        ));
    }

    #[test]
    fn signature_header_shape_is_enforced() {
        let body = b"{}";
        let valid = meta_signature(&Secret::new("app-secret"), body).unwrap();
        let hex = valid.trim_start_matches("sha256=");

        let cases = [
            ("no prefix", hex.to_owned()),
            ("empty digest", "sha256=".to_owned()),
            ("one nibble short", format!("sha256={}", &hex[1..])),
            ("one nibble long", format!("sha256={hex}0")),
            ("non-hex digit", format!("sha256={}", "z".repeat(64))),
            (
                "64 bytes with a non-ascii byte",
                format!("sha256={}ü", "0".repeat(62)),
            ),
            ("sha1 prefix", format!("sha1={}", &hex[..40])),
            ("leading space", format!(" {valid}")),
            ("wrong case prefix", format!("SHA256={hex}")),
        ];
        for (label, header) in cases {
            let headers = Headers::from_iter([("x-hub-signature-256", header.as_str())]);
            assert!(
                matches!(
                    whatsapp().verify(&headers, body),
                    Err(Error::MalformedVerificationHeader { .. })
                ),
                "{label} was not rejected as malformed"
            );
        }

        // Upper-case hex is still the same digest and must be accepted.
        let upper = format!("sha256={}", hex.to_uppercase());
        let headers = Headers::from_iter([("x-hub-signature-256", upper.as_str())]);
        assert!(whatsapp().verify(&headers, body).is_ok());
    }

    #[test]
    fn signature_is_bound_to_the_exact_body() {
        let headers = signed(br#"{"entry":[]}"#);
        assert!(whatsapp().verify(&headers, br#"{"entry":[]}"#).is_ok());

        // Same length, one byte different.
        assert!(matches!(
            whatsapp().verify(&headers, br#"{"entry":[ ]}"#),
            Err(Error::BadSignature { .. })
        ));
        // Trailing newline is a different body.
        assert!(matches!(
            whatsapp().verify(&headers, b"{\"entry\":[]}\n"),
            Err(Error::BadSignature { .. })
        ));
        // Another secret's signature.
        let foreign = Headers::from_pairs([(
            "x-hub-signature-256",
            meta_signature(&Secret::new("other"), br#"{"entry":[]}"#).unwrap(),
        )]);
        assert!(matches!(
            whatsapp().verify(&foreign, br#"{"entry":[]}"#),
            Err(Error::BadSignature { .. })
        ));
    }

    #[test]
    fn verification_passes_on_an_empty_but_signed_body_then_parsing_fails() {
        let headers = signed(b"");
        assert!(whatsapp().verify(&headers, b"").is_ok());
        assert!(matches!(whatsapp().adapt(b""), Err(Error::Payload(_))));
    }

    #[test]
    fn duplicate_keys_are_last_wins_not_a_panic() {
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[]}}]}],
                          "entry":[{"changes":[{"value":{"messages":[
                            {"id":"wamid.1","from":"49","type":"text","text":{"body":"x"}}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events.len(), 1, "the last duplicate key wins");

        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"first","id":"second","from":"49","type":"text","text":{"body":"x"}}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events[0].external_id, "second");
    }

    #[test]
    fn wrong_types_are_tolerated_field_by_field() {
        // `messages` is not an array: no events, no error.
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":"oops"}}]}]}"#;
        assert_eq!(whatsapp().adapt(payload).unwrap(), Vec::new());

        // `text` is not an object: the message survives without text.
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"wamid.1","from":"49","type":"text","text":"not-an-object"}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].text().is_none());

        // A timestamp object is not a timestamp.
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"wamid.1","from":"49","type":"text","timestamp":{},"text":{"body":"x"}}]}}]}]}"#;
        assert!(whatsapp().adapt(payload).unwrap()[0].timestamp.is_none());
    }

    #[test]
    fn unknown_message_type_is_carried_verbatim() {
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"wamid.1","from":"49","type":"button_reply_v9","button_reply_v9":{"id":"b1"}}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider_type.as_deref(), Some("button_reply_v9"));
        assert!(events[0].raw.is_some());
        assert!(events[0].text().is_none());
        assert_eq!(events[0].attachments.len(), 0);
    }

    #[test]
    fn unknown_message_types_keep_readable_text() {
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"wamid.1","from":"49","type":"com.example.poll","com.example.poll":{"body":"Which day?"}},
            {"id":"wamid.2","from":"49","type":"button_reply_v9","button_reply_v9":{"caption":"Yes"}},
            {"id":"wamid.3","from":"49","type":"future_text","text":{"body":"Hello"}},
            {"id":"wamid.4","from":"49","type":"future_body","body":"Plain body"}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].text(), Some("Which day?"));
        assert_eq!(events[1].text(), Some("Yes"));
        assert_eq!(events[2].text(), Some("Hello"));
        assert_eq!(events[3].text(), Some("Plain body"));
        for event in events {
            assert_eq!(event.attachments.len(), 0);
        }
    }

    #[test]
    fn twenty_recipients_keep_order_and_identity() {
        let messages: Vec<String> = (0..20)
            .map(|index| {
                format!(
                    r#"{{"id":"wamid.{index}","from":"49{index:09}","timestamp":"172960000{index}","type":"text","text":{{"body":"hi {index}"}}}}"#
                )
            })
            .collect();
        let payload = format!(
            r#"{{"entry":[{{"changes":[{{"value":{{"messages":[{}]}}}}]}}]}}"#,
            messages.join(",")
        );

        let events = whatsapp().adapt(payload.as_bytes()).unwrap();
        assert_eq!(events.len(), 20);
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.external_id, format!("wamid.{index}"));
            assert_eq!(event.conversation_id, format!("49{index:09}"));
            assert_eq!(event.text(), Some(format!("hi {index}").as_str()));
        }
    }

    #[test]
    fn unicode_and_nul_survive_the_full_round_trip() {
        let text = "Hallo \u{0} Welt — 😀 𝄞 مرحبا 中文";
        let message = messenger_webhook::OutboundMessage::text(
            messenger_webhook::Channel::WhatsApp,
            "4915112345678",
            text,
        );
        let payload = whatsapp().encode(&message).unwrap();
        let json = payload.json().unwrap();
        let echoed = json["text"]["body"].as_str().unwrap();
        assert_eq!(echoed, text, "no escaping damage on the way out");

        // Feeding the encoded body back through a provider-shaped envelope.
        let inbound = format!(
            r#"{{"entry":[{{"changes":[{{"value":{{"messages":[{{
                "id":"wamid.1","from":"4915112345678","type":"text",
                "text":{{"body":{}}}}}]}}}}]}}]}}"#,
            serde_json::to_string(text).unwrap()
        );
        let events = whatsapp().adapt(inbound.as_bytes()).unwrap();
        assert_eq!(events[0].text(), Some(text));
    }

    #[test]
    fn numeric_ids_are_normalised_to_strings() {
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":12345,"from":4915112345678,"type":"text","timestamp":1729600000,"text":{"body":"x"}}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events[0].external_id, "12345");
        assert_eq!(events[0].conversation_id, "4915112345678");
        assert_eq!(events[0].timestamp, Some(1_729_600_000));
    }

    #[test]
    fn entries_without_an_id_are_carried_with_a_synthetic_key() {
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"from":"49","type":"text","text":{"body":"no id"}},
            {"id":"wamid.2","from":"49","type":"text","text":{"body":"has id"}}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events.len(), 2, "an event without an id is not dropped");
        assert!(events[0].has_synthetic_id());
        assert_eq!(events[0].text(), Some("no id"));
        assert_eq!(events[1].external_id, "wamid.2");
    }

    #[test]
    fn identifiers_that_are_not_strings_or_numbers_are_synthesised() {
        // A boolean id cannot be an idempotency key, so it is replaced.
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":true,"from":"49","type":"text"}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].has_synthetic_id());

        // An empty string is not an id either.
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"","from":"49","type":"text"}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].has_synthetic_id());
    }

    #[test]
    fn field_order_does_not_matter() {
        let a = br#"{"entry":[{"changes":[{"value":{"messages":[{"id":"1","from":"2","type":"text","text":{"body":"x"}}]}}]}]}"#;
        let b = br#"{"entry":[{"changes":[{"value":{"messages":[{"text":{"body":"x"},"type":"text","from":"2","id":"1"}]}}]}]}"#;
        assert_eq!(whatsapp().adapt(a).unwrap(), whatsapp().adapt(b).unwrap());
    }

    #[test]
    fn oversized_body_is_rejected_before_any_header_is_read() {
        let over = vec![b'0'; messenger_webhook::MAX_BODY_BYTES + 1];
        let error = whatsapp().verify(&Headers::new(), &over).unwrap_err();
        assert!(matches!(error, Error::BodyTooLarge { .. }));
    }
}

#[cfg(feature = "telegram")]
mod telegram_edges {
    use super::{telegram, Error};
    use messenger_webhook::{Adapter, Channel, Headers, TelegramAdapter};

    #[test]
    fn token_header_is_compared_exactly() {
        let adapter = telegram();
        assert!(adapter
            .verify(
                &Headers::from_iter([("x-telegram-bot-api-secret-token", "bot-secret")]),
                b"{}"
            )
            .is_ok());
        for wrong in ["bot-secre", "bot-secret ", "BOT-SECRET", ""] {
            let headers = Headers::from_iter([("x-telegram-bot-api-secret-token", wrong)]);
            assert!(
                matches!(
                    adapter.verify(&headers, b"{}"),
                    Err(Error::BadSignature { .. })
                ),
                "{wrong:?} was accepted"
            );
        }
    }

    #[test]
    fn empty_secret_fails_closed() {
        let adapter = TelegramAdapter::new("");
        let headers = Headers::from_iter([("x-telegram-bot-api-secret-token", "")]);
        assert!(matches!(
            adapter.verify(&headers, b"{}"),
            Err(Error::MissingSecret { .. })
        ));
    }

    #[test]
    fn header_name_is_case_insensitive() {
        let headers = Headers::from_iter([("X-Telegram-Bot-Api-Secret-Token", "bot-secret")]);
        assert!(telegram().verify(&headers, b"{}").is_ok());
    }

    #[test]
    fn duplicate_token_headers_fail_closed() {
        let single = Headers::from_iter([("x-telegram-bot-api-secret-token", "bot-secret")]);
        assert!(telegram().verify(&single, b"{}").is_ok());

        for values in [
            ["bot-secret", "bot-secret"],
            ["bot-secret", "wrong"],
            ["wrong", "bot-secret"],
        ] {
            let headers = Headers::from_iter([
                ("x-telegram-bot-api-secret-token", values[0]),
                ("X-Telegram-Bot-Api-Secret-Token", values[1]),
            ]);
            assert!(matches!(
                telegram().verify(&headers, b"{}"),
                Err(Error::DuplicateVerificationHeader {
                    channel: Channel::Telegram,
                    header: "x-telegram-bot-api-secret-token",
                    count: 2,
                })
            ));
        }
    }

    #[test]
    fn unknown_update_kinds_yield_no_events() {
        for payload in [
            &br#"{"update_id":1,"inline_query":{"id":"x"}}"#[..],
            br#"{"update_id":1,"callback_query":{"id":"x"}}"#,
            br#"{"update_id":1,"poll":{"id":"x"}}"#,
            br#"{"update_id":1}"#,
        ] {
            assert_eq!(
                telegram().adapt(payload).unwrap(),
                Vec::new(),
                "unexpected events for {payload:?}"
            );
        }
    }

    #[test]
    fn channel_post_without_from_uses_the_chat() {
        let payload = br#"{"update_id":1,"channel_post":{"message_id":3,
            "chat":{"id":-1001,"type":"channel"},"date":1729600000,"text":"post"}}"#;
        let events = telegram().adapt(payload).unwrap();
        assert_eq!(events[0].sender_id, "-1001");
        assert_eq!(events[0].conversation_id, "-1001");
    }

    #[test]
    fn string_and_numeric_ids_both_work() {
        let numeric = br#"{"message":{"message_id":7,"chat":{"id":-1001},"from":{"id":42},"date":1,"text":"x"}}"#;
        let textual = br#"{"message":{"message_id":"7","chat":{"id":"-1001"},"from":{"id":"42"},"date":"1","text":"x"}}"#;
        let numeric_events = telegram().adapt(numeric).unwrap();
        let textual_events = telegram().adapt(textual).unwrap();
        assert_eq!(numeric_events.len(), textual_events.len());
        for (left, right) in numeric_events.iter().zip(&textual_events) {
            // `raw` differs by design (numbers vs strings); the normalised
            // fields must not.
            assert_eq!(left.external_id, right.external_id);
            assert_eq!(left.conversation_id, right.conversation_id);
            assert_eq!(left.sender_id, right.sender_id);
            assert_eq!(left.timestamp, right.timestamp);
            assert_eq!(left.text(), right.text());
        }
    }

    #[test]
    fn missing_optionals_are_none_not_errors() {
        let payload = br#"{"message":{"message_id":1,"chat":{"id":2}}}"#;
        let events = telegram().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].timestamp.is_none());
        assert!(events[0].text().is_none());
        assert_eq!(events[0].attachments.len(), 0);
        assert_eq!(events[0].sender_id, "2", "falls back to the chat");
    }

    #[test]
    fn all_media_kinds_are_recognised() {
        let payload = br#"{"message":{"message_id":1,"chat":{"id":2},"date":1,
          "document":{"file_id":"d","file_name":"a.pdf","mime_type":"application/pdf","file_size":10},
          "audio":{"file_id":"a","duration":5},
          "voice":{"file_id":"v","duration":3},
          "video":{"file_id":"m","width":10,"height":20},
          "video_note":{"file_id":"n","duration":2},
          "animation":{"file_id":"g"},
          "sticker":{"file_id":"s","emoji":"\ud83d\ude00"},
          "location":{"latitude":1.0,"longitude":2.0,"name":"here"},
          "contact":{"phone_number":"1","first_name":"F"},
          "poll":{"id":"p"}}}"#;
        let events = telegram().adapt(payload).unwrap();
        let kinds: Vec<&str> = events[0]
            .attachments
            .iter()
            .map(|attachment| attachment.kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "document", "audio", "voice", "video", "video", "video", "sticker", "location",
                "contact", "poll"
            ]
        );
    }

    #[test]
    fn numeric_media_fields_tolerate_strings_and_reject_objects() {
        // Telegram sometimes serialises numbers as strings.
        let strings = br#"{"message":{"message_id":1,"chat":{"id":2},
          "photo":[{"file_id":"p","width":"640","height":"480","file_size":"9000"}],
          "document":{"file_id":"d","file_size":"12"}}}"#;
        let events = telegram().adapt(strings).unwrap();
        assert_eq!(events[0].attachments[0].size_bytes, Some(9000));
        assert_eq!(events[0].attachments[0].width, Some(640));
        assert_eq!(events[0].attachments[1].size_bytes, Some(12));

        // An object where a number belongs is simply absent.
        let objects = br#"{"message":{"message_id":1,"chat":{"id":2},
          "document":{"file_id":"d","file_size":{}}}}"#;
        let events = telegram().adapt(objects).unwrap();
        assert_eq!(events[0].attachments[0].size_bytes, None);
    }

    #[test]
    fn photo_dimensions_stop_at_u32_before_choosing_the_largest() {
        let payload = format!(
            r#"{{"message":{{"message_id":1,"chat":{{"id":2}},"photo":[
              {{"file_id":"small","width":90,"height":90}},
              {{"file_id":"at-limit","width":{},"height":1}},
              {{"file_id":"over-limit","width":{},"height":1}}
            ]}}}}"#,
            u32::MAX,
            u64::from(u32::MAX) + 1
        );
        let events = telegram().adapt(payload.as_bytes()).unwrap();
        let attachment = &events[0].attachments[0];
        assert_eq!(attachment.id.as_deref(), Some("at-limit"));
        assert_eq!(attachment.width, Some(u32::MAX));
        assert_eq!(attachment.height, Some(1));
    }

    #[test]
    fn crafted_photo_dimensions_do_not_overflow_when_skipped() {
        let payload = format!(
            r#"{{"message":{{"message_id":1,"chat":{{"id":2}},"photo":[
              {{"file_id":"valid","width":1280,"height":1280}},
              {{"file_id":"overflow","width":{},"height":{} }}
            ]}}}}"#,
            1u64 << 63,
            1u64 << 63
        );
        let events = telegram().adapt(payload.as_bytes()).unwrap();
        let attachment = &events[0].attachments[0];
        assert_eq!(attachment.id.as_deref(), Some("valid"));
        assert_eq!(attachment.width, Some(1280));
        assert_eq!(attachment.height, Some(1280));
    }

    #[test]
    fn reaction_removal_keeps_an_empty_emoji() {
        let removed = br#"{"message_reaction":{"chat":{"id":1},"message_id":9,
            "user":{"id":2},"date":1,"old_reaction":[{"type":"emoji","emoji":"\ud83d\udc4d"}],"new_reaction":[]}}"#;
        let events = telegram().adapt(removed).unwrap();
        let reaction = events[0].reaction.as_ref().unwrap();
        assert!(reaction.removed);
        assert_eq!(reaction.emoji, "");
        assert_eq!(reaction.added_reactions, Vec::<String>::new());
        assert_eq!(reaction.removed_reactions, ["👍"]);
        assert_eq!(reaction.message_id, "9");
    }

    #[test]
    fn reaction_updates_keep_every_current_reaction() {
        let payload = br#"{"message_reaction":{"chat":{"id":1},"message_id":9,
            "user":{"id":2},"date":1,"old_reaction":[],
            "new_reaction":[{"type":"emoji","emoji":"\ud83d\udc4d"},
                {"type":"emoji","emoji":"\u2764\ufe0f"}]}}"#;
        let events = telegram().adapt(payload).unwrap();
        let reaction = events[0].reaction.as_ref().unwrap();
        assert!(!reaction.removed);
        assert_eq!(reaction.emoji, "👍");
        assert_eq!(reaction.added_reactions, ["👍", "❤️"]);
        assert_eq!(reaction.removed_reactions, Vec::<String>::new());
    }

    #[test]
    fn reaction_updates_report_the_replaced_reaction() {
        let payload = br#"{"message_reaction":{"chat":{"id":1},"message_id":9,
            "user":{"id":2},"date":1,"old_reaction":[{"type":"emoji","emoji":"\ud83d\udc4d"}],
            "new_reaction":[{"type":"emoji","emoji":"\u2764\ufe0f"}]}}"#;
        let events = telegram().adapt(payload).unwrap();
        let reaction = events[0].reaction.as_ref().unwrap();
        assert!(!reaction.removed);
        assert_eq!(reaction.emoji, "❤️");
        assert_eq!(reaction.added_reactions, ["❤️"]);
        assert_eq!(reaction.removed_reactions, ["👍"]);
    }

    #[test]
    fn unparseable_reaction_entries_are_not_removals() {
        for new_reaction in [r#"[{"type":"unknown"}]"#, r#""unknown""#, r"null"] {
            let payload = format!(
                r#"{{"message_reaction":{{"chat":{{"id":1}},"message_id":9,
                    "user":{{"id":2}},"date":1,"old_reaction":[],"new_reaction":{new_reaction}}}}}"#
            );
            let events = telegram().adapt(payload.as_bytes()).unwrap();
            let reaction = events[0].reaction.as_ref().unwrap();
            assert!(!reaction.removed, "{new_reaction}");
            assert_eq!(reaction.emoji, "");
            assert_eq!(reaction.added_reactions, Vec::<String>::new());
            assert_eq!(reaction.removed_reactions, Vec::<String>::new());
        }
    }
}

#[cfg(feature = "matrix")]
mod matrix_edges {
    use super::{matrix, Error};
    use messenger_webhook::{Adapter, Channel, Headers, MatrixAdapter};

    #[test]
    fn bearer_scheme_is_required_and_compared_exactly() {
        let adapter = matrix();
        assert!(adapter
            .verify(
                &Headers::from_iter([("authorization", "Bearer hs-token")]),
                b"{}"
            )
            .is_ok());
        assert!(adapter
            .verify(
                &Headers::from_iter([("authorization", "bearer hs-token")]),
                b"{}"
            )
            .is_ok());

        for (header, expected) in [
            ("Bearer wrong", true),
            ("Basic hs-token", false),
            ("hs-token", false),
            ("Bearer ", true),
        ] {
            let headers = Headers::from_iter([("authorization", header)]);
            let result = adapter.verify(&headers, b"{}");
            if expected {
                assert!(
                    matches!(result, Err(Error::BadSignature { .. })),
                    "{header:?} should be a signature mismatch"
                );
            } else {
                assert!(
                    matches!(result, Err(Error::MalformedVerificationHeader { .. })),
                    "{header:?} should be malformed"
                );
            }
        }
    }

    #[test]
    fn malformed_authorization_scheme_is_not_echoed() {
        let adapter = matrix();
        let scheme = "Basic\r\nx-secret";
        let header = format!("{scheme} hs-token");
        let headers = Headers::from_iter([("authorization", header.as_str())]);

        let error = adapter.verify(&headers, b"{}").unwrap_err();
        let rendered = error.to_string();
        assert!(matches!(error, Error::MalformedVerificationHeader { .. }));
        assert!(
            !rendered.contains(scheme),
            "error echoed attacker text: {rendered}"
        );
        assert!(
            !rendered.contains("x-secret"),
            "error echoed attacker text: {rendered}"
        );
        assert_eq!(
            rendered,
            "matrix verification header is malformed: authorization scheme is not Bearer"
        );
    }

    #[test]
    fn empty_token_fails_closed() {
        let adapter = MatrixAdapter::new("");
        let headers = Headers::from_iter([("authorization", "Bearer ")]);
        assert!(matches!(
            adapter.verify(&headers, b"{}"),
            Err(Error::MissingSecret { .. })
        ));
    }

    #[test]
    fn duplicate_authorization_headers_fail_closed() {
        let single = Headers::from_iter([("authorization", "Bearer hs-token")]);
        assert!(matrix().verify(&single, b"{}").is_ok());

        for values in [
            ["Bearer hs-token", "Bearer hs-token"],
            ["Bearer hs-token", "Bearer wrong"],
            ["Bearer wrong", "Bearer hs-token"],
        ] {
            let headers =
                Headers::from_iter([("authorization", values[0]), ("Authorization", values[1])]);
            assert!(matches!(
                matrix().verify(&headers, b"{}"),
                Err(Error::DuplicateVerificationHeader {
                    channel: Channel::Matrix,
                    header: "authorization",
                    count: 2,
                })
            ));
        }
    }

    #[test]
    fn encrypted_and_unknown_events_are_carried_not_rejected() {
        let payload = br#"{"events":[
          {"type":"m.room.encrypted","event_id":"$1","room_id":"!r:s","sender":"@a:s",
           "content":{"algorithm":"m.megolm.v1.aes-sha2","ciphertext":"AAAA"}},
          {"type":"m.typing","event_id":"$2","room_id":"!r:s","sender":"@a:s","content":{"user_ids":["@a:s"]}},
          {"type":"m.room.member","event_id":"$3","room_id":"!r:s","sender":"@a:s","content":{"membership":"join"}}]}"#;
        let events = matrix().adapt(payload).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].provider_type.as_deref(), Some("m.room.encrypted"));
        assert!(events[0].raw.is_some());
    }

    #[test]
    fn receipts_become_status_events() {
        let payload = br#"{"type":"m.receipt","event_id":"$r","room_id":"!r:s","sender":"@a:s",
          "content":{"$msg1":{"m.read":{"@b:s":{"ts":1729600000000}}},
                     "$msg2":{"m.read":{"@c:s":{"ts":1729600001000}}}}}"#;
        let events = matrix().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].is_status_update());
        assert_eq!(events[0].external_id, "$msg1");
    }

    #[test]
    fn edits_replace_text_and_keep_the_target() {
        let payload =
            br#"{"type":"m.room.message","event_id":"$2","room_id":"!r:s","sender":"@a:s",
          "content":{"msgtype":"m.text","body":"* fixed",
                     "m.new_content":{"msgtype":"m.text","body":"fixed"},
                     "m.relates_to":{"rel_type":"m.replace","event_id":"$1"}}}"#;
        let events = matrix().adapt(payload).unwrap();
        assert_eq!(events[0].edit_of.as_deref(), Some("$1"));
        assert_eq!(events[0].text(), Some("fixed"));
    }

    #[test]
    fn threads_and_replies_are_distinct() {
        let payload = br#"{"events":[
          {"type":"m.room.message","event_id":"$2","room_id":"!r:s","sender":"@a:s",
           "content":{"msgtype":"m.text","body":"threaded",
                      "m.relates_to":{"rel_type":"m.thread","event_id":"$root",
                                      "m.in_reply_to":{"event_id":"$1"}}}}]}"#;
        let events = matrix().adapt(payload).unwrap();
        assert_eq!(events[0].thread_id.as_deref(), Some("$root"));
        assert_eq!(events[0].reply_to.as_deref(), Some("$1"));
    }

    #[test]
    fn twenty_events_keep_order() {
        let events: Vec<String> = (0..20)
            .map(|index| {
                format!(
                    r#"{{"type":"m.room.message","event_id":"${index}","room_id":"!r:s","sender":"@a:s","origin_server_ts":{},"content":{{"msgtype":"m.text","body":"hi {index}"}}}}"#,
                    index * 1000
                )
            })
            .collect();
        let payload = format!(r#"{{"events":[{}]}}"#, events.join(","));
        let normalized = matrix().adapt(payload.as_bytes()).unwrap();
        assert_eq!(normalized.len(), 20);
        for (index, event) in normalized.iter().enumerate() {
            assert_eq!(event.external_id, format!("${index}"));
            assert_eq!(event.text(), Some(format!("hi {index}").as_str()));
        }
    }

    #[test]
    fn media_without_info_is_still_normalised() {
        let payload =
            br#"{"type":"m.room.message","event_id":"$1","room_id":"!r:s","sender":"@a:s",
          "content":{"msgtype":"m.file","body":"notes.txt","url":"mxc://s/notes"}}"#;
        let events = matrix().adapt(payload).unwrap();
        let attachment = &events[0].attachments[0];
        assert_eq!(attachment.kind.as_str(), "document");
        assert_eq!(attachment.filename.as_deref(), Some("notes.txt"));
        assert_eq!(attachment.mime_type, None);
        assert_eq!(attachment.size_bytes, None);
    }

    #[test]
    fn events_without_an_id_are_carried() {
        let payload = br#"{"events":[
          {"type":"m.room.message","room_id":"!r:s","sender":"@a:s","content":{"msgtype":"m.text","body":"no id"}},
          {"type":"m.room.message","event_id":"$1","room_id":"!r:s","sender":"@a:s","content":{"msgtype":"m.text","body":"has id"}}]}"#;
        let events = matrix().adapt(payload).unwrap();
        assert_eq!(events.len(), 2, "an event without an id is not dropped");
        assert!(events[0].has_synthetic_id());
        assert_eq!(events[0].text(), Some("no id"));
        assert_eq!(events[1].external_id, "$1");
    }

    #[test]
    fn malformed_transaction_envelopes_are_rejected() {
        assert_eq!(matrix().adapt(br#"{"events":[]}"#).unwrap().len(), 0);

        for payload in [
            &br#"{"events":{"chunk":[]}}"#[..],
            br#"{"events":"not an array"}"#,
            br#"{"events":null}"#,
            br#"{"type":"m.room.message","events":[]}"#,
        ] {
            assert!(
                matches!(matrix().adapt(payload), Err(Error::Payload(_))),
                "unexpectedly accepted {payload:?}"
            );
        }
    }
}
