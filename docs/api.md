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

## Idempotency

Every write endpoint requires an `idempotencyKey`:

- Replaying the same key with the same content returns the original receipt with `isNew: false`; nothing is booked twice.
- The same key with different content is `CONFLICT`; the original record is never overwritten. "Different content" means a different amount, a different kind of write, or any other field the entry is built from.

On timeout or network errors the client retries with the same key, never a new one.

## Holds and settlements

- A hold reserves part of the available balance. The settled balance is untouched until a settlement discharges the hold; the difference between the amount held and the amount charged goes back to the available balance.
- `heldMinor` on a settlement is a claim about a hold this wallet took, and it is checked as one: the amount released may not exceed the holds outstanding, or the settlement is `INSUFFICIENT_FUNDS`. The check runs inside the append, so two settlements cannot both release the same hold.
- The pairing is aggregate, not one-to-one: a settlement may name a hold smaller than the amount it releases while other holds cover the total. No value can be fabricated that way — the total released can never exceed the total held — but a client should settle the hold it took, for the amount it took it for.

## OpenAI-compatible gateway (`/v1`)

Point an OpenAI client's `base_url` at `/v1` and everything else stays the client's own.

- `GET /v1/models` lists the models this deployment serves — exactly those with a configured price.
- `POST /v1/chat/completions` relays one turn, streaming or not, and bills it. Fields the gateway does not act on are forwarded upstream unchanged, so a client that works against the provider works here. `max_tokens` is set to the output upper bound the freeze was computed for, `max_completion_tokens` is dropped, and `stream_options.include_usage` is forced on for a stream.
- Errors are OpenAI's `{"error": {"message", "type", "param", "code"}}`, with oxsum's own code in `error.code`. Upstream's error object is passed through verbatim on a 502.
- Every response carries `x-oxsum-request-id`. The entries a turn wrote are `req-<id>:hold` and `req-<id>:settle`, and `oxsum_core::entry_id_for` derives their ids, so the id in a header is enough to name the bill.
- Text content only: an image or another content part is 400, because the freeze needs an input bound that cannot be undercounted.
- How each turn ends is recorded in the settlement entry's description as its `kind`; docs/user-guide.md has the table.

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
| `UNAUTHORIZED` | 401 | key missing, malformed, unknown, revoked or expired |
| `FORBIDDEN` | 403 | the caller may not do this; registration when signup is not open |
| `INSUFFICIENT_FUNDS` | 402 | the wallet cannot cover it: a hold larger than the available balance, or a settlement releasing more than is held |
| `NOT_FOUND` | 404 | resource does not exist, or belongs to another organization |
| `CONFLICT` | 409 | the value is already taken, or a key was reused for a different request; registering an email that exists |
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
