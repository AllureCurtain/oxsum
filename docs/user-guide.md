# User guide

For callers integrating with oxsum; no technical internals. Field details live in `crates/server/openapi.yaml`. Every API request except `GET /healthz` and `POST /api/v1/auth/register` carries `Authorization: Bearer <api-key>`, and the key decides which organization the request acts for — no request names a tenant.

## Get an account and a key

Purpose: get a user, an organization to hold credits, and a credential to call the API with.

Steps:

1. `POST /api/v1/auth/register` with `email`, `password` (at least 12 characters) and optionally `organizationName`.
   - The response carries `user`, `organization` and `apiKey.secret`. **The secret is shown once and cannot be retrieved again**; store it now.
   - A personal organization is created with you as its owner, named after the address's local part unless you chose a name.
   - An existing address answers 409 `CONFLICT`.
   - Some deployments answer 403 `FORBIDDEN`: registration is by invitation there, and the operator hands out the key instead.
2. Use `apiKey.secret` as the Bearer credential from then on.

## Keys

Purpose: several integrations, one organization, revocable separately.

- `POST /api/v1/org/keys` mints another key, with an optional `name` and an optional RFC 3339 `expiresAt`. The secret comes back once.
- `GET /api/v1/org/keys` lists the organization's keys: id, name, display prefix, timestamps. Never a secret.
- `DELETE /api/v1/org/keys/{keyId}` revokes one. It stops working immediately; the rest keep working.
- `GET /api/v1/org` describes the organization the credential belongs to.
- A key that is unknown, revoked or expired all answer the same 401 `UNAUTHORIZED`, so a leaked key's state is not revealed by probing.

## Top up

Purpose: add balance to the organization's wallet.

Steps:

1. `POST /api/v1/topups` with `idempotencyKey` and `amountMinor` in the body.
2. Save the returned `contentHash`; you will need it to verify the bill later.

Note: the unit is minor, 1 credit = 1_000_000.

## Paying for one AI call

Purpose: reserve credit before the call, charge actual usage after it, refund the unspent rest.

Steps:

1. Before calling the LLM, `POST /api/v1/holds` to freeze an upper bound of what the call can cost.
   - A 402 `INSUFFICIENT_FUNDS` response means the balance is too low; do not start the call.
2. After the call finishes (success or failure), `POST /api/v1/settlements`:
   - `heldMinor`: the amount originally frozen
   - `actualMinor`: the actual spend; use 0 for a failed call and the whole hold is refunded

Notes:

- The hold and the settlement use different `idempotencyKey`s, e.g. `req-123:hold` and `req-123:settle`.
- After a network timeout, retry with the same key; you will not be charged twice.
- `actualMinor` must not exceed `heldMinor`.

## Balance

Purpose: check the currently available balance.

Step: `GET /api/v1/balance`. The returned `availableMinor` already subtracts unsettled holds.

## Verifying a bill

Purpose: confirm a recorded transaction has not been altered since.

Steps:

1. `GET /api/v1/entries/{entryId}/proof` to fetch the proof bundle.
2. Verify with the bundle and the `contentHash` you saved. The browser verification page is still in development.

Note: verification proves "this record has not been altered since it was written"; it cannot prove "the recorded number was correct in the first place".
