# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) with the pre-1.0
reading of it described in the README section *Versioning and semver policy*.

Breaking changes are marked `Breaking` in the section that introduced them and
carry a `!` in the conventional-commit subject.

## [Unreleased]

### Changed

- `signature::decode_hex` and the synthetic-id suffix now use `const-hex` for
  the fixed-shape digest encode/decode path. The wire format is unchanged:
  signatures are still 64 lower-case hex digits after `sha256=`, and synthetic
  ids still carry a 64 lower-case hex SHA-256 suffix.

### Breaking

- **`Headers::insert` validates before it stores.** It now returns `Result` and
  rejects empty or non-token header names, values with bytes outside visible
  ASCII plus horizontal tab, and names or values above the new
  `MAX_HEADER_NAME_BYTES` / `MAX_HEADER_VALUE_BYTES` ceilings. Closes #26.

### Added

- **Repeated HTTP header fields are observable.** `Headers::append`,
  `Headers::from_pairs`, `Headers::from_iter` and `Headers::from` now preserve
  repeated field names, and `Headers::get_all` returns every value for a name
  in insertion order. `Headers::insert` keeps its last-write-wins
  normalisation for callers that intentionally build a single-value map.
  Closes #25.
- **Outbound retry metadata.** `ProviderPayload` now carries
  `retry(): RetryPolicy` and `idempotency_key(): Option<&IdempotencyKey>`, so a
  caller that owns HTTP retries can tell whether repeating an outbound request
  is safe. The Matrix adapter emits a transaction id and marks retries as
  `Safe`; WhatsApp and Telegram mark their POST sends as
  `UnsafeUnlessKeyPresent`, because repeating them can duplicate the message.
  `ProviderPayload::new` keeps its existing signature and defaults to the
  conservative unsafe policy, while `ProviderPayload::with_retry` is available
  for adapters that know the provider contract. Closes #39.
- **Bounded body reads: `read_body` and `MAX_BODY_READ_BYTES`.** The adapter
  size check only sees an already-materialised `&[u8]`, so it cannot stop a
  hostile client from streaming an unbounded body into memory. `read_body`
  reads at most `MAX_BODY_READ_BYTES` (`MAX_BODY_BYTES + 1`) and returns
  `Error::BodyTooLarge` once the ceiling is crossed; read failures get their own
  typed `Error::Io` variant. Closes #38.
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

### Fixed

- **Formatting spans stay tied to their text.** `NormalizedMessage::text` and
  `NormalizedMessage::spans` are no longer independent public fields. The
  fields are now private behind checked accessors
  (`text()`, `spans()` and `span_text()`), and every setter re-validates spans
  against the final text so an event cannot carry an offset that would panic
  when sliced. `with_text`, `without_text`, `with_spans` and
  `with_text_and_spans` keep text and formatting in lockstep. Closes #92.
- **Verification headers must appear exactly once.** A proxy or attacker can
  forward repeated `X-Hub-Signature-256`, `X-Telegram-Bot-Api-Secret-Token` or
  `Authorization` fields; the adapters used to authenticate whichever value
  happened to be stored last. `Headers` now preserves duplicates and every
  verifier returns the new typed `Error::DuplicateVerificationHeader` instead
  of choosing one. Closes #25.
- **`Channel::Other` no longer disagrees with canonical channel names.** The
  enum derived identity from the variant, while `as_str` returned the payload,
  so `Channel::Other("whatsapp")` logged as `whatsapp` but looked up a different
  adapter than `Channel::WhatsApp`. Equality, hashing and ordering now compare
  `Channel::as_str`, and `Channel::other` validates caller-supplied custom names
  against the same lower-case `[a-z0-9_-]` grammar. Closes #93.
- **Custom channel names are validated at construction.** `Channel::Other` now
  carries a private-field `ChannelName` instead of a raw `String`, so a custom
  channel can only be built through `Channel::other`. That constructor trims
  surrounding whitespace, lower-cases ASCII letters, rejects empty, oversized
  or malformed names, and reserves built-in channel names. This closes the
  remaining path where an empty or unbounded raw key could be registered or
  echoed through `UnknownChannel`. Closes #97.
- **`Registry::encode` rejects cross-channel messages.** The registry used the
  caller's `channel` argument for dispatch but never compared it with
  `NormalizedMessage::channel`, so a message tagged for one provider could be
  encoded by another provider's adapter under the wrong recipient and length
  rules. A mismatch now returns the existing typed `InvalidMessage` error before
  the adapter is called. Closes #96.
- **`Registry` no longer replaces an adapter invisibly.** `register` kept its
  documented override behaviour, but a duplicate registration used to discard
  the previous adapter with no error and no trace in `Debug`. `try_register`
  now reports a duplicate as `Error::ChannelAlreadyRegistered` and keeps the
  existing adapter, and `Registry`'s `Debug` output lists the channel-to-adapter
  map (secrets stay redacted) so an operator can see which adapter won. Closes
  #98.
- **HMAC construction failures are no longer reported as `BadSignature`.**
  Meta signature construction and verification now surface an impossible HMAC construction
  failure as `Error::Hmac` with the underlying `hmac::digest::InvalidLength` source, so an
  internal signing fault cannot be mistaken for a forged webhook. Closes #112.
