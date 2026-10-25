# Provider contracts this crate encodes

Every shape below is quoted from the published specification and pinned by a
fixture or an assertion, because a client written against an imagined API passes
its own mocks and fails against the real server. Source: **Matrix Client-Server
API v1.19**, <https://spec.matrix.org/latest/client-server-api/> (retrieved
2026-10-25).

## Transaction identifiers — the idempotency contract

Section *Transaction identifiers*:

> If the client retries the request with the same transaction ID, the server
> should return the same HTTP response code and content as the original request.
> For example, `PUT /_matrix/client/v3/rooms/{roomId}/send/{eventType}/{txnId}`
> would return a 200 OK with the `event_id` of the original request in the
> response body.
>
> The scope of a transaction ID is for a single device, and a single HTTP
> endpoint.

What the crate does with it:

- [`TxnId`](../src/matrix_out/types.rs) mints one id per send and reuses it for
  every internal attempt (pinned by
  `every_attempt_of_one_send_shares_one_transaction_id`).
- A caller re-driving a send passes the id from
  `OutboundError::Ambiguous::txn_id`, so a send whose outcome was unknown is
  repeated at most once, not twice.
- The *device* scope matters: a token refresh keeps the device, a logout does
  not — so an id is only reusable while the client keeps the same access token.

## Send an event

`PUT /_matrix/client/v3/rooms/{roomId}/send/{eventType}/{txnId}`

Response `200`: `{"event_id": "$143273582443PhrSn:example.org"}`.
Verified by `tests/matrix_out.rs::a_text_send_is_one_put_with_a_bearer_token_and_a_json_body`
and the loopback test `tests/matrix_out_http.rs`.

## Event size ceiling

Section *Room Events → Size limits*:

> The complete event MUST NOT be larger than 65536 bytes, when formatted with
> the federation event format, including any signatures, and encoded as Canonical
> JSON.

The crate checks the **content** against that number before building a request
(`MAX_CONTENT_BYTES`). The content is a strict part of the event, so the check
rejects only what can never be sent; the homeserver enforces the exact bound,
because the envelope adds sender, room, signatures and metadata. Pinned at N/N+1
by `the_content_ceiling_is_exact` and `content_at_the_ceiling_is_accepted_and_one_byte_past_is_not`.

## Rate limiting

Section *Rate limiting* defines the `429` answer:

```json
{ "errcode": "M_LIMIT_EXCEEDED", "error": "Too many requests", "retry_after_ms": 2000 }
```

- `retry_after_ms` is honoured exactly (clamped by `RetryPolicy::max_rate_limit_wait`);
  a missing field falls back to the policy's backoff, and a `Retry-After` header
  (HTTP seconds) is the second fallback for proxies in front of a homeserver.
- A `429` is answered before the request is processed, so it is retryable even
  for a non-idempotent upload. Pinned by
  `a_429_without_a_hint_with_garbage_and_with_a_zero_hint` and the classification
  property test.

## Media upload

`POST /_matrix/media/v3/upload?filename={name}` with the bytes as the body and
`Content-Type` describing them. Response `200`:
`{"content_uri": "mxc://example.org/AQwafuaFswefuhsfAFAgsw"}`.

- Error statuses the crate classifies: `403` (`M_FORBIDDEN`), `413`
  (`M_TOO_LARGE`), `429` (`M_LIMIT_EXCEEDED`).
- **No transaction id, therefore no idempotency.** The upload is the one
  `Operation::NonIdempotent` leg: after a `5xx` or a read timeout it is never
  retried, and the failure surfaces as `OutboundError::Ambiguous`.
- The size ceiling is *not* in the specification. `DEFAULT_MAX_UPLOAD_BYTES`
  (50 000 000) is the spec's own example `m.upload.size` value and Synapse's
  default; the live value is advertised by the homeserver:

  ```
  GET /_matrix/client/v3/capabilities  →  {"m.upload.size": 50000000}
  GET /_matrix/client/v1/media/config  →  the same value
  ```

  Read it and pass it to `ClientConfig::with_max_upload_bytes`, so oversized
  media fails before the bytes are read instead of as a `413` afterwards.
- The specification's `POST /_matrix/client/v1/media/upload` does **not** exist
  in v1.19; the authenticated `/_matrix/client/v1/media/*` endpoints cover
  `config`, `download`, `thumbnail`, `preview_url` only. Uploading is
  `/_matrix/media/v3/upload` (and the older `POST /_matrix/media/v1/create` +
  `PUT /_matrix/media/v3/upload/{serverName}/{mediaId}` pair for resumable
  uploads). This is exactly the kind of shape a mock cannot catch.

## Reactions

Section *Event annotations and reactions*:

```json
{ "m.relates_to": { "rel_type": "m.annotation", "event_id": "$some_event_id", "key": "👍" } }
```

Removing a reaction is a **redaction**, not an empty `key`; the client rejects an
empty key instead of storing a broken annotation
(`an_empty_reaction_key_is_refused_before_a_request_exists`).

## Typing and receipts

- `PUT /_matrix/client/v3/rooms/{roomId}/typing/{userId}` with
  `{"typing": true, "timeout": 4000}`, or `{"typing": false}` to clear.
- `POST /_matrix/client/v3/rooms/{roomId}/receipt/{receiptType}/{eventId}` with
  `{}`; `receiptType` is `m.read` here.
- Both answer `200` with an empty JSON object, so the client ignores the body.

## Redaction

`PUT /_matrix/client/v3/rooms/{roomId}/redact/{eventId}/{txnId}` with
`{"reason": "..."}` (the reason is optional) and a transaction id — idempotent in
the same way a send is.

## The probe

`GET /_matrix/client/v3/account/whoami` → `{"user_id": "@bot:example.org"}`
(`device_id`, `is_guest` and further fields are accepted and ignored). The
decoder also accepts `userId`, because a reverse proxy or an appservice bridge in
front of a homeserver sometimes renames fields; unknown fields are ignored
everywhere (permissive decoding is a crate rule, strictness lives in the
validators).

## Unverified

Nothing in this client is guessed, but two behaviours are *specified* rather than
*observed*:

- Homeserver-specific `retry_after_ms` values: Synapse sends them, some proxies
  strip them. The client tolerates both, and the fixture set covers both.
- The `m.upload.size` capability value for a self-hosted homeserver. The
  default is documented, not assumed.
