# messenger-webhook

A transport-neutral layer between messenger payloads and an application.

Every provider reports the same facts differently. This crate maps **inbound**
`WhatsApp` Cloud API, Telegram Bot API and Matrix events onto one
`NormalizedMessage`, and **encodes** that message back into a provider payload —
with signature verification as part of the adapter contract, not an optional
extra.

The default build performs **no I/O**: you own the HTTP server, the retries and
the rate limits. The one exception is opt-in and default-off — the `matrix-out`
feature adds a typed Matrix *outbound client* (send, react, redact, typing,
receipts, media upload) behind a `Transport` trait, because a webhook parser and
a homeserver client are different jobs and only one of them needs an HTTP stack.

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

## Matrix outbound client (`matrix-out`)

Enable the feature, hand it a transport, keep the access token out of the logs:

```rust
use std::sync::Arc;
use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, ReqwestTransport};

let client = MatrixClient::new(
    Arc::new(ReqwestTransport::new()?),
    ClientConfig::new("https://matrix.example.org")?.with_user_id("@bot:example.org"),
    std::env::var("MATRIX_ACCESS_TOKEN")?,
)?;

let outcome = client.send_text("!room:example.org", "Hallo").await?;
println!("{} (txn {}, {} attempt(s))", outcome.event_id, outcome.txn_id, outcome.attempts);
```

The client never opens a socket itself. It builds a request, hands it to a
`Transport`, and interprets the response — so every protocol rule below is
tested without a homeserver, and `StubTransport` + `RecordingSleeper` are public
so your tests can do the same.

| leg | endpoint | idempotency |
|-----|----------|-------------|
| `send_text` / `send_event` | `PUT /rooms/{id}/send/{type}/{txn}` | transaction id |
| `send_reaction` | `m.reaction` with `m.annotation` | transaction id |
| `redact` | `PUT /rooms/{id}/redact/{event}/{txn}` | transaction id |
| `send_typing` / `stop_typing` | `PUT /rooms/{id}/typing/{user}` | state-setting |
| `send_read_receipt` | `POST /rooms/{id}/receipt/m.read/{event}` | state-setting |
| `upload_media` | `POST /_matrix/media/v3/upload` | **none** |
| `send_media` | upload, then `m.image`/`m.video`/`m.audio`/`m.file` | upload: none, send: transaction id |
| `whoami` | `GET /account/whoami` | read |

### Delivery contract

| outcome | what the client does | what you see |
|---------|---------------------|--------------|
| `2xx` | done | `SendOutcome { event_id, txn_id, attempts }` |
| `429` with `retry_after_ms` | waits exactly that long (bounded by `RetryPolicy::max_rate_limit_wait`), retries | more `attempts` |
| `429` without it | exponential backoff (`500 ms`, doubling, capped) | more `attempts` |
| `5xx` on a transaction-keyed send | retries **with the same txn id** — the homeserver de-duplicates | `Ambiguous { txn_id }` once the attempts are spent |
| `5xx` / timeout on a **media upload** | **never retried** | `Ambiguous { txn_id: None }` |
| connect/DNS/TLS failure | retried even for an upload — nothing was written | `Transport` (retryable) |
| other `4xx` | not retried | `Rejected { status, errcode, message }` |
| `2xx` with an unreadable body | not retried | `Ambiguous` for a write, `Decode` for a read |

`OutboundError::is_retryable()` is true only when nothing was applied;
`is_ambiguous()` is true when the event *may* be in the room. An ambiguous write
carries the transaction id, and repeating that send with it
(`send_text_with_txn`) is the documented recovery: one event, not two.

Sends into one room are serialised (one request in flight, any number across
rooms) so a bot's two-part reply cannot arrive out of order; turn it off with
`ClientConfig::with_per_room_ordering(false)`. Limits are checked before a
request exists: content against the 65 536-byte event ceiling, media against the
configured `max_upload_bytes` (read the homeserver's real value from
`m.upload.size` in `/capabilities`). The wire shapes and their sources are
quoted in [docs/contracts.md](docs/contracts.md).

