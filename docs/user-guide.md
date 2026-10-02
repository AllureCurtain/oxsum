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

There are two ways to pay: let the gateway do both halves for you, or drive the hold and the
settlement yourself. Both are billed the same way.

### Through the gateway (recommended)

Purpose: point an OpenAI client at oxsum and have every turn frozen and charged for you.

Steps:

1. Set the client's `base_url` to `https://<your-oxsum-host>/v1` and its API key to your oxsum key:

   ```python
   from openai import OpenAI

   client = OpenAI(base_url="http://127.0.0.1:3000/v1", api_key="oxs-…")
   answer = client.chat.completions.create(
       model="deepseek-chat",
       messages=[{"role": "user", "content": "Explain holds in one sentence."}],
       max_tokens=200,
   )
   ```

2. Every response carries an `x-oxsum-request-id` header. That id is how the turn is found in the
   ledger: its entries are keyed `req-<id>:hold` and `req-<id>:settle`.

Notes:

- The gateway freezes an upper bound *before* it contacts upstream, then charges upstream's reported
  usage, never more than the freeze. A 402 means the freeze does not fit in your balance: the
  message states the freeze and the balance, and lowering `max_tokens` lowers the freeze.
- `max_tokens` is what bounds the price, so it is always what upstream is told; a request that sets
  it too high is refused rather than quietly charged for more than you meant to allow.
- Streaming works with `stream=True`. The forwarded frames are upstream's own, and the stream only
  closes after the turn has settled, so a finished stream is a settled bill. A client that hangs up
  first — an OpenAI SDK client does, the moment it reads the terminator — cancels the upstream call
  if it is still running; what the turn cost is upstream's own count when it had already reported
  one, and an estimate of what had been forwarded otherwise, marked `client_cancelled` in the
  settlement record when the turn was cut short.
- Text only in v1: an image or another content part is refused 400, because the freeze needs a
  computable input bound.

How each turn is recorded, in the settlement entry's own words:

| `kind` | When | Charged |
| --- | --- | --- |
| `usage` | Upstream reported usage and it was priced | The reported usage, rounded up |
| `estimated` | Upstream reported no usage | A local `o200k_base` estimate of the input and the forwarded answer |
| `client_cancelled` | The client went away before the turn ended | Upstream's usage when it had already reported one, an estimate of what had been forwarded otherwise |
| `upstream_error` | Upstream answered with an error before emitting anything | Nothing; the whole freeze is released |
| `upstream_unreachable` | Upstream could not be reached at all | Nothing; the whole freeze is released |
| `capped` | Upstream's usage priced above the freeze | The freeze, and the excess is recorded as an anomaly |

### By hand

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
- `heldMinor` must be covered by holds that are still outstanding; settling an amount that was never held is a 402 `INSUFFICIENT_FUNDS`, not free credit. Settle the hold you took, for the amount you took it for.
- A hold that is never settled is not released by itself yet: sweeping timed-out holds is issue #13.

## Balance

Purpose: check the currently available balance.

Step: `GET /api/v1/balance`. The returned `availableMinor` already subtracts unsettled holds.

## Verifying a bill

Purpose: confirm a recorded transaction has not been altered since.

Steps:

1. `GET /api/v1/entries/{entryId}/proof` to fetch the proof bundle.
2. Verify with the bundle and the `contentHash` you saved. The browser verification page is still in development.

Note: verification proves "this record has not been altered since it was written"; it cannot prove "the recorded number was correct in the first place".
