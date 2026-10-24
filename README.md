# messenger-webhook

A transport-neutral layer between messenger payloads and an application.

Every provider reports the same facts differently. This crate maps **inbound**
`WhatsApp` Cloud API, Telegram Bot API and Matrix events onto one
`NormalizedMessage`, and **encodes** that message back into a provider payload —
with signature verification as part of the adapter contract, not an optional
extra. Nothing in the crate performs I/O: you own the HTTP server, the retries
and the rate limits.

| provider | inbound | verification | outbound |
|----------|---------|--------------|----------|
| `whatsapp` | `entry[].changes[].value.messages` / `statuses` | HMAC-SHA256 `X-Hub-Signature-256` | text, media link |
| `telegram` | `message`, edits, channel posts, `message_reaction` | `X-Telegram-Bot-Api-Secret-Token` | `sendMessage` / send-media |
| `matrix` | `m.room.message`, `m.reaction`, `m.receipt`, unknown types | `Authorization: Bearer <hs_token>` | `m.room.message` content |

All three features are on by default; each module is independently gated.

Each gated item carries a `feature` badge on docs.rs, which builds the crate
with `--all-features`. The crate uses `std` only — there is no `no_std` story.

**MSRV: Rust 1.89.** A dedicated CI job builds all features, no features and
the test targets on 1.89 with `--locked`, so a dependency bump that raises the
floor fails the pipeline instead of a user's build. The floor moves only in a
minor release, and only when a dependency forces it.

## Quick start

```rust
use messenger_webhook::{Adapter, Headers, WhatsAppAdapter};

let app_secret = std::env::var("WA_APP_SECRET")?;
let adapter = WhatsAppAdapter::new(&app_secret);

// 1. Verify the raw body — before it is parsed.
let headers = Headers::from_pairs([("x-hub-signature-256", signature_from_request)]);
adapter.verify(&headers, &body)?;

// 2. Normalise.
for message in adapter.adapt(&body)? {
    store.upsert(&message.external_id, &message.conversation_id, message.text.as_deref()).await?;
}
```

`external_id` is the provider's event id and therefore the right idempotency
key. `raw` keeps the provider's original object, so anything this crate does not
model is still reachable.

### Registry

Instead of holding adapters yourself:

```rust
use messenger_webhook::{Channel, Headers, Registry, TelegramAdapter, WhatsAppAdapter};

let mut registry = Registry::new();
registry
    .register(WhatsAppAdapter::new("app-secret"))
    .register(TelegramAdapter::new("bot-secret"));

// Verify and normalise in one step; an unregistered channel is a typed error.
let events = registry.handle(&Channel::WhatsApp, &headers, &body)?;
```

### Outbound

```rust
use messenger_webhook::{Adapter, Channel, NormalizedMessage, TelegramAdapter};

let adapter = TelegramAdapter::new("bot-secret");
let message = NormalizedMessage::text(Channel::Telegram, "-1001", "Hallo");
let payload = adapter.encode(&message)?;

assert_eq!(payload.method(), "POST");
let body = payload.json()?;              // {"chat_id":"-1001","text":"Hallo"}
```

Media, replies, threads and edits are encoded too (`attachments`, `reply_to`,
`thread_id`, `edit_of`). Provider limits are enforced before a payload is
produced:

| limit | value | error |
|-------|-------|-------|
| `WhatsApp` text | 4096 **bytes** | `Error::MessageTooLong` |
| `WhatsApp` caption | 1024 **bytes** | `Error::MessageTooLong` |
| Telegram text | 4096 **UTF-16 units** | `Error::MessageTooLong` |
| Telegram caption | 1024 **UTF-16 units** | `Error::MessageTooLong` |
| Matrix | none documented | — |

Messages the provider cannot express (empty recipient, no text and no media,
unsupported media kind, non-numeric Telegram ids, non-`mxc://` Matrix urls,
edits) fail with `Error::InvalidMessage` instead of being sent as something the
provider would reject.

## Normalised shape

`NormalizedMessage` carries the provider id, conversation, sender, text and
timestamp plus:

