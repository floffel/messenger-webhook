# messenger-webhook

Normalise inbound messenger webhooks into **one** message type.

Every provider reports the same facts differently. This crate maps them onto
`NormalizedMessage { channel, external_id, conversation_id, sender_id, text, timestamp }`
so the consumer has one routing/persistence path and one place for provider
quirks.

- `whatsapp` — WhatsApp Cloud API payloads (default)
- `telegram` — Telegram Bot API updates (default)
- `matrix` — reserved

```rust
let messages = messenger_webhook::whatsapp::adapter().adapt(body)?;
for m in messages {
    store.upsert(&m.external_id, &m.conversation_id, m.text.as_deref()).await?;
}
```

`external_id` is the provider message id and therefore the right idempotency key.

Licensed under Apache-2.0 OR MIT.
