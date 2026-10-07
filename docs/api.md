# API conventions

Field definitions for each endpoint are authoritative in `crates/server/openapi.yaml`; this file only records general conventions.

## Basics

- Prefix: `/api/v1`. `/healthz` needs no auth and sits outside the prefix; so do `POST /api/v1/auth/register`, when the deployment allows signup at all, and `POST /api/v1/invitations/redeem` — the invitation link's token is its credential.
- Auth: two credentials, one principal. `Authorization: Bearer oxs-…` is an organization's API key; the `oxsum_session` cookie is a logged-in user's session (`POST /api/v1/auth/login`, `HttpOnly`, `SameSite=Lax`, `Path=/`, plus `Secure` when the deployment sets `OXSUM_SESSION_COOKIE_SECURE`). Both resolve to the organization the request acts for, so no endpoint takes a tenant from the caller; a session additionally carries the acting user and their role. An explicitly presented bearer token is resolved as a key and never falls through to the cookie. Unknown, revoked and expired credentials are all `UNAUTHORIZED`, with no hint which it was. The cookie is never accepted on `/v1`: the gateway takes an API key only, presented as `Authorization: Bearer` or — the spelling an Anthropic SDK sends — `x-api-key`.
- Time: ISO 8601, UTC. The posting date is decided by the server.
- Money: integers in minor units, 1 credit = 1_000_000. Field names end in `Minor`.
- Field naming: camelCase for both requests and responses.
- Organization tenant ids: 32 lowercase hex characters, the organization's UUID without dashes. The ledger lives in the schema `ledger_<tenantId>`. Clients only ever read this value; they never send it.
- Idempotency keys: non-empty UTF-8 strings, at most 128 bytes.
- `/v1/*` is a second surface with the same key: a protocol-native gateway whose bodies and errors are the client's own — OpenAI's on `/v1/chat/completions`, Anthropic's on `/v1/messages`. It is documented in its own section below.
- `/api/v1/admin/*` is a third: the platform admin's surface, opened by `OXSUM_ADMIN_TOKEN` rather than by an API key. It answers in the same envelope and the same error codes as the rest of `/api/v1`, and is documented in its own section below.

## Idempotency

Every write to the ledger requires an `idempotencyKey` (holds, settlements, top-ups) — except the ones that derive their own: a settlement keys itself off its hold, and a redemption off the code (`redemption:<code_id>`), so neither takes the field. Writes that are not ledger entries — API keys, memberships, minted redemption-code batches — do not take one: creating a key twice creates two keys, and adding a member who is already one is `CONFLICT` rather than a replay.

- Replaying the same key with the same content returns the original receipt with `isNew: false`; nothing is booked twice.
- The same key with different content is `CONFLICT`; the original record is never overwritten. "Different content" means a different amount, a different kind of write, or any other field the entry is built from.

On timeout or network errors the client retries with the same key, never a new one.

## Holds and settlements

- A hold reserves part of the available balance. The settled balance is untouched until a settlement discharges the hold; the difference between the amount held and the amount charged goes back to the available balance.
- A settlement names the hold it releases (`holdKey`): the server reads the hold's amount from the hold entry in the ledger, so there is no amount to assert. One hold settles at most once — the settlement entry's idempotency key is derived from the hold's key (`oxsum_core::settlement_key_for`), and the ledger's idempotency gate refuses a second settlement of the same hold with `CONFLICT`, inside the append. Retrying the identical settlement replays it.
- Naming a hold that is not outstanding is `NOT_FOUND`, even when other holds would cover the amount. The wallet's limit still refuses a release the outstanding reservations cannot cover, as the backstop behind the pairing.
- A hold taken with an API key is checked against the key's spend limit when it has one: settled charges plus outstanding holds attributed to the key may not exceed `spendLimitMinor`. With a `budgetDuration` the limit is periodic: `daily`, `weekly` and `monthly` bound the UTC calendar day, ISO week and month respectively — settled charges inside the current window count, plus every outstanding hold whatever its age, because it still reserves money now. The check is serialized per key and atomic with the ledger append — concurrent holds cannot together exceed it — and the refusal is 429 `KEY_LIMIT_EXCEEDED`.
- Two more per-key protections run beside the spend limit: `requestsPerMinute` caps how many requests the key may start inside a rolling minute — consumed at admission on the gateway and on `POST /api/v1/holds` alike, refused 429 `RATE_LIMITED` with `Retry-After` and `X-RateLimit-Limit`/`X-RateLimit-Remaining`/`X-RateLimit-Reset` headers — and `maxConcurrentHolds` caps how many holds the key may have outstanding at once, counted from the ledger's pending entries under the same per-key lock, refused 429 `TOO_MANY_HOLDS`. A replayed request is answered before any of these checks run: a retry never consumes the key's windows twice.

