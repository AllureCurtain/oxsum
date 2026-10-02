# API conventions

Field definitions for each endpoint are authoritative in `crates/server/openapi.yaml`; this file only records general conventions.

## Basics

- Prefix: `/api/v1`. `/healthz` needs no auth and sits outside the prefix; so does `POST /api/v1/auth/register`, when the deployment allows signup at all.
- Auth: `Authorization: Bearer oxs-…`, an organization's API key. The key decides which organization a request spends, so no endpoint takes a tenant from the caller. Unknown, revoked and expired keys are all `UNAUTHORIZED`, with no hint which it was.
- Time: ISO 8601, UTC. The posting date is decided by the server.
- Money: integers in minor units, 1 credit = 1_000_000. Field names end in `Minor`.
- Field naming: camelCase for both requests and responses.
- Organization tenant ids: 32 lowercase hex characters, the organization's UUID without dashes. The ledger lives in the schema `ledger_<tenantId>`. Clients only ever read this value; they never send it.
- Idempotency keys: non-empty UTF-8 strings, at most 128 bytes.
- `/v1/*` is a second surface with the same key: an OpenAI-compatible gateway, whose bodies and errors are OpenAI's rather than oxsum's. It is documented in its own section below.
- `/api/v1/admin/*` is a third: the platform admin's surface, opened by `OXSUM_ADMIN_TOKEN` rather than by an API key. It answers in the same envelope and the same error codes as the rest of `/api/v1`, and is documented in its own section below.

## Idempotency

Every write endpoint requires an `idempotencyKey`:

- Replaying the same key with the same content returns the original receipt with `isNew: false`; nothing is booked twice.
- The same key with different content is `CONFLICT`; the original record is never overwritten. "Different content" means a different amount, a different kind of write, or any other field the entry is built from.

On timeout or network errors the client retries with the same key, never a new one.

## Holds and settlements

- A hold reserves part of the available balance. The settled balance is untouched until a settlement discharges the hold; the difference between the amount held and the amount charged goes back to the available balance.
- A settlement names the hold it releases (`holdKey`): the server reads the hold's amount from the hold entry in the ledger, so there is no amount to assert. One hold settles at most once — the settlement entry's idempotency key is derived from the hold's key (`oxsum_core::settlement_key_for`), and the ledger's idempotency gate refuses a second settlement of the same hold with `CONFLICT`, inside the append. Retrying the identical settlement replays it.
- Naming a hold that is not outstanding is `NOT_FOUND`, even when other holds would cover the amount. The wallet's limit still refuses a release the outstanding reservations cannot cover, as the backstop behind the pairing.

## OpenAI-compatible gateway (`/v1`)

Point an OpenAI client's `base_url` at `/v1` and everything else stays the client's own.

- `GET /v1/models` lists the models this deployment serves — exactly those with a price. `owned_by` is the channel that serves one, and `created` is when the version in force was written.
- `POST /v1/chat/completions` relays one turn, streaming or not, and bills it. Fields the gateway does not act on are forwarded upstream unchanged, so a client that works against the provider works here. `max_tokens` is set to the output upper bound the freeze was computed for, `max_completion_tokens` is dropped, and `stream_options.include_usage` is forced on for a stream.
- Errors are OpenAI's `{"error": {"message", "type", "param", "code"}}`, with oxsum's own code in `error.code`. Upstream's error object is passed through verbatim on a 502.
- Every response carries `x-oxsum-request-id`. The entries a turn wrote are `req-<id>:hold` and the settlement derived from it (`oxsum_core::settlement_key_for`), and `oxsum_core::entry_id_for` derives their ids, so the id in a header is enough to name the bill.
- Text content only: an image or another content part is 400, because the freeze needs an input bound that cannot be undercounted.
- How each turn ends is recorded in the settlement entry's description as its `kind`; docs/user-guide.md has the table. The same description carries the `channel` and the `priceVersion` that priced the turn, so the prices can be checked against the configuration afterwards.
- A request is priced by the version in force when it starts. A price change during a turn reaches later requests only; the turn in flight settles at the version it began on.

## Platform admin (`/api/v1/admin`)

Whoever deploys oxsum, and nobody else: the credential is the operator token from `OXSUM_ADMIN_TOKEN`, sent as a bearer token, compared in constant time, and refused unless it is at least 16 characters when the server starts. An organization's API key does not open this surface, and this token does not open an organization's.

- `GET /api/v1/admin/channels` — the channels, each with the current version of every model it serves. Only the last four characters of an upstream credential are ever returned.
- `POST /api/v1/admin/channels` — create a channel (name, baseUrl, apiKey), or replace the connection of the channel that already has that name. Prices are untouched by a connection change.
- `POST /api/v1/admin/channels/{channelName}/prices` — append a price version for a model, and answer the version that was written. `CONFLICT` when another channel already serves that model: in v1 one model belongs to one channel.
- `GET /api/v1/admin/channels/{channelName}/prices` — every version of every model of that channel, newest first. This is the history, and it is what makes the `priceVersion` in an old settlement checkable.

A deployment that sets no `OXSUM_ADMIN_TOKEN` has no admin surface: the routes exist and answer `UNAUTHORIZED`, rather than being open or absent.

## Response format

Success:

```json
{ "data": {} }
```

Failure:

```json
{ "error": { "code": "VALIDATION_ERROR", "message": "caller-facing explanation", "details": [] } }
```

## Error codes

| code | HTTP | meaning |
| --- | --- | --- |
| `VALIDATION_ERROR` | 400 | request validation failed |
| `UNAUTHORIZED` | 401 | key or operator token missing, malformed, unknown, revoked or expired |
| `FORBIDDEN` | 403 | the caller may not do this; registration when signup is not open |
| `INSUFFICIENT_FUNDS` | 402 | the wallet cannot cover it: a hold larger than the available balance |
| `NOT_FOUND` | 404 | resource does not exist, or belongs to another organization; a settlement naming a hold that is not outstanding |
| `CONFLICT` | 409 | the value is already taken, or a key was reused for a different request; registering an email that exists; settling a hold that is already discharged |
| `INTERNAL_ERROR` | 500 | server error; details only in logs |

## API keys

- Format: `oxs-` followed by 32 random bytes, so secret scanners can recognize one. The plaintext is returned once, at creation; the database stores only its SHA-256 hash and a display prefix.
- Keys belong to an organization and hold no money. `created_by` records the user who minted one, for the role rules product.md defines.
- Revocation is permanent and idempotent; a key id of another organization is `NOT_FOUND`, never `FORBIDDEN`, so ids cannot be probed.
- Expiry is optional. An expired key is refused exactly like a revoked one.
- Until sessions arrive, any active key of an organization may create and revoke keys of that organization: a role check needs a user to be the acting principal, and users act through web login. The schema already records what the check needs.

## Pagination

No list endpoints paginate yet. `GET /api/v1/org/keys` returns an organization's keys in full, which is bounded and small. When a genuinely unbounded list arrives, use cursor pagination with `?cursor=<opaque>&limit=20`, limit capped at 100. The ledger log paginates by position, not page/offset.

## Change process

1. Change `crates/server/openapi.yaml`
2. Change the backend implementation, add integration tests
3. Change the callers (Leptos pages, browser verification component)
