//! Minimal end-to-end example: verify one inbound `WhatsApp` webhook body, normalise it, and
//! encode a reply — no HTTP server, no framework.
//!
//! ```sh
//! cargo run --example minimal --features whatsapp
//! ```

use messenger_webhook::{
    signature::meta_signature, Adapter, Channel, Headers, NormalizedMessage, Secret,
    WhatsAppAdapter,
};

/// The app secret Meta shows in the app dashboard. A real service reads it from
/// the environment or a secret store — never from source.
const APP_SECRET: &str = "app-secret-from-the-meta-dashboard";

/// One inbound text message, shaped like Meta's webhook reference.
const BODY: &[u8] = br#"{"object":"whatsapp_business_account","entry":[{"id":"1","changes":[{"field":"messages","value":{"metadata":{"display_phone_number":"1","phone_number_id":"2"},"messages":[{"id":"wamid.1","from":"4915112345678","timestamp":"1729600000","type":"text","text":{"body":"Hallo"}}]}}]}]}"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = WhatsAppAdapter::new(APP_SECRET);

    // In a real service both values come off the HTTP request: the raw body bytes
    // and the `X-Hub-Signature-256` header. Verify before anything parses the body.
    let headers = Headers::from_pairs([(
        "x-hub-signature-256",
        meta_signature(&Secret::new(APP_SECRET), BODY)?,
    )]);
    adapter.verify(&headers, BODY)?;

    for message in adapter.adapt(BODY)? {
        println!(
            "{} event {} from {} in conversation {}: {:?}",
            message.channel,
            message.external_id,
            message.sender_id,
            message.conversation_id,
            message.text,
        );
    }

    // Outbound: the same adapter renders the provider's own request body, so the
    // caller only has to POST it (with its token) — the wire shape stays here.
    let reply = NormalizedMessage::text(Channel::WhatsApp, "4915112345678", "Hallo zurück");
    let payload = adapter.encode(&reply)?;
    println!(
        "{} {} ({} bytes)",
        payload.method(),
        payload.content_type(),
        payload.body().len(),
    );
    println!("{}", String::from_utf8_lossy(payload.body()));

    Ok(())
}
