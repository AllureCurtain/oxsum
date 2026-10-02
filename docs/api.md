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

## Idempotency

Every write endpoint requires an `idempotencyKey`:

- Replaying the same key with the same content returns the original receipt with `isNew: false`; nothing is booked twice.
- The same key with different content is an error; the original record is never overwritten.

On timeout or network errors the client retries with the same key, never a new one.

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
| `INSUFFICIENT_FUNDS` | 402 | available balance too low; hold or charge refused |
| `NOT_FOUND` | 404 | resource does not exist, or belongs to another organization |
| `CONFLICT` | 409 | the value is already taken; registering an email that exists |
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
