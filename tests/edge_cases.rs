//! The just-valid / just-invalid matrix for every public entry point:
//! empty, oversized, missing header, wrong secret shape, duplicate keys,
//! unknown types, 20 recipients, unicode, NUL and wrong JSON types.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code, unused_imports, unused_mut, unused_variables)]

use messenger_webhook::{Adapter, Error, Headers, MAX_BODY_BYTES};

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
    use messenger_webhook::{signature::meta_signature, Adapter, Secret, WhatsAppAdapter};

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
        assert!(events[0].text.is_none());

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
        assert_eq!(events[0].provider_type.as_deref(), Some("button_reply_v9"));
        assert!(events[0].raw.is_some());
        assert!(events[0].text.is_none());
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
            assert_eq!(event.text.as_deref(), Some(format!("hi {index}").as_str()));
        }
    }

    #[test]
    fn unicode_and_nul_survive_the_full_round_trip() {
        let text = "Hallo \u{0} Welt — 😀 𝄞 مرحبا 中文";
        let message = messenger_webhook::NormalizedMessage::text(
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
        assert_eq!(events[0].text.as_deref(), Some(text));
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
    fn entries_without_an_id_are_skipped_not_fatal() {
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"from":"49","type":"text","text":{"body":"no id"}},
            {"id":"wamid.2","from":"49","type":"text","text":{"body":"has id"}}]}}]}]}"#;
        let events = whatsapp().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].external_id, "wamid.2");
    }

    #[test]
    fn identifiers_that_are_not_strings_or_numbers_are_ignored() {
        // A boolean id cannot be an idempotency key.
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":true,"from":"49","type":"text"}]}}]}]}"#;
        assert_eq!(whatsapp().adapt(payload).unwrap().len(), 0);

        // An empty string is not an id either.
        let payload = br#"{"entry":[{"changes":[{"value":{"messages":[
            {"id":"","from":"49","type":"text"}]}}]}]}"#;
        assert_eq!(whatsapp().adapt(payload).unwrap().len(), 0);
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
    use messenger_webhook::{Adapter, Headers, TelegramAdapter};

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
            assert_eq!(left.text, right.text);
        }
    }

    #[test]
    fn missing_optionals_are_none_not_errors() {
        let payload = br#"{"message":{"message_id":1,"chat":{"id":2}}}"#;
        let events = telegram().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].timestamp.is_none());
        assert!(events[0].text.is_none());
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
    fn reaction_removal_keeps_an_empty_emoji() {
        let removed = br#"{"message_reaction":{"chat":{"id":1},"message_id":9,
            "user":{"id":2},"date":1,"old_reaction":[{"type":"emoji","emoji":"\ud83d\udc4d"}],"new_reaction":[]}}"#;
        let events = telegram().adapt(removed).unwrap();
        let reaction = events[0].reaction.as_ref().unwrap();
        assert!(reaction.removed);
        assert_eq!(reaction.emoji, "");
        assert_eq!(reaction.message_id, "9");
    }
}

#[cfg(feature = "matrix")]
mod matrix_edges {
    use super::{matrix, Error};
    use messenger_webhook::{Adapter, Headers, MatrixAdapter};

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
    fn empty_token_fails_closed() {
        let adapter = MatrixAdapter::new("");
        let headers = Headers::from_iter([("authorization", "Bearer ")]);
        assert!(matches!(
            adapter.verify(&headers, b"{}"),
            Err(Error::MissingSecret { .. })
        ));
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
        assert_eq!(events[0].text.as_deref(), Some("fixed"));
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
            assert_eq!(event.text.as_deref(), Some(format!("hi {index}").as_str()));
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
    fn events_without_an_id_are_skipped() {
        let payload = br#"{"events":[
          {"type":"m.room.message","room_id":"!r:s","sender":"@a:s","content":{"msgtype":"m.text","body":"no id"}},
          {"type":"m.room.message","event_id":"$1","room_id":"!r:s","sender":"@a:s","content":{"msgtype":"m.text","body":"has id"}}]}"#;
        let events = matrix().adapt(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].external_id, "$1");
    }
}