## Funding (deposits)

Money arriving is one row on `oxsum.deposits` per rail payment, whatever the rail — `manual` today, `redemption` today, Stripe and a chain watcher when those rails land. `POST /api/v1/topups` is the manual rail: it takes `{"idempotencyKey", "amountMinor"}` and the key becomes the deposit's `payment_ref`, so a replayed top-up is the same row and the same entry.

- `POST /api/v1/redemptions` — redeem an operator-minted code. Body `{"code"}`; answers the credit's `Receipt` plus `amountMinor` (the code does not name what it carries). The redeem is one atomic claim: the code's row is taken `FOR UPDATE`, its deposit is recorded `confirmed` and the ledger credits the purchased pool under `redemption:<code_id>`, so a retried redeem — the caller's or a second in-flight call — replays the same credit and one code credits once ever. Unknown, spent and expired codes are all `NOT_FOUND`, with no hint which; a code this same organization already redeemed replays its own receipt (`isNew: false`).

## Statements

An organization that draws its credit line settles monthly (issue #124): the platform issues one statement per organization and UTC `YYYY-MM` period, itemized by channel and model, and repayments settle the oldest open statement first.

- `GET /api/v1/statements` — the organization's finalized statements, newest period first: `period`, `totalMinor` (the month's charges), `creditDrawnMinor` (what the line carried — what the statement is owed), `paidMinor`, `outstandingMinor`, `paymentStatus` (`pending`, `paid`, `overdue`, `suspended`), the `paymentTermsDays` snapshotted at issue, `dueDate`, and the ledger window (`logFromIndex`/`logToIndex`) the statement's lines prove. Drafts are the platform's working documents and never appear here.
- `GET /api/v1/statements/{statementId}` — one finalized statement with its `lines` (channel, model, turns, token totals, `amountMinor`). A draft or another organization's statement is `NOT_FOUND` — an id probes nothing.

Payments are not something the organization makes against a statement directly: any top-up or redemption repays the drawn credit line first, and the reconciliation that follows every money-in applies it to the oldest open statement — so paying off a bill is just funding the wallet.

## OpenAI-compatible gateway (`/v1`)

Point an OpenAI client's `base_url` at `/v1` and everything else stays the client's own.

