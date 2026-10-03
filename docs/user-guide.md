# User guide

For callers integrating with oxsum; no technical internals. Field details live in `crates/server/openapi.yaml`. Every API request except `GET /healthz`, `POST /api/v1/auth/register`, `POST /api/v1/auth/login` and `POST /api/v1/auth/logout` carries a credential — `Authorization: Bearer <api-key>` or the session cookie — and the credential decides which organization the request acts for — no request names a tenant.

## Get an account and a key

Purpose: get a user, an organization to hold credits, and a credential to call the API with.

Steps:

1. `POST /api/v1/auth/register` with `email`, `password` (at least 12 characters) and optionally `organizationName`.
   - The response carries `user`, `organization` and `apiKey.secret`. **The secret is shown once and cannot be retrieved again**; store it now.
   - A personal organization is created with you as its owner, named after the address's local part unless you chose a name.
   - An existing address answers 409 `CONFLICT`.
   - Some deployments answer 403 `FORBIDDEN`: registration is by invitation there, and the operator hands out the key instead.
2. Use `apiKey.secret` as the Bearer credential from then on.

## Log in and out

Purpose: act as yourself in a browser instead of as an API key.

Steps:

1. `POST /api/v1/auth/login` with `email` and `password`.
   - The response carries `user`, `organization`, your `role` in it, and the `session`; a `Set-Cookie` header sets the `oxsum_session` cookie (`HttpOnly`, `SameSite=Lax`). Send it back with every request — a browser does this on its own.
   - A wrong password and an unknown email both answer 401 `UNAUTHORIZED` with the same message, so neither reveals whether an account exists.
2. `GET /api/v1/session` tells you who you are logged in as: the user, the organization, the role.
3. `POST /api/v1/auth/logout` revokes the session and clears the cookie. It always answers 200 — logging out twice is not an error.

Notes:

- The session expires thirty days after login, and it stops working the moment your membership in the organization is gone.
- Every `/api/v1` endpoint accepts the cookie wherever it accepts a Bearer key; the gateway (`/v1`) takes an API key only.
- Members see and revoke only the keys they created; owners and admins see and revoke every key of the organization. Keys you mint while logged in record you as their creator.

## Dashboard

Purpose: see and manage the organization in a browser instead of over the API.

Open `/login` in a browser pointed at the server and log in with your email and
password; `/dashboard` is the overview. `/logout` logs out.

- **Overview**: the organization, who you are logged in as and in which role, the
  available balance, the frozen total (everything the organization's outstanding holds
  have reserved), this month's spend (what settlements have charged since the first day
  of the current month — the server's UTC month), the in-flight holds, and the newest
  ledger entries.
- **In-flight holds** update live: a hold appears when a gateway turn starts freezing,
  shows streaming progress while upstream answers, and leaves the list when the turn
  settles. The stream behind it is a WebSocket at `/ws/billing` (session login, like
  the pages); it carries only your organization's turns.
- **API keys**: list, mint and revoke. The same role rules as the API apply: members
  see and revoke only the keys they created, owners and admins see all. A freshly
  minted secret is shown once — store it then, it is never shown again.
- **Members**: everyone in the organization, with their roles.
- **Transaction log**: the newest ledger entries, newest first: position, id,
  description and content hash.

Notes:

- Amounts are in credits with six decimals; the ledger keeps them as integers.
- The dashboard needs the browser build: serve it with `cargo leptos serve` (or
  `cargo leptos build` once, then run the server). Without it the pages render but
  the live updates do not run.

## Chat in the browser

Purpose: top up, pick a model and chat without touching the API by hand, with each
turn's billing visible in real time.

Steps:

1. Log in at `/login` and open **Chat** in the dashboard (`/dashboard/chat`).
2. **Top up**: enter an amount in credits and submit. The page calls the top-up
   endpoint with your login session; keep the content hash it shows — it is what
   the top-up's bill verifies against later.
3. **Chat key**: the gateway takes an API key, not the login session, so the page
   needs one. **Mint a chat key** mints a key named `chat` and keeps it in this
   browser — you never have to handle it. (Or paste a key you minted under API
   keys.) The key lives in the browser's local storage: **Forget it** removes it
   from this browser; revoking it under API keys kills it everywhere.
4. **Model**: pick one of the models the deployment serves.
5. Type a message and send. The answer streams in.

Under each answer the turn's billing shows live: the frozen upper bound while the
answer streams, then the settled charge — amount, settlement kind, token counts and
the price version that priced it. **Verify this bill** opens `/verify` with the
turn's proof bundle and content hash prefilled, and the check runs there, in your
browser, as always.

Notes:

- The conversation is kept in the browser only; the server stores nothing. Reloading
  the page restores the conversation and re-reads each turn's bill.
