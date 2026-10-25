# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) with the pre-1.0
reading of it described in the README section *Versioning and semver policy*.

Breaking changes are marked `Breaking` in the section that introduced them and
carry a `!` in the conventional-commit subject.

## [Unreleased]

Nothing yet.

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
