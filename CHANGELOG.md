# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) with the pre-1.0
reading of it described in the README section *Versioning and semver policy*.

Breaking changes are marked `Breaking` in the section that introduced them and
carry a `!` in the conventional-commit subject.

## [Unreleased]

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
