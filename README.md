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
the test targets on 1.89 with a fresh dependency resolution, so a bump that raises the
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

## Examples

Two runnable examples ship with the crate — the minimal one has no dependencies
beyond the crate, the Axum one is the shape a real webhook endpoint takes.

```sh
cargo run --example minimal --features whatsapp
```

Verifies a webhook body against a signature it computes itself, normalises the
message and prints the outbound payload the provider would expect. No server,
no network.

```sh
WA_APP_SECRET=dev-only-secret cargo run --example axum_handler
```

A real Axum handler on `127.0.0.1:3000`: takes the **raw** body as `Bytes`
(before any deserialisation, so the signature still covers it), maps the
framework's header map onto `Headers`, answers `401` for a missing or wrong
signature and `200` for a verified event. CI runs the first example and
compiles the second, so neither can rot into something that only looks runnable.

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
| `tests/changelog.rs` | the newest changelog heading equals `Cargo.toml`, sections are newest-first, no duplicates and none empty — a version bump without an entry fails the build |

**Coverage: 98.9 % of lines** (`cargo llvm-cov --all-features`, 1292 lines,
14 missed). Half of the misses are the `is_empty()` helpers on `Headers` and
`Registry`, which no test calls directly; the rest are single optional-field and
error arms inside `json`, `lib`, `matrix` and `telegram`. CI fails below 95 %.
Fuzz target: `normalize` (`cargo fuzz run normalize`), seeded with
`fuzz/corpus/normalize/` — the same files the tests compile in.

## Versioning and semver policy

The crate is pre-1.0, so cargo's compatibility rule applies to the *middle*
number: `0.2.x` are compatible patches, `0.2` → `0.3` may break, and a break is
never published as `0.2.1`. Every release adds a section to
[CHANGELOG.md](CHANGELOG.md), and `tests/changelog.rs` fails the build when the
newest changelog heading and `Cargo.toml` disagree — a release cannot ship
without its entry.

**Breaking (needs a new minor, marked `Breaking` + `!` in the commit):**

- any public type, trait, method, function or module path this README or the
  docs.rs page names — including moving an item between modules
- `Adapter`'s methods and their contracts (`verify` before `adapt`)
- renaming or removing a feature (`whatsapp`, `telegram`, `matrix`); adding one
  is additive
- the encoded payload for a message the crate claims to support: HTTP method,
  content type or body field names
- a raised MSRV — the floor is Rust 1.89 and moves only in a minor release

**Additive (a patch or a minor, never breaks a build):**

- a new variant on `Error`, `AttachmentKind` or `DeliveryStatus` — all three are
  `#[non_exhaustive]`, so `match` arms need a `_` anyway
- new `Channel::Other(..)` values, new inherent methods, new trait methods with
  a default body
- a new optional field on a type that is `#[non_exhaustive]`

**Breaking today, because these types are exhaustive on purpose:** adding a
field to `NormalizedMessage`, `Attachment` or `Reaction`, or a built-in variant
to `Channel`. The three structs are built with field literals by callers, so a
new field is source-breaking, and a closed `Channel` breaks exhaustive matches.
`Channel::Other` exists so an unmodelled messenger never forces a variant; a
genuine new field is announced as breaking and lands in a minor bump.

**Deliberately not covered by semver:** the wording of `Error`'s `Display`
output, and the content of `provider_type` and `raw`, which are the provider's
strings and objects passed through verbatim.

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
