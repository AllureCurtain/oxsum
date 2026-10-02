# User guide

For callers integrating with oxsum; no technical internals. Field details live in `crates/server/openapi.yaml`. Include `Authorization: Bearer <OXSUM_API_TOKEN>` on every API request except `GET /healthz`.

## Top up

Purpose: add balance to a tenant's wallet.

Steps:

1. `POST /api/v1/tenants/{tenant}/topups` with `idempotencyKey` and `amountMinor` in the body.
2. Save the returned `contentHash`; you will need it to verify the bill later.

Note: the unit is minor, 1 credit = 1_000_000.

## Paying for one AI call

Purpose: reserve credit before the call, charge actual usage after it, refund the unspent rest.

Steps:

1. Before calling the LLM, `POST /api/v1/tenants/{tenant}/holds` to freeze an upper bound of what the call can cost.
   - A 402 `INSUFFICIENT_FUNDS` response means the balance is too low; do not start the call.
2. After the call finishes (success or failure), `POST /api/v1/tenants/{tenant}/settlements`:
   - `heldMinor`: the amount originally frozen
   - `actualMinor`: the actual spend; use 0 for a failed call and the whole hold is refunded

Notes:

- The hold and the settlement use different `idempotencyKey`s, e.g. `req-123:hold` and `req-123:settle`.
- After a network timeout, retry with the same key; you will not be charged twice.
- `actualMinor` must not exceed `heldMinor`.

## Balance

Purpose: check the currently available balance.

Step: `GET /api/v1/tenants/{tenant}/balance`. The returned `availableMinor` already subtracts unsettled holds.

## Verifying a bill

Purpose: confirm a recorded transaction has not been altered since.

Steps:

1. `GET /api/v1/tenants/{tenant}/entries/{entryId}/proof` to fetch the proof bundle.
2. Verify with the bundle and the `contentHash` you saved. The browser verification page is still in development.

Note: verification proves "this record has not been altered since it was written"; it cannot prove "the recorded number was correct in the first place".
