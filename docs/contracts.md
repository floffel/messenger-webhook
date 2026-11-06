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

# Meta: the webhook subscription handshake

Source: **Graph API — Webhooks, Getting Started**, *Verification Requests*,
<https://developers.facebook.com/docs/graph-api/webhooks/getting-started>
(retrieved 2026-10-27).

> Anytime you configure the Webhooks product in your App Dashboard, we'll send a
> GET request to your endpoint URL. Verification requests include the following
> query string parameters, appended to the end of your endpoint URL. They will
> look something like this:
>
> ```
> GET https://www.your-clever-domain-name.com/webhooks?
>   hub.mode=subscribe&
>   hub.challenge=1158201444&
>   hub.verify_token=meatyhamhock
> ```
>
> | Parameter | Sample Value | Description |
> |---|---|---|
> | `hub.mode` | `subscribe` | This value will always be set to `subscribe`. |
> | `hub.challenge` | `1158201444` | An int you must pass back to us. |
> | `hub.verify_token` | `meatyhamhock` | A string that we grab from the Verify Token field in your app's App Dashboard. |

> Whenever your endpoint receives a verification request, it must: Verify that the
> `hub.verify_token` value matches the string you set in the **Verify Token** field
> […] Respond with the `hub.challenge` value.

> Note: PHP converts periods (.) to underscores (_) in parameter names.

What the crate does with it:

- `subscription::meta_subscription_challenge` takes the channel whose
  subscription is being answered, the raw query string and the dashboard token,
  compares the token in constant time and returns the challenge parsed as an
  unsigned integer and rendered canonically in decimal; nothing is concatenated
  onto it. The channel is carried on every error, so a Messenger handshake
  failure is not misreported as WhatsApp.
- Both spellings of every parameter are accepted (`hub.mode` and `hub_mode`),
  because the underscore note above means an endpoint behind a PHP layer sees the
  second one.
- A parameter may appear more than once only when every occurrence has the same
  decoded value. Conflicting values are rejected instead of resolved: the
  request would otherwise carry two candidate tokens and the endpoint would have
  to pick one. The dotted and PHP alias spellings share one logical parameter,
  so equal values across spellings are accepted together. Unknown parameters are
  ignored.
- The challenge is reflected into the caller's response, so it is bounded
  (`MAX_CHALLENGE_BYTES`) and parsed as an integer before it is returned. Only
  canonical decimal digits can therefore be reflected, so markup, quotes, angle
  brackets, backslashes or CR/LF never reach the caller's response body.
- An empty dashboard token fails closed (`Error::MissingSecret`) before the query
  is even parsed, because comparing against an unset token accepts anyone. This
  case is server misconfiguration and should be answered with `500`, not `403`,
  so Meta keeps retrying while the operator fixes the deployment;
  `Error::is_misconfiguration()` exposes that classification. Malformed
  verification requests and bad signatures remain client rejections and should
  be answered with `403`.