- **Header lookup no longer allocates on mixed-case queries.** `Headers::get`
  copied the query into a fresh lower-cased `String` whenever it contained an
  ASCII upper-case byte, then scanned the entries comparing owned strings; that
  copy sat on the verification hot path every webhook request runs before
  authentication. The lookup now compares in place with
  `str::eq_ignore_ascii_case`, so the per-request allocation is gone and the
  matching semantics are unchanged: ASCII case folds, non-ASCII bytes still
  compare exactly. Closes #28.
- **Outbound recipients are validated as a boundary, not for emptiness.**
  The one shared `encode` recipient check only rejected an empty
  `conversation_id`, so a `1 MiB` id, a value carrying `\n`, NUL or a
  zero-width format character, or a non-numeric id for a provider that needs a
  numeric one all reached the provider request body. Validation now lives in
  the shared helper with a per-channel rule — the trimmed id must be printable
  and at most `MAX_RECIPIENT_BYTES` (256), `WhatsApp` takes E.164 digits,
  Telegram a signed `i64` chat id and Matrix a `!`-prefixed room id — and every
  adapter inherits it. `MAX_RECIPIENT_BYTES` is the new public ceiling. Closes
  #37.

### Breaking

- **`ProviderPayload` constructors enforce the payload contract.**
  `ProviderPayload::new` and `ProviderPayload::with_retry` now return `Result`,
  take a typed `ProviderMethod`, require `application/json`, reject non-object
  JSON and bodies above `MAX_BODY_BYTES` before the payload can be sent. This
  stops the type from accepting any method, content type and body while
  `ProviderPayload::json` documented that decode failure could not happen for
  payloads this crate produces. Closes #91.
- **Outbound messages are a distinct type.** `Adapter::encode` now accepts
  `OutboundMessage`, the fields an application can know before a provider
  assigns an event id. `NormalizedMessage` stays the inbound event shape and
  keeps `external_id` as the provider event id, so outbound requests no longer
  carry an empty field documented as an idempotency key. `NormalizedMessage::text`
  is replaced by `NormalizedMessage::inbound_text` for tests and examples; use
  `OutboundMessage::text` for sends. Closes #90.
- `NormalizedMessage` gained the public field `spans`, so a struct literal that
  builds the whole message no longer compiles. Use `NormalizedMessage::text`
  (which leaves `spans` empty) or `..Default::default()`-style updates where
  possible. The next version therefore moves to **0.4.0**; 0.3.4 stays the
  released one.

### Fixed

- **Synthesised ids no longer materialise the event body twice.** An event the
  provider sent without an id is keyed by the canonical JSON of that object, but
  the digest path serialised the object into a second heap `String` before
  hashing it. The renderer now streams directly into SHA-256 through a bounded
  writer, so the same stable `synthetic:` key is produced without a
  body-sized allocation, and the private helper rejects input over
  `MAX_BODY_BYTES` if it is ever reached outside the adapter's existing size
  gate.
- **`MxcUri` now hands out the server and media id it validated.** `parse`
  split the URI with `strip_prefix("mxc://")` but `server_name` and `media_id`
  re-derived the split with `trim_start_matches("mxc://")`, which strips the
  prefix repeatedly. An accepted value such as `mxc://mxc://x` therefore
  reported `server_name() == "x"` and an empty `media_id()`, contradicting the
  type's own invariant. The parsed split is now stored on the value and returned
  directly, so the accessors cannot disagree with validation.
- **The body ceiling is enforced by the dispatch path.** `Registry::verify`,
  `Registry::adapt` and `Registry::handle` now reject a body above
  `MAX_BODY_BYTES` before calling the registered adapter, so a custom `Adapter`
  cannot silently widen the documented crate-wide ceiling. Closes #36.
- **Provider ids that impersonated the synthetic namespace are retagged.** A
  real provider id such as `synthetic:x` used to reach
  `NormalizedMessage::external_id` verbatim, so `has_synthetic_id()` reported
  the opposite of the truth. Adapters now keep the id but add a `provider:`
  namespace when the provider's value starts with `SYNTHETIC_ID_PREFIX`, which
  preserves provenance for idempotency and trust decisions. Closes #33.
- **Synthetic ids are now canonical under `serde_json/preserve_order` and
  number spelling changes.** The digest for an event without a provider id used
  `serde_json::Value`'s `Display`, which follows the active map implementation
  and preserves `1` versus `1.0`. It now hashes RFC 8785 JCS bytes via
  `serde_json_canonicalizer`, so a provider replay that reorders object keys or
  rewrites equivalent numbers still maps to the same key. A default-off
  `preserve_order` feature builds and tests the `IndexMap` path in CI.

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

- The empty-id boundary was checked on one side only: `json::id` rejected `""`
  but accepted any other non-empty string, so an id of `" "` or `"\u{200b}"` —
  placeholders providers do emit for absent ids — became an `external_id`
  verbatim, identical for every such event. The ledger then dropped every event
  but the first, the exact failure synthesis exists to prevent. A string id is
  now returned only when it carries at least one graphic character (the Unicode
  general category decides, so zero-width format characters fail closed too);
  otherwise the id is synthesised. Numbers are unaffected: `0` stays a real id.
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
