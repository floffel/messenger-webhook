//! Both adapters against real-shaped payloads.

use messenger_webhook::{Adapter, Channel};

#[cfg(feature = "whatsapp")]
#[test]
fn whatsapp_text_message_is_normalised() {
    let payload = br#"{"object":"whatsapp_business_account","entry":[{"id":"1","changes":[{"field":"messages",
      "value":{"metadata":{"display_phone_number":"1","phone_number_id":"2"},
      "messages":[{"id":"wamid.1","from":"4915112345678","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;

    let messages = messenger_webhook::whatsapp::adapter()
        .adapt(payload)
        .unwrap();
    assert_eq!(messages.len(), 1);
    let m = &messages[0];
    assert_eq!(m.channel, Channel::WhatsApp);
    assert_eq!(m.external_id, "wamid.1");
    assert_eq!(m.sender_id, "4915112345678");
    assert_eq!(m.conversation_id, "4915112345678");
    assert_eq!(m.text.as_deref(), Some("Hallo"));
    assert_eq!(m.timestamp, Some(1_729_600_000));
}

#[cfg(feature = "whatsapp")]
#[test]
fn whatsapp_status_only_payload_yields_no_messages() {
    let payload =
        br#"{"object":"whatsapp_business_account","entry":[{"changes":[{"field":"messages",
      "value":{"statuses":[{"id":"x","status":"read"}]}}]}]}"#;
    let messages = messenger_webhook::whatsapp::adapter().adapt(payload).unwrap();
    assert_eq!(messages, Vec::new(), "status-only payload must yield no messages");
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_message_is_normalised() {
    let payload = br#"{"update_id":1,"message":{"message_id":7,"from":{"id":42,"first_name":"F"},
      "chat":{"id":-1001,"type":"supergroup"},"date":1729600000,"text":"Hallo"}}"#;
    let messages = messenger_webhook::telegram::adapter()
        .adapt(payload)
        .unwrap();
    assert_eq!(messages.len(), 1);
    let m = &messages[0];
    assert_eq!(m.channel, Channel::Telegram);
    assert_eq!(m.conversation_id, "-1001");
    assert_eq!(m.sender_id, "42");
    assert_eq!(m.timestamp, Some(1_729_600_000));
}

#[cfg(feature = "telegram")]
#[test]
fn telegram_update_without_message_yields_nothing() {
    let payload = br#"{"update_id":2}"#;
    let messages = messenger_webhook::telegram::adapter().adapt(payload).unwrap();
    assert_eq!(messages, Vec::new(), "update without message must yield nothing");
}

#[cfg(feature = "whatsapp")]
#[test]
fn malformed_payload_is_an_error() {
    assert!(messenger_webhook::whatsapp::adapter()
        .adapt(b"not json")
        .is_err());
}