### How this crate absorbed `channel-core` and `matrix-out`

Both were monorepo crates in `repr/backend/aws/crates`, and the question was
whether they belong here or in a second published crate. Evidence, then the
decision:

| monorepo item | verdict here |
|---------------|--------------|
| `channel_core::ChannelKind` (`WhatsApp`, `Telegram`, `Matrix`, `Email`, `Sms`, `Call`, `Letter`) | **Covered, minus the non-messengers.** `Channel` is the same idea with `Other(..)` for anything else; `Email`/`Sms`/`Call`/`Letter` are not messengers and stay in the monorepo, which is why `channel-core` is still a dependency of four other crates there. |
| `ChannelCapabilities` (typing / reactions / read + delivery receipts / templates) | **Not merged, deliberately.** A capability matrix is a *product* decision (which channel may show what), not a wire fact, and the published crate has no channel that needs gating. `channels.md`-style truth belongs to the caller. |
| `ChannelClient` trait (`send_text`, `indicate_typing`, `send_reaction`, defaulting to `Unsupported`) | **Covered and dropped.** `NormalizedMessage` + `Adapter::encode` is the outbound surface; the trait's default bodies existed to make an unsupported call fail loudly, which `encode` already does with `Error::InvalidMessage`. A second, `()`-returning send path was dead surface. |
| `matrix_out::MatrixClient` | **Merged, hardened.** Same calls, but typed: `TxnId` instead of `Uuid::new_v4()` per attempt, `OutboundError` instead of `String`, limits and response-size checks, per-room ordering, and an injectable transport. |
| `matrix_out` doc-comment claim that Matrix has "no documented body limit" | **Wrong, corrected.** The spec caps a complete event at 65 536 bytes; the client enforces what it can and says why ([docs/contracts.md](docs/contracts.md)). |
| Media upload | **New.** `matrix-out` never uploaded anything; the client does, with the non-idempotent delivery rules the upload endpoint demands. |

So: one crate, one release. `matrix-out` lives here behind a default-off
feature, because an outbound client is additive surface on a crate that already
owns the Matrix wire format — a second crate would publish the same provider
twice and force consumers to keep two versions in step.

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
| `tests/synthetic_ids.rs` | an event the provider sent without an id is carried with a stable synthesised key: same body, same id across replays; different body, different id; per adapter |
| `tests/media_and_receipts.rs` | every media kind, reaction and receipt shape in both directions |
| `tests/concurrency.rs` | 32 threads over one `Registry`, 32 threads over a borrowed `&dyn Adapter`, forged signatures under load |
| `tests/fixtures.rs` | the fuzz seeds, compiled in, normalised through every adapter |
| `tests/matrix_out.rs` | every outbound leg: URL, headers, body, txn reuse across retries, 429/5xx/4xx/timeout classification, ambiguous-vs-retryable, media ordering |
| `tests/matrix_out_edge.rs` | the boundary matrix: empty and over-long ids, txn charset, control characters, media at N/N+1, `429` without a hint, statuses 100–999, undecodable bodies |
| `tests/matrix_out_properties.rs` | proptest: txn-id grammar and uniqueness, total retry classification, "no non-idempotent request retried after 5xx", backoff bounds, `mxc://` grammar, every attempt of one send sharing one txn id |
| `tests/matrix_out_concurrency.rs` | 32 tasks over one client: unique txn ids, one in-flight send per room, two rooms in parallel, retries not holding the gate |
| `tests/matrix_out_http.rs` | the `reqwest` transport against a loopback server: token header, JSON body, media bytes, `5xx`, closed port (connect) and timeout (ambiguous) |
| `tests/changelog.rs` | the newest changelog heading equals `Cargo.toml`, sections are newest-first, no duplicates and none empty — a version bump without an entry fails the build |