Pinned by `tests/subscription.rs` (34 tests, three of them proptest properties:
"challenge returned exactly when the token matches", "arbitrary query strings
never panic" and "accepted challenges are bounded decimal"), plus the
`subscription` fuzz target. The sample request above is the first test case,
byte for byte.

# Telegram: `MessageEntity`

Source: **Telegram Bot API — `MessageEntity`**,
<https://core.telegram.org/bots/api#messageentity> (retrieved 2026-10-29).

`type`:

> Type of the entity. Currently, can be “mention” (`@username`), “hashtag”
> (`#hashtag` or `#hashtag@chatusername`), “cashtag” (`$USD` or
> `$USD@chatusername`), “bot_command” (`/start@jobs_bot`), “url”
> (`https://telegram.org`), “email” (`do-not-reply@telegram.org`),
> “phone_number” (`+1-212-555-0123`), “bold” (bold text), “italic” (italic
> text), “underline” (underlined text), “strikethrough” (strikethrough text),
> “spoiler” (spoiler message), “blockquote” (block quotation),
> “expandable_blockquote” (collapsed-by-default block quotation), “code”
> (monowidth string), “pre” (monowidth block), “text_link” (for clickable text
> URLs), “text_mention” (for users without usernames), “custom_emoji” (for inline
> custom emoji stickers), or “date_time” (for formatted date and time).

`offset`:

> Offset in UTF-16 code units to the start of the entity.

`length`:

> Length of the entity in UTF-16 code units.

What the crate does with it:

- All twenty types are mapped in [`format_of`](../src/telegram.rs); nineteen onto
  a `TextFormat` variant, `date_time` onto `TextFormat::Other("date_time")`
  because the neutral model has no timestamp attribute yet — the run stays
  visible instead of disappearing.
- Every range is converted from UTF-16 units to UTF-8 byte offsets before a
  `TextSpan` is built, so `"a😀b"` with offset 1 / length 2 is bytes `1..5`. The
  conversion and its rejections (mid-surrogate, past the end, zero length) are
  pinned by `tests/rich_text.rs`, including a proptest over every char boundary.
- `text_link` carries `url`, `pre` carries `language`, `text_mention` carries
  `user.id`, `custom_emoji` carries `custom_emoji_id`; a structured entity
  without its payload keeps its provider name as `TextFormat::Other`, so a
  malformed entity cannot erase a run (`structured_entities_without_their_payload_keep_the_provider_name`).
- `caption_entities` describes `caption`, not `text`; a media message is pinned
  by `caption_entities_describe_the_caption`.
- Outbound, `OutboundMessage::spans` is encoded back into `entities` or
  `caption_entities`; offsets are converted from UTF-8 bytes to UTF-16 code
  units and `parse_mode` stays unset because Telegram treats entities and
  markdown as mutually exclusive. `text_mention` is rejected because Telegram
  requires a full `User` object, while the neutral span carries only `user_id`.
  More than `MAX_ENTITIES` (100) runs is rejected before the wire: Telegram
  silently discards the extra entities past that ceiling, so a larger array
  would arrive with its tail unformatted and no error.

The Telegram update/MessageEntity model is intentionally local rather than a
maintained Telegram type crate. `frankenstein` is WTFPL, which is outside
`deny.toml`'s allow list. `teloxide`/`teloxide-core` are MIT, but their model
types arrive with an HTTP/runtime stack and stricter enum modelling than this
webhook-only decoder wants. The permissive `json::*` accessors are kept because
unknown fields and unknown enum variants have to survive in
`NormalizedMessage::raw`. To keep that choice from drifting, every Telegram
field name and update-root mapping used by the adapter lives in
[`src/telegram/schema.rs`](../src/telegram/schema.rs) and is pinned by unit tests
in that module against the Bot API names above.

Unverified: the visible result of `date_time` formatting (it depends on the
client's locale), and whether `custom_emoji` markup survives forwarding — neither
affects the offsets this crate emits.

# Webhook body ceilings

Each provider publishes its own ceiling on a webhook body, so the ceiling is a
property of the adapter (`Adapter::max_body_bytes`), not a crate-wide number.
`Registry` reads the value from the registered adapter and rejects an oversized
body before dispatch, hashing or JSON parsing; `MAX_BODY_BYTES` is only the
fallback a custom adapter inherits and the bound `read_body` applies while the
adapter is still unknown.

## WhatsApp — 3 MiB

Source: **WhatsApp Cloud API — Webhooks, Overview**, *Payload size*,
<https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/overview>
(retrieved 2026-11-06).

> Webhook payloads can be up to 3 MB.

`whatsapp::MAX_WEBHOOK_BODY_BYTES` is `3 * 1024 * 1024`, pinned at N/N+1 by
`body_size_is_checked_before_hashing` and `whatsapp_body_ceiling_is_three_mebibytes`.

## Telegram — 4 MiB

The Bot API does not publish a webhook body ceiling. Its reference server,
tdlib `telegram-bot-api`, caps a serialised batch of updates at `1 << 22` bytes
and sends one update per webhook `POST`
(`Client::JsonUpdates` in
<https://github.com/tdlib/telegram-bot-api/blob/master/telegram-bot-api/Client.cpp>:
`int left_len = 1 << 22;`). `telegram::MAX_WEBHOOK_BODY_BYTES` is `1 << 22`.

## Matrix — 6 MiB

The Matrix specification caps one event at 65536 bytes
(see *Event size ceiling* above), and a homeserver pushes events in one
application-service transaction, batching up to 100 of them
(`MAX_PERSISTENT_EVENTS_PER_TRANSACTION` in Synapse's
`synapse/appservice/scheduler.py`). `matrix::MAX_WEBHOOK_BODY_BYTES` is
`100 * 65_536`, the largest transaction the reference implementation produces.