- `GET /v1/models` lists the models this deployment serves — exactly those with a price. `owned_by` is the channel that serves one, and `created` is when the version in force was written.
- `POST /v1/chat/completions` relays one turn, streaming or not, and bills it — served by channels whose protocol is `openai`. Fields the gateway does not act on are forwarded upstream unchanged, so a client that works against the provider works here. `max_tokens` is set to the output upper bound the freeze was computed for, `max_completion_tokens` is dropped, and `stream_options.include_usage` is forced on for a stream. Three fields are recorded as the caller's attribution on the turn's usage record and forwarded unchanged — `user` (a string of at most 128 characters), `metadata` (at most ten string-valued pairs, keys and values at most 64 characters) and `service_tier` (a string of at most 64); anything past a bound is a 400 naming the field, because silently truncating an id would bill it under the wrong name.
- `POST /v1/messages` is the same turn in Anthropic's Messages shape — served by `anthropic` channels and answered in Anthropic's envelope, errors included (`{"type": "error", "error": {…}}`, with oxsum's code kept on `error.code`). `system` accepts a string or text blocks; a `text` content block contributes its `text`, a `tool_use`/`tool_result`/`thinking` block its serialized JSON, and a media block (`image`, `document`, `audio`, `video`) is refused 400 — the input bound must stay computable. `metadata.user_id` is the attribution field here. Upstream is called at `{baseUrl}/messages` with the channel credential as `x-api-key`; `anthropic-version` and `anthropic-beta` pass through, and `anthropic-version` defaults to `2023-06-01`. Usage normalization follows Anthropic's report: `input_tokens` plus `cache_read_input_tokens` plus `cache_creation_input_tokens` is the billed input, and the 5-minute/1-hour write tiers come from `cache_creation.ephemeral_*_input_tokens` (an unsplit lump counts as 5-minute). On a stream `message_start` carries the input side and `message_delta` the cumulative output, ending with `message_stop`. The cost headers, the `Idempotency-Key` semantics, and every refusal status are exactly `/v1/chat/completions`'s.
- Errors are OpenAI's `{"error": {"message", "type", "param", "code"}}`, with oxsum's own code in `error.code`. Upstream's error object is passed through verbatim on a 502.
- `Idempotency-Key` makes a retry the same turn: the first request claims the key, and a retry with the identical request body is answered from the record — a finished non-streamed turn replays its stored response, a finished streamed turn answers its settled receipt (`{"object": "oxsum.receipt", ...}` naming the request id, charge and usage), and either replay carries `Idempotent-Replayed: true` and the original `x-oxsum-request-id`. A retry while the first turn still runs is `409 IDEMPOTENCY_IN_FLIGHT`; the same key under a different body is `422 IDEMPOTENCY_MISMATCH`. A refusal that never reached the wallet releases the key, and every claim expires 24 hours after it was made.
- `x-oxsum-request-id` names a turn, and the two turn endpoints set it: `POST /v1/chat/completions` and `POST /v1/messages` each stamp it on every response they produce — a settled 200, a 400 for a body that is not JSON, any error they raise — because the id is minted before the body is read. `GET /v1/models` does not set it, and a 401 cannot: the auth middleware answers before any handler runs, so no id exists yet. The entries a turn wrote are `req-<id>:hold` and the settlement derived from it (`oxsum_core::settlement_key_for`), and `oxsum_core::entry_id_for` derives their ids, so the id in a header is enough to name the bill.
- Three cost headers answer what a request spent without a second call. Every answer that got as far as the hold — a completion or a post-hold upstream refusal — carries `x-oxsum-freeze-minor` (the reserved bound, the most the request can cost) and `x-oxsum-balance-minor` (the spendable balance left under that reservation, read right after the hold). The settled charge is `x-oxsum-charged-minor`: a header on a non-streamed answer and on a post-hold refusal, and an HTTP trailer on a streamed answer — the head is already out when a stream settles, and the trailer is the channel that leaves upstream's frames untouched. A client that does not read trailers still has the request id and the settled receipt.
- Text content only: an image or another content part is 400, because the freeze needs an input bound that cannot be undercounted.
- How each turn ends is recorded in the settlement entry's description as its `kind`; docs/user-guide.md has the table. The same description is a versioned billing credential — `"v": 3` today — carrying the `channel`, the `priceVersion`, the metered `usage` dimensions, the `matchedRule` when a conditional price hit, and the `lines` the charge is the capped sum of, each spelled `[item, units, pricePerMillion]` so the credential fits the ledger's description limit — so the charge can be recomputed from inside the entry's own content hash (`oxsum_verify::verify_charge`; docs/decisions.md, T1-2).
- A request is priced by the version in force when it starts. A price change during a turn reaches later requests only; the turn in flight settles at the version it began on.

## Platform admin (`/api/v1/admin`)

Whoever deploys oxsum, and nobody else: the credential is the operator token from `OXSUM_ADMIN_TOKEN`, sent as a bearer token, compared in constant time, and refused unless it is at least 16 characters when the server starts. An organization's API key does not open this surface, and this token does not open an organization's.

- `GET /api/v1/admin/channels` — the channels, each with the current version of every model it serves. Only the last four characters of an upstream credential are ever returned.
- `POST /api/v1/admin/channels` — create a channel (name, baseUrl, apiKey, optional `protocol`), or replace the connection of the channel that already has that name. `protocol` names the upstream wire protocol the channel speaks — `openai` or `anthropic` — which selects the adapter that normalizes its usage reports and the `/v1` surface its models are served on; it defaults to `openai`, and any other name is `VALIDATION_ERROR`, because a channel that cannot normalize its own usage reports cannot serve. Prices are untouched by a connection change.
- `POST /api/v1/admin/channels/{channelName}/prices` — append a price version for a model, and answer the version that was written. The body is the whole price set: `inputPricePerMillion`, `outputPricePerMillion` and `maxOutputTokens` are required; `cacheReadPricePerMillion`, `cacheWrite5mPricePerMillion`, `cacheWrite1hPricePerMillion`, `reasoningPricePerMillion`, `costPerRequest`, `mode` and an `upstream` cost set are optional dimensions, and `rules` lists conditional sets — a `match` (`serviceTier`, `minInputTokens`, `maxInputTokens`) swaps in the rule's whole set, most-specific-wins. `VALIDATION_ERROR` for a malformed rule or two rules that could match the same request at the same specificity; `CONFLICT` when another channel already serves that model: in v1 one model belongs to one channel.
- `GET /api/v1/admin/channels/{channelName}/prices` — every version of every model of that channel, newest first. This is the history, and it is what makes the `priceVersion` in an old settlement checkable.
- `GET /api/v1/admin/organizations` — every organization, oldest first, with its kind, headcount and what its wallet shows (`availableMinor`, `reservedMinor`), plus the credit line it carries (`creditLimitMinor`, `creditUsedMinor`).
- `PATCH /api/v1/admin/organizations/{organizationId}` — update the organization's billing terms: `{"creditLimitMinor"?, "paymentTermsDays"?, "idempotencyKey"}`, at least one of the two. The limit is spendable credit a hold may draw after granted and purchased credit run out; `availableMinor` counts undrawn credit, and `GET /api/v1/balance` answers `creditLimitMinor`/`creditUsedMinor` beside the pools' sums. Shrinking the limit below the outstanding draw is `VALIDATION_ERROR` — the repayment is a top-up, which credits the drawn line before the purchased pool. `0` is allowed once the line is repaid. `paymentTermsDays` is how long a finalized statement's payment has before it falls due, snapshotted onto each statement at issue. `NOT_FOUND` for an organization that does not exist. Replaying the idempotency key answers the original result; a credit-limit change itself is one ledger entry — the whole history of a limit is inside the verifiable log.
- `POST /api/v1/admin/organizations/{organizationId}/adjustments` — book a signed amount into the organization's wallet: `{"amountMinor", "reason", "idempotencyKey"}`. Positive grants, negative deducts; the reason is required and becomes the entry's description. `VALIDATION_ERROR` for a zero amount or an empty reason, `NOT_FOUND` for an organization that does not exist, `INSUFFICIENT_FUNDS` for a deduction the balance cannot carry — the ledger's no-overdraft rule, not a read-then-write check. Replaying the idempotency key answers the same entry rather than writing a second.
- `GET /api/v1/admin/holds` — every unsettled hold across all organizations, newest first: the in-flight requests the platform is holding money for, read off the sweeper's watch table. Each row also carries the sweep bookkeeping — `sweepAttempts`, `lastError`, `deadAt` — so a hold whose settlement keeps failing is visibly dead-lettered rather than silently stuck.
- `POST /api/v1/admin/redemption-codes` — mint a batch of redemption codes: `{"count", "amountMinor", "expiresAt"?}` (`count` ≤ 1000, `expiresAt` RFC 3339 and in the future, absent means the codes never expire). Answers `{batchId, count, amountMinor, expiresAt, codes}` — the only place the codes are readable: the table keeps SHA-256 only. `VALIDATION_ERROR` for a bad count, a non-positive amount or a past expiry.
- `GET /api/v1/admin/anomalies` — the settled turns that did not price cleanly (`capped`, `estimated`, `client_cancelled`, `swept`, `unpriced`), across all organizations and newest first. The rows are read back out of each organization's own ledger — the same settlement records a bill proves — so channel, model, price version, token counts, charge and freeze are exactly what was written; a settlement recorded without a settlement record (a hand-driven `Wallet::settle`) does not appear.
- `GET /api/v1/admin/margin` — what the platform charged versus what upstream cost it, summed per `channel` and `model` over the usage records: `turns`, `chargedMinor`, `upstreamCostMinor`, `marginMinor` (the difference) and `untrackedTurns`. `untrackedTurns` counts the rows whose upstream cost is NULL — a price with no `upstream` block, a swept turn, or history predating the column — which is deliberately not zero: a coverage gap shows up there instead of inflating the margin. Operator-only; upstream cost never reaches an organization-facing endpoint.
- `GET /api/v1/admin/reconciliation` — the drift between the ledgers and the tables that project them, answered as `{checkedAt, organizations, clean, classes}`: one entry per drift class (`usage_orphans`, `settlements_unrecorded`, `deposits_unbooked`, `deposits_mismatched`, `deposits_stuck`, `watches_orphaned`, `holds_unwatched`, `holds_dead_lettered`, `log_gaps`), each with a `count` and a bounded `samples` list of `{organization, detail}` identifiers. The scan is read-only — drift is reported for an operator to act on, never patched in place.
- `GET /api/v1/admin/closings` — every sealed month across all organizations, newest first: the closing records. A seal names the period, how many entries it covers, the log's tree head at sealing, the closing trial-balance root and the seal's own hash, which chains onto the seal before it.
- `POST /api/v1/admin/closings` — close a month. Body `{"month": "YYYY-MM"}`; the month must have fully ended (a month still containing today answers `400`). Seals the period in every organization's ledger and answers each closing record. Idempotent: closing an already-sealed month answers the record it holds. Once sealed, no entry may carry a booking date in that month again — the ledger's sealed watermark refuses it at seal time.
- `GET /api/v1/admin/statements` — every statement the platform has generated, newest period first, drafts and finalized alike; `?period=` and `?organizationId=` narrow it. A malformed or still-running period is `VALIDATION_ERROR`, so a mistyped filter does not look like "no statements". The lazy overdue flip runs first, so a pending statement past its due date already answers `overdue`.
- `POST /api/v1/admin/statements` — generate the period's draft statements: body `{"period": "YYYY-MM"}`, plus `organizationId` to bill one organization only. A month still running or malformed is `VALIDATION_ERROR`. Generation is idempotent per organization and period: a draft rebuilds its lines from the usage rows each call, a finalized statement stands, and an organization with no usage in the period produces none.
- `GET /api/v1/admin/statements/{statementId}` — one statement with its itemized lines, in whichever standing it holds; `NOT_FOUND` for an unknown id.
- `POST /api/v1/admin/statements/{statementId}/finalize` — issue a draft: locks the lines and totals, snapshots the organization's `paymentTermsDays` into `dueDate`, and pins the ledger window (`logFromIndex`/`logToIndex`) the lines prove. Idempotent — an already-final statement answers itself. The body carries `{"idempotencyKey"}` for uniformity; the transition derives its own idempotency.
- `POST /api/v1/admin/statements/{statementId}/payments` — record a payment: `{"amountMinor", "idempotencyKey"}`. The amount books into the organization's ledger (`debit Cash`), repaying the drawn credit line first — and the statement book reconciles oldest-first from there. The amount may reach as far as the credit-line debt outstanding through the statement's period — earlier unbilled draws take their share — and no further (`400`). A draft refuses (`400`: it is not issued), and a paid statement refuses too — there is nothing left to take. The key names the ledger entry: a retried payment on a still-open statement replays the receipt; the same key under a different amount is `409`.
- `POST /api/v1/admin/statements/{statementId}/suspend` — mark an unpaid finalized statement `suspended` (`{"idempotencyKey"}`): the standing for a bill unpaid past grace. Suspension is a standing, not a refusal of money — a later payment still settles it to `paid`. A draft or a paid statement is `400`; suspending an already-suspended one answers the standing document.
- `POST /api/v1/admin/users/{userId}/password-reset` — set a new password for a user (`{"newPassword"}`, same length rule as signup) and revoke every session they hold, in one transaction. Answers `{"userId", "sessionsRevoked"}`; an unknown user is `NOT_FOUND`. This is the only password-recovery path v1 has — no email is sent.

- `GET /metrics` — the process's telemetry in Prometheus exposition format: HTTP request counts and durations by route pattern, the gateway's money path (holds taken, settlements by kind, charged minor units, settled tokens, upstream response latency), rate-limit rejections, idempotency claim outcomes, and scrape-time gauges for open holds and the connection pool. It sits outside the `/api/v1` prefix like `/healthz` but carries the same operator token: a scrape config sends it as `bearer_token`.

A deployment that sets no `OXSUM_ADMIN_TOKEN` has no admin surface: the routes exist and answer `UNAUTHORIZED`, rather than being open or absent — `/metrics` included.

## Tree heads (`/api/v1/log`)

Each organization's ledger is its own append-only Merkle log, and the operator signs its head. A user holding an old head can verify the new head was appended onto it — the signature, then a consistency proof:

- `GET /api/v1/log/head` — the current head, signed: a C2SP signed-note (`note`: body, blank line, signature lines), the `origin` (`oxsum/ledgers/<tenant_id>`), the structured head (`size`, lowercase-hex `root`), and the signing key (`keyName`, base64 `publicKey`, the `keyHash` selector naming the operator's signature line). The note text is what the signature covers, byte for byte: verify what you read, not what you re-render.
- `GET /api/v1/log/consistency?from=<size>` — the signed new head, the old head at `from` recomputed from the log, and the `proof` (`oldSize`, `newSize`, lowercase-hex `path`) between them. `from` must be at least 1: every log extends the empty tree, so a proof from size 0 would verify against any history and is `VALIDATION_ERROR` rather than answered; `from` beyond the log is `VALIDATION_ERROR` too.
- `GET /api/v1/log/key` — the operator's verifying key. No credential: this is a public key. Fetching it from the server is convenience — a verifier must have chosen the key through a channel the operator does not control, or the signature proves only that the server agrees with itself.

The key signs under the fixed name `oxsum/tree-heads`; the seed is `OXSUM_HEAD_SIGNING_KEY` (32 bytes, base64). A deployment that sets none serves the wallet but not the log surface: those routes answer `SERVICE_UNAVAILABLE`.

## Response format

Success:

```json
{ "data": {} }
```

Failure:

```json
{ "error": { "code": "VALIDATION_ERROR", "message": "caller-facing explanation", "details": [] } }
```

Every failure on the envelope's surface is this shape, including a request body the server cannot read: a missing or wrong `content-type`, a body that is not JSON, and JSON that does not match the endpoint's request type are all `VALIDATION_ERROR`, which is the 400 every body-taking path documents. `/v1` is the exception, and its own section says so.

## Error codes

| code | HTTP | meaning |
| --- | --- | --- |
| `VALIDATION_ERROR` | 400 | request validation failed |
| `UNAUTHORIZED` | 401 | credential missing, malformed, unknown, revoked or expired — key, operator token or session alike — or a login with an unknown email or a wrong password |
| `FORBIDDEN` | 403 | the caller may not do this; registration when signup is not open |
| `INSUFFICIENT_FUNDS` | 402 | the wallet cannot cover it: a hold larger than the available balance |
| `KEY_LIMIT_EXCEEDED` | 429 | the acting API key's spend limit is exhausted: settled charges plus outstanding holds attributed to the key would exceed it |
| `RATE_LIMITED` | 429 | the acting API key's rolling-minute request allowance is used up; `Retry-After` and `X-RateLimit-*` headers say when a slot frees |
| `TOO_MANY_HOLDS` | 429 | the acting API key's outstanding-holds cap is reached: settle or let a hold lapse before opening another |
| `NOT_FOUND` | 404 | resource does not exist, or belongs to another organization; a settlement naming a hold that is not outstanding |
| `CONFLICT` | 409 | the value is already taken, or a key was reused for a different request; registering an email that exists; settling a hold that is already discharged |
| `INTERNAL_ERROR` | 500 | server error; details only in logs |
| `SERVICE_UNAVAILABLE` | 503 | a feature the deployment did not configure: the wallet works, this surface does not — today, the tree-head endpoints without `OXSUM_HEAD_SIGNING_KEY` |

## API keys

- Format: `oxs-` followed by 32 random bytes, so secret scanners can recognize one. The plaintext is returned once, at creation; the database stores only its SHA-256 hash and a display prefix.
- Keys belong to an organization and hold no money. `created_by` records the user who minted one, for the role rules product.md defines: a key minted through a session records its creator, a key minted with an API key records none.
- Members see and revoke only the keys they created; owners and admins see and revoke every key of the organization. A key that exists but is not the caller's answers `NOT_FOUND`, never `FORBIDDEN`, so ids cannot be probed. An API key acting as the organization keeps its full authority: roles constrain sessions, not keys.
- Revocation is permanent and idempotent; a key id of another organization is `NOT_FOUND`, never `FORBIDDEN`, so ids cannot be probed.
- Expiry is optional. An expired key is refused exactly like a revoked one.
- Since web login (TODO B-4, issue #17), role checks are enforced: members create and revoke only the keys they created; owners and admins see and revoke every key of the organization. A key acting as the organization keeps its full authority — roles constrain sessions, not keys.
- A key may carry a spend limit (`spendLimitMinor`, minor units; null is unlimited), set at creation or with `PATCH /api/v1/org/keys/{keyId}` under the revoke scope rules. The limit caps the key's committed spend — settled charges plus outstanding holds attributed to it. A hold that would push past it is refused with 429 `KEY_LIMIT_EXCEEDED`, atomically with the ledger append, so concurrent requests cannot exceed it. The gateway answers the same refusal as `insufficient_quota` in its OpenAI error shape. Sessions carry no limit.
- `budgetDuration` (`"daily"`, `"weekly"`, `"monthly"`, or null) makes the limit periodic instead of cumulative, over the UTC calendar day, the ISO week (starting Monday) or the calendar month. Settled charges count only when their booking date falls in the current window; outstanding holds always count, because a reservation still binds money now. A window without a limit is meaningless and refused with `VALIDATION_ERROR`.
- `modelAllowlist` (array of model names, or null for all) restricts which gateway models the key may call. A `/v1` request naming another model is refused with 403 (`FORBIDDEN` in the oxsum envelope, `permission_error` in the OpenAI shape) before anything is frozen or sent upstream. `POST /api/v1/holds` names no model and is not governed by the list.
- `requestsPerMinute` (integer ≥ 1, or null for uncapped) bounds the requests the key may start inside a rolling minute — on the gateway and on `POST /api/v1/holds`. Past it the refusal is 429 `RATE_LIMITED` carrying `Retry-After` and the `X-RateLimit-*` headers; on the gateway the same refusal arrives with the `rate_limit_exceeded` type in the OpenAI shape.
- `maxConcurrentHolds` (integer ≥ 1, or null for uncapped) bounds the holds the key may have outstanding at once. A hold over it is refused 429 `TOO_MANY_HOLDS` — on the gateway the `rate_limit_exceeded` type carries the same code — and a settlement frees the slot it took, because the count is the ledger's own pending entries.
- A `PATCH` to `/api/v1/org/keys/{keyId}` replaces the whole constraint set — `spendLimitMinor`, `budgetDuration`, `modelAllowlist`, `requestsPerMinute`, `maxConcurrentHolds` — under the revoke scope rules; each field clears with `null`, and an omitted field behaves the same as `null`.

## Organizations of a user

A user belongs to one organization per membership, and a session acts as exactly one of them — the one the session row names, the oldest membership until it is switched. These three endpoints are a person's actions: they take the session cookie only and refuse a bearer API key, because a key belongs to one organization and names no user — the same rule membership management follows.

- `GET /api/v1/orgs` — every organization the session's user is a member of, oldest membership first, each as `{"organization": Organization, "role": "owner"|"admin"|"member"}`. This is the list the dashboard's switcher offers.
- `POST /api/v1/orgs` — create a `team` organization. Body `{"name": "…"}`, trimmed, 1–80 characters; the user becomes its owner. There is no `personal` kind to create — a personal organization is exactly what signup makes. The session keeps acting as the organization it had.
- `POST /api/v1/session/organization` — switch the acting organization. Body `{"organizationId": "…"}` naming a membership; anything else is `NOT_FOUND`, so the answer never says whether an organization exists. The session row is updated: every request after this one, and every reload, acts as the chosen organization. Answers the `SessionInfo` now in force.

## Membership management

A tenant is an organization and its memberships carry a role (owner, admin, member). Managing them is a *person's* action: these four endpoints require the session cookie and refuse a bearer API key, because a key is not a person and names no role. This is the one place a key does not act with the organization's full authority (docs/decisions.md, "membership management is a person's action"). The rules are enforced in `crates/core/src/orgs.rs`, so the members page's server functions refuse exactly what these endpoints refuse.

- `POST /api/v1/org/members` — add an existing account to the organization as a member. Body `{"email": "…"}`, compared case-insensitively against `users.email_normalized`; the new role is always `member`. Answers the `Member` (`userId`, `email`, `role`, `joinedAt`). An unknown email is `NOT_FOUND`: a person without an account is invited by link instead.
- `POST /api/v1/org/invitations` — mint an invitation link into the organization. Answers `{"id", "token", "expiresAt"}`; the token (`oxi-` plus 32 random bytes) is answered once, here, and only its SHA-256 hash is stored. The link lives seven days and registers exactly one account. Owner-or-admin, session-only — the same rule as every membership write.
- `POST /api/v1/invitations/redeem` — register through a link. Public: the token is the credential. Body `{"token", "email", "password"}`; answers the `Registration` (user, organization, first `apiKey`), the account landing in the invitation's organization as a `member` with no personal organization. Unknown, spent and expired tokens are all `NOT_FOUND`; a taken email is `CONFLICT` and leaves the link unspent, as does a rejected email or password (`VALIDATION_ERROR` is answered before the link is touched).
- `PATCH /api/v1/org/members/{userId}` — change a member's role. Body `{"role": "admin"|"member"}`; `owner` is not a value of that field, so no request body can promote anyone to owner. Answers the updated `Member`.
- `DELETE /api/v1/org/members/{userId}` — remove the membership. The user and their personal organization are untouched. Answers the `Member` that was removed.
- `POST /api/v1/org/ownership` — transfer ownership. Body `{"userId": "…"}`. The named member becomes `owner` and the transferring owner becomes an `admin`, in one transaction, so the organization has exactly one owner before and after. Answers `{"owner": Member, "previousOwner": Member}`.

### Who may do what

| Acting credential | add | remove | change role | transfer ownership |
| --- | --- | --- | --- | --- |
| Session, `owner` | yes | yes, except the last owner | yes, except the last owner | yes |
| Session, `admin` | yes | only members and admins | only members and admins | no, 403 |
| Session, `member` | no, 403 | no, 403 | no, 403 | no, 403 |
| API key (bearer) | no, 403 | no, 403 | no, 403 | no, 403 |

- **The last owner is protected.** An organization always has at least one owner: the sole owner cannot be removed or demoted (`CONFLICT`), whichever endpoint is asked, so ownership has to be transferred first. `POST /api/v1/org/ownership` moves the seat — it never creates a second owner — and the previous owner stays an admin, so they can still manage members.
- **An admin may not touch an owner.** An admin removing or re-rolling an owner is `FORBIDDEN`, not `CONFLICT`: the refusal is about the admin's authority, not about the owner count.
- **No role change grants ownership.** The only path to `owner` is the ownership endpoint, so an admin cannot promote anyone (including themselves) to owner by any route.
- **The acting role is checked before the target is looked at.** A caller who may not perform an action is refused `FORBIDDEN` whatever they name, so an admin's ownership transfer naming an account that is not even a member answers 403, not 404. Within the allowed roles, naming a non-member is 404.
- **Any member may read the list.** Reading who is in the organization is not a management action, and the members page's `get_members` server function (`crates/web/src/api.rs`) is reachable by any live session; the session resolves through `memberships`, so a session can only ever read the list of an organization it is a member of (docs/decisions.md, issue #61).
- Adding the second person to a `personal` organization flips its `kind` to `team`; the ledger does not move and nothing flips back when a member is removed.

### Statuses

| situation | status | code |
| --- | --- | --- |
| not a session in the owner or admin role (member, or any API key) | 403 | `FORBIDDEN` |
| an admin acting on an owner | 403 | `FORBIDDEN` |
| an admin transferring ownership | 403 | `FORBIDDEN` |
| unknown account (the email names nobody) | 404 | `NOT_FOUND` |
| target is not a member of the caller's organization | 404 | `NOT_FOUND` |
| the add email is already a member | 409 | `CONFLICT` |
| the last owner cannot be removed or demoted | 409 | `CONFLICT` |
| ownership transferred to the current owner | 409 | `CONFLICT` |
| a body naming `owner`, or one that is not the request type | 400 | `VALIDATION_ERROR` |

## Pagination

`GET /api/v1/admin/organizations` paginates (issue #93): `?limit=` caps a page at 100 rows (the default), and a page that is not the last answers `nextCursor` — an opaque string the next call echoes back as `?cursor=`; absent means the list is done. A cursor is a keyset bound, never a page/offset: a malformed one answers `400 VALIDATION_ERROR`. The dashboard's own lists paginate the same way — the bills, transaction-log and requests pages walk their ledgers with the log index as `?before=` — and bounded lists (`GET /api/v1/org/keys`, holds, anomalies, closings) answer in full.

## Change process

1. Change `crates/server/openapi.yaml`
2. Change the backend implementation, add integration tests
3. Change the callers (Leptos pages, browser verification component)