- `attachments` — `AttachmentKind` (image, video, audio, voice, document,
  sticker, location, contact, other), mime type, size, dimensions, duration,
  filename, caption
- `reply_to` / `thread_id` — reply and thread references
- `edit_of` — set when the event replaces an earlier message
- `reaction` — `Reaction { message_id, emoji, removed }`
- `status` — `DeliveryStatus` (sent, delivered, read, failed, deleted, unknown)
- `provider_type` — the provider's own event name, verbatim
- `raw` — the provider object, verbatim

An event that has none of these is still an event: unknown message types are
carried rather than rejected.

## Security model

- **Verification is not optional.** Adapters are constructed with their secret;
  `verify` is on the `Adapter` trait and fails closed on an empty secret, a
  missing header, a malformed header or a body over `MAX_BODY_BYTES` (1 MiB).
- **Constant time.** HMAC digests and provider tokens are compared with
  `subtle`; nothing short-circuits. `Secret`'s `Debug` is redacted, so a
  `{:?}` of an adapter never prints a credential.
- **Size before work.** The body ceiling is checked before hashing and before
  JSON parsing.
- **Strict authentication, permissive parsing.** Header shapes are validated
  exactly; payloads tolerate unknown fields, unknown types, numeric-or-string
  ids, missing optionals and reordered keys.
- `#![forbid(unsafe_code)]`, `#![deny(missing_docs)]`, no `unwrap`/`panic!` in
  library code, `#[non_exhaustive]` error enum, every error terminal (so
  `is_retryable()` is honestly `false`).

## Tests

| suite | what it proves |
|-------|----------------|
| doctests on every public item | the documented examples really run |
| `tests/properties.rs` | arbitrary JSON (any depth/types) and arbitrary bytes never panic; encode → provider echo → normalise round-trips on all three channels; unknown fields survive; limits hold exactly at N and N+1 |
| `tests/edge_cases.rs` | empty body, 1 MiB boundary, missing/ malformed signature headers, signature bound to the exact body, duplicate keys, unknown types, 20 recipients, unicode/NUL, numeric-vs-string ids, wrong JSON types, deep nesting |
| `tests/media_and_receipts.rs` | every media kind, reaction and receipt shape in both directions |
| `tests/concurrency.rs` | 32 threads over one `Registry`, 32 threads over a borrowed `&dyn Adapter`, forged signatures under load |
| `tests/fixtures.rs` | the fuzz seeds, compiled in, normalised through every adapter |

**Coverage: 99.4 % of lines** (`cargo llvm-cov --all-features`, 1292 lines;
the remainder are unreachable `None` arms of optional JSON fields). CI fails
below 95 %. Fuzz target: `normalize` (`cargo fuzz run normalize`), seeded with
`fuzz/corpus/normalize/` — the same files the tests compile in.

## Deliberate exclusions

Each of these is a decision, not an omission:

- **No network.** No sending, media download, token refresh or long polling.
  The crate produces payloads and reads webhook bodies.
- **No rich text.** Telegram entities, `WhatsApp` interactive payloads and
  Matrix formatted bodies (`format`, `formatted_body`) are not parsed; the plain
  text is normalised and the raw object is preserved.
- **No Matrix E2EE.** `m.room.encrypted` events are carried verbatim
  (`provider_type`, `raw`), never decrypted.
- **No edits/reactions outbound.** Telegram edits require `editMessageText` on
  the original message id and `WhatsApp` reactions the Cloud API message id;
  those are separate endpoints, so `encode` returns `Error::InvalidMessage`
  rather than silently sending a duplicate message.
- **No SHA-1 signatures.** Meta's deprecated `X-Hub-Signature` header is
  rejected; only `X-Hub-Signature-256` is accepted.
- **No per-adapter body limits.** One crate-wide `MAX_BODY_BYTES` (1 MiB) keeps
  the gate simple; providers are far below it.
- **Events without an id are skipped** (documented on `Adapter::adapt`): they
  cannot serve as an idempotency key.

## License

Licensed under Apache-2.0 OR MIT.
