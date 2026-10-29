# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) with the pre-1.0
reading of it described in the README section *Versioning and semver policy*.

Breaking changes are marked `Breaking` in the section that introduced them and
carry a `!` in the conventional-commit subject.

## [Unreleased]

### Added

- **Rich text: `NormalizedMessage::spans`.** Providers flatten formatting into
  markup that this crate threw away, so a Telegram `**bold**` run reached the
  application as nothing but text. `NormalizedMessage` now carries
  `spans: Vec<TextSpan>`, each with UTF-8 byte offsets into `text`, and
  `telegram` fills them from `entities` and `caption_entities`: `bold`, `italic`,
  `underline`, `strikethrough`, `spoiler`, `code`, `pre` (with `language`),
  `text_link` (with `url`), `mention`, `hashtag`, `cashtag`, `bot_command`,
  `url`, `email`, `phone_number`, `text_mention` (with `user_id`),
  `custom_emoji` (with id), `blockquote` and `expandable_blockquote`.
  Telegram counts offsets in UTF-16 code units, so every range is converted to
  bytes; a range that starts inside a surrogate pair, runs past the end or is
  empty is dropped instead of clamped — the text stays complete and no span
  claims markup the provider did not send. `TextSpan::new` is the only
  constructor and rejects such ranges (`SpanError`), `slice`/`covers` answer the
  two questions a renderer asks, and `NormalizedMessage::is_formatted` says
  whether there is anything to render. A structured entity whose payload is
  missing (`text_link` without `url`, `custom_emoji` without its id) keeps its
  provider name as `TextFormat::Other`, so the run is not lost. The remaining
  surface is tracked in #12 (WhatsApp interactive replies), #13 (Matrix
  `formatted_body`) and #14 (outbound Telegram entities).

### Breaking

- `NormalizedMessage` gained the public field `spans`, so a struct literal that
  builds the whole message no longer compiles. Use `NormalizedMessage::text`
  (which leaves `spans` empty) or `..Default::default()`-style updates where
  possible. The next version therefore moves to **0.4.0**; 0.3.4 stays the
  released one.

## [0.3.3] - 2026-10-27

### Added

- **The Meta webhook-subscription handshake.** Before Meta posts an event it
  sends a `GET` with `hub.mode`, `hub.challenge` and `hub.verify_token`, and the
  endpoint has to compare the token and echo the challenge back. Nothing in the
  crate did that, so every `WhatsApp` consumer had to hand-roll it — usually
  comparing the token with `==` (timing leak) and reflecting whatever arrived.
  `subscription::meta_subscription_challenge(&secret, query)` now takes the raw
  query string and returns the challenge: the token is compared in constant
  time, an empty dashboard token fails closed before the query is parsed, a
  parameter that appears twice is rejected instead of resolved, unknown
  parameters are ignored, and the echoed challenge is bounded
  (`MAX_CHALLENGE_BYTES`, 128) and must be printable, non-space ASCII so it
  cannot split a response. Both spellings of each parameter are accepted
  (`hub.mode` and `hub_mode`) because Meta's page notes that PHP rewrites the
  dots to underscores. The query string is parsed with `form_urlencoded` — the
  maintained parser for that syntax — rather than by splitting on `&`, which is
  why the `whatsapp` feature now pulls it in; no other feature does.
  `Error::MalformedVerificationRequest` is the new, additive failure for a
  request whose shape is wrong. The contract is quoted with its source in
  `docs/contracts.md`, and `tests/subscription.rs` pins it with 24 tests
  (including two proptest properties). Closes #1.
- `tests/api_surface.snapshot` records the six new items, so the surface change
  is a reviewed diff rather than a surprise.

## [0.3.2] - 2026-10-27

### Changed

- **An event without a provider id is no longer dropped.** Adapters used to
  return nothing for a `WhatsApp` message or status without `id`, a Telegram
  update without `message_id` or a Matrix event without `event_id`, which lost
  real traffic. Those events now arrive with a synthesised `external_id`:
  `SYNTHETIC_ID_PREFIX` (`synthetic:`) followed by the SHA-256 of the channel,
  the event role and the raw object. The digest ignores JSON key order, so a
  provider replay yields the same key and the caller's dedupe ledger still
  works; `NormalizedMessage::has_synthetic_id` distinguishes an invented key
  from a provider-assigned one. An event whose conversation or sender field is
  missing keeps an empty string there instead of vanishing; entries without
  the provider's own fields at all (a body from another provider) are still
  skipped. Closes #10.

## [0.3.1] - 2026-10-25

### Added