**Coverage: 98.4 % of lines** (`cargo llvm-cov --all-features`, 3690 lines,
58 missed; `src/id.rs` and `model.rs` at 100 %). The outbound module alone
is ≥94 % per file and 100 % in `ordering.rs`; the misses are `reqwest`'s own error arms,
`serde_json` failure paths and the `is_empty()` helpers no test calls directly.
The gate is `--fail-under-lines 95`, so a regression fails CI rather than
quietly eroding the number. Fuzz target: `normalize`
(`cargo fuzz run normalize`), seeded with `fuzz/corpus/normalize/`; the outbound
validators are covered by the property suite instead, because the response
decoders are private and `StubTransport` already feeds them arbitrary bodies.

## Versioning and semver policy

The crate is pre-1.0, so cargo's compatibility rule applies to the *middle*
number: `0.3.x` are compatible patches, `0.3` → `0.4` may break, and a break is
never published as `0.3.1`. Every release adds a section to
[CHANGELOG.md](CHANGELOG.md), and `tests/changelog.rs` fails the build when the
newest changelog heading and `Cargo.toml` disagree — a release cannot ship
without its entry.

**Breaking (needs a new minor, marked `Breaking` + `!` in the commit):**

- any public type, trait, method, function or module path this README or the
  docs.rs page names — including moving an item between modules
- `Adapter`'s methods and their contracts (`verify` before `adapt`)
- renaming or removing a feature (`whatsapp`, `telegram`, `matrix`,
  `matrix-out`); adding one is additive
- the encoded payload for a message the crate claims to support: HTTP method,
  content type or body field names
- a raised MSRV — the floor is Rust 1.89 and moves only in a minor release

**Additive (a patch or a minor, never breaks a build):**

- a new variant on `Error`, `Channel`, `AttachmentKind` or `DeliveryStatus` — all
  four are `#[non_exhaustive]`, so `match` arms need a `_` anyway
- new `Channel::Other(..)` values, new inherent methods, new trait methods with
  a default body
- a new optional field on a type that is `#[non_exhaustive]`

**Breaking today, because these types are exhaustive on purpose:** adding a
field to `NormalizedMessage`, `Attachment` or `Reaction`. The three structs are
built with field literals by callers, so a new field is source-breaking; it is
announced as breaking and lands in a minor bump. `Channel::Other` exists so an
unmodelled messenger never forces a variant.

**The surface is frozen, not remembered.** `tests/api_freeze.rs` records every
public item of `src/` in `tests/api_surface.snapshot` and fails when one is
added, renamed or removed; it also enforces `#[non_exhaustive]` on every public
enum and that no private module leaks through a `pub use`. A deliberate change
regenerates the snapshot in the same commit:
`UPDATE_API_SURFACE=1 cargo test --all-features --test api_freeze`.

**Deliberately not covered by semver:** the wording of `Error`'s `Display`
output, and the content of `provider_type` and `raw`, which are the provider's
strings and objects passed through verbatim.

## Deliberate exclusions

Each of these is a decision, not an omission:

- **No network in the default build.** Without `matrix-out` the crate produces
  payloads and reads webhook bodies; with it, the only I/O is the Matrix
  Client-Server legs listed above. No media download, no token refresh, no sync
  or long polling, no E2EE, no room administration (create/join/invite/state).
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
- **No event is dropped for want of an id.** A provider that omits its own id
  (a `WhatsApp` status without `id`, a Matrix event without `event_id`) still
  gets an event: the `external_id` is then synthesised — `SYNTHETIC_ID_PREFIX`
  followed by the SHA-256 of the channel, the event role and the raw object, so
  a replay of the same body dedupes. `NormalizedMessage::has_synthetic_id` tells
  an invented key from a provider-assigned one. An entry that carries none of the
  provider's own fields — a body forwarded from another provider — is skipped
  rather than turned into an invented event.

## License

Licensed under Apache-2.0 OR MIT.