- A 402 on send means the freeze does not fit your balance: top up, or pick a
  cheaper moment. The gateway's message states the freeze and the balance.

## Keys

Purpose: several integrations, one organization, revocable separately.

- `POST /api/v1/org/keys` mints another key, with an optional `name`, an optional RFC 3339 `expiresAt`, and an optional `spendLimitMinor`: the most the key may have committed — settled charges plus outstanding holds, in minor units. Null (the default) is unlimited. The secret comes back once.
- `GET /api/v1/org/keys` lists the organization's keys: id, name, display prefix, timestamps, and the spend limit. Never a secret.
- `PATCH /api/v1/org/keys/{keyId}` sets or clears the spend limit (`{"spendLimitMinor": 1000000}`, or `null` for unlimited), under the same role rules as revoking. A hold that would push the key past its limit is refused with 429 `KEY_LIMIT_EXCEEDED` — on the gateway too, where it looks like OpenAI's `insufficient_quota` — so concurrent requests cannot exceed it. A limit of 0 means the key can never hold.
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
   ledger: its entries are keyed `req-<id>:hold` and the settlement derived from it
   (`oxsum_core::settlement_key_for`).

Notes:

- The gateway freezes an upper bound *before* it contacts upstream, then charges upstream's reported
  usage, never more than the freeze. A 402 means the freeze does not fit in your balance: the
  message states the freeze and the balance, and lowering `max_tokens` lowers the freeze.
- `max_tokens` is what bounds the price, so it is always what upstream is told; a `max_tokens`
  larger than the model's configured maximum output is lowered to that maximum before the freeze
  is computed, so a request is never charged for more than the model can emit.
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
| `swept` | The hold timed out with no settlement (e.g. the gateway crashed) | Nothing; the whole freeze is released, recorded as an anomaly |

Every settlement also records which channel served the turn and which price version priced it
(`priceVersion` beside the prices and the token counts). A version is never rewritten, so a price
change affects later calls only: a bill written today can still be checked against the configuration
it was written under, however often the price changes afterwards.

### By hand

Purpose: reserve credit before the call, charge actual usage after it, refund the unspent rest.

Steps:

1. Before calling the LLM, `POST /api/v1/holds` to freeze an upper bound of what the call can cost.
   - A 402 `INSUFFICIENT_FUNDS` response means the balance is too low; do not start the call.
2. After the call finishes (success or failure), `POST /api/v1/settlements`:
   - `holdKey`: the `idempotencyKey` the hold was taken under
   - `actualMinor`: the actual spend; use 0 for a failed call and the whole hold is refunded

Notes:

- The settlement names the hold it releases; its own idempotency key is derived from the hold's.
- After a network timeout, retry with the same hold key and the same actual; you will not be charged twice.
- `actualMinor` must not exceed what the hold reserved.
- Naming a hold that was never taken, or one that is already settled, is refused (404, 409) — not free credit. Settle the hold you took, for the amount you took it for.
- A hold that is never settled does not stay frozen forever: the hold sweeper (issue #13) releases gateway holds older than `OXSUM_HOLD_TIMEOUT` (default 30 minutes) at 0 with settlement kind `swept`, recorded as an anomaly.

## Balance

Purpose: check the currently available balance.

Step: `GET /api/v1/balance`. The returned `availableMinor` already subtracts unsettled holds.

## Verifying a bill

Purpose: confirm a recorded transaction has not been altered since.

Steps:

1. `GET /api/v1/entries/{entryId}/proof` to fetch the proof bundle (the entry source, the inclusion proof and the tree head, as JSON).
2. Open `/verify` in the browser — no login needed — paste the bundle and the `contentHash` you saved, and submit. The check runs entirely in your browser: the page compiles the same `verify_bundle` the server runs to WebAssembly, so a passed check does not depend on trusting the server, and the bundle never leaves your machine.
3. Read the verdict. "Verification passed" means the bundle matches the content hash and the inclusion proof links it to the tree head. "Verification failed" means something changed: altering any single number in the bundle fails the check. A bundle that does not parse, or a hash that is not 64 hex characters, gets its own error instead.

Note: verification proves "this record has not been altered since it was written"; it cannot prove "the recorded number was correct in the first place".

## Verifying the log's history

Purpose: confirm the ledger you see today is the ledger you saw last week, with entries appended and nothing rewritten.

The operator signs the head of your organization's log. Save the signed head from `GET /api/v1/log/head` (the `note` text and the `size`/`root` it carries) alongside your records. Later, `GET /api/v1/log/consistency?from=<that size>` returns the new signed head and the proof between them. Check the note's signature against the operator's key (published at `GET /api/v1/log/key` — get the key through a channel the operator does not control the first time, or the signature proves only that the server agrees with itself), then check the proof against the two heads. It passes only when the new log is the old log with entries appended: a second history at a size the operator already signed for cannot produce it.