- **`matrix-out` feature (default off): a typed Matrix Client-Server outbound
  client.** `MatrixClient` sends text and arbitrary event types, reactions
  (`m.reaction` / `m.annotation`), redactions, typing notifications, read
  receipts and media uploads; `whoami` probes the account. The crate still does
  no I/O in its default build — the client talks to a `Transport`, and
  `ReqwestTransport` is the shipped implementation. `StubTransport` and
  `RecordingSleeper` are public so consumers can test without a homeserver.
- **Idempotent sends.** Every event send is keyed by a validated `TxnId`; the
  id is reused across internal retries and is returned in `SendOutcome` and in
  `OutboundError::Ambiguous`, so a caller can repeat a send at most once.
  `TxnId::generate()` yields unique, monotonically ordered ids.
- **Delivery classification.** `RetryPolicy` + `Operation` decide whether an
  attempt may be repeated: `429` is honoured via `retry_after_ms` (or a
  `Retry-After` header, or the backoff), `5xx`/timeouts on a transaction-keyed
  send retry with the same id, a media upload is never retried after an
  uncertain outcome, and `OutboundError::is_retryable()` / `is_ambiguous()`
  state which of the three delivery contracts applies.
- **Per-room ordering.** Sends into one room are serialised (configurable),
  bounded gate map, no lock held across an `await`.
- `docs/contracts.md` — every endpoint and error shape this crate encodes,
  quoted from the Matrix Client-Server API v1.19 with its source URL.
- Test suites `matrix_out`, `matrix_out_edge`, `matrix_out_properties`,
  `matrix_out_concurrency` and `matrix_out_http` (the only one that opens a
  socket: a loopback server for the `reqwest` transport).

## [0.3.0] - 2026-10-25

### Breaking

- `Channel` is `#[non_exhaustive]`. An exhaustive `match` over it needs a `_`
  arm; in exchange, a new built-in messenger is additive from here on, which is
  what the 0.1.0 API review asked for. `Error`, `AttachmentKind` and
  `DeliveryStatus` already were.

### Added

- `tests/api_freeze.rs` freezes the surface: every public item of `src/` is
  recorded in `tests/api_surface.snapshot`, so adding, renaming or removing one
  fails the build until the snapshot is regenerated on purpose. The same file
  enforces `#[non_exhaustive]` on every public enum and that no private module
  (`json`) leaks through a `pub use`.

### Fixed

- `signature::verify_meta` and `signature::meta_signature` fail closed on an
  empty secret instead of verifying with a zero-length HMAC key. HMAC accepts
  `""` as a key, so an unset app secret previously accepted
  `sha256=HMAC("", body)` for *any* body — a signature bypass for callers of the
  public helpers (the adapters already refused). Both now return
  `Error::MissingSecret { channel: Channel::WhatsApp }` before parsing the
  header or hashing.
- The `decode_hex` non-ASCII test exercised the length check, not the byte
  check: `"ü".repeat(64)` is 128 UTF-8 bytes. It now also asserts on a 64-byte
  input that carries a multi-byte character, directly and through the public
  header path.

## [0.2.1] - 2026-10-25

### Changed

- `hmac` 0.12 → 0.13 and `sha2` 0.10 → 0.11 (digest 0.11). No API change: the
  HMAC-SHA256 digests are byte-identical, which the signature tests pin against
  fixed vectors, so existing `X-Hub-Signature-256` values keep verifying.

## [0.2.0] - 2026-10-24

First release on crates.io. `0.1.0` was an internal iteration that was never
published.

### Added

- `Registry` — register adapters once, verify and normalise by `Channel`, with a
  typed error for an unregistered channel.
- Outbound encoding: `Adapter::encode` turns a `NormalizedMessage` into a
  `ProviderPayload` (method, content type, JSON body) for `whatsapp`, `telegram`
  and `matrix`, enforcing provider limits before a payload is produced
  (`Error::MessageTooLong`, `Error::InvalidMessage`).
- `MatrixAdapter` for `m.room.message`, `m.reaction`, `m.receipt` and unknown
  event types.
- Property tests over arbitrary JSON and arbitrary bytes, edge-case suites,
  a 32-task concurrency test, the `normalize` fuzz target with a compiled-in
  corpus, and a coverage gate in CI.

### Breaking

- Verification is part of the adapter contract: adapters are constructed with
  their secret and `verify` lives on the `Adapter` trait. `adapt` is no longer
  the only entry point and an adapter cannot exist without a secret.
- Normalisation is complete: reactions, delivery receipts, edits, threads,
  reply references and media metadata are now carried on `NormalizedMessage`.
- Unknown channels are carried as `Channel::Other(String)` instead of failing.

## [0.1.0] - 2026-10-22

### Added

- Initial implementation: `WhatsApp` and Telegram inbound adapters, the
  `NormalizedMessage` model, `Headers` and `Secret`, constant-time
  `X-Hub-Signature-256` verification, dual `Apache-2.0 OR MIT` licensing, and a
  tag-driven release workflow.
