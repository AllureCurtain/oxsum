# API conventions

Field definitions for each endpoint are authoritative in `crates/server/openapi.yaml`; this file only records general conventions.

## Basics

- Prefix: `/api/v1`. `/healthz` needs no auth and sits outside the prefix.
- Auth: `Authorization: Bearer <OXSUM_API_TOKEN>`. Currently one shared token; per-tenant keys are on the TODO.md roadmap.
- Time: ISO 8601, UTC. The posting date is decided by the server.
- Money: integers in minor units, 1 credit = 1_000_000. Field names end in `Minor`.
- Field naming: camelCase for both requests and responses.
- Tenant ids: lowercase letters, digits and underscores only, 1 to 40 chars.
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
| `UNAUTHORIZED` | 401 | token missing or wrong |
| `INSUFFICIENT_FUNDS` | 402 | available balance too low; hold or charge refused |
| `NOT_FOUND` | 404 | resource does not exist |
| `INTERNAL_ERROR` | 500 | server error; details only in logs |

## Pagination

No list endpoints yet. When they arrive, use cursor pagination with `?cursor=<opaque>&limit=20`, limit capped at 100. The ledger log paginates by position, not page/offset.

## Change process

1. Change `crates/server/openapi.yaml`
2. Change the backend implementation, add integration tests
3. Change the callers (Leptos pages, browser verification component)
