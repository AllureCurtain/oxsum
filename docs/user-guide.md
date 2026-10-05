# User guide

For callers integrating with oxsum; no technical internals. Field details live in `crates/server/openapi.yaml`. Every API request except `GET /healthz`, `POST /api/v1/auth/register`, `POST /api/v1/auth/login` and `POST /api/v1/auth/logout` carries a credential — `Authorization: Bearer <api-key>` or the session cookie — and the credential decides which organization the request acts for — no request names a tenant.

## Get an account and a key

Purpose: get a user, an organization to hold credits, and a credential to call the API with.

Steps:

1. `POST /api/v1/auth/register` with `email`, `password` (at least 12 characters) and optionally `organizationName`.
   - The response carries `user`, `organization` and `apiKey.secret`. **The secret is shown once and cannot be retrieved again**; store it now.
   - A personal organization is created with you as its owner, named after the address's local part unless you chose a name.
   - An existing address answers 409 `CONFLICT`.
   - Some deployments answer 403 `FORBIDDEN`: registration is by invitation there. An
     owner or admin of the organization hands you a link (`/register?invite=…`); it
     registers one account, expires seven days after it was minted, and cannot be
     reused. The same page also serves `open` deployments for invitees who prefer it.
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
- **Members**: everyone in the organization, with their role and join date. Every member
  sees the list. Owners and admins also manage it, from the controls on each row:
  - **Add member**: enter the email of an account that already exists. The page says
    so when no account has that email — for someone new, use the next control instead.
    The new member starts as a member.
  - **Invite by link**: mint a link for someone who has no account yet. Copy it and
    hand it over yourself — v1 sends no email. The link registers one account into the
    organization as a member, expires seven days after minting, and is shown once.
  - **Make admin** / **Make member**: change a member's role. This never grants
    ownership; see below.
  - **Remove**: take the membership away. The person's account and their own personal
    organization stay, and if they were logged in, that session stops working.
  - **Make owner**: transfer ownership. You become an admin and the member you chose
    becomes the owner, in one step — the organization always has exactly one owner.
    Only an owner sees this control.
  - What you are not offered, you cannot do: an admin has no controls on the owner's own
    row, and nobody can remove or demote the last owner — transfer ownership first. A
    refused action says why, in words.
- **Transaction log**: the ledger entries, newest first: position, id,
  description and content hash — a hundred at a time, with **Older** and **Newest**
  links under the table walking further back.
- **Bills**: the organization's transactions, newest first, at `/dashboard/bills`:
  top-ups, adjustments and settled requests, each with the date it was booked, its
  kind, what it was (a settled request shows its request id, model, token counts,
  freeze and settlement kind; a top-up or an adjustment shows its reason), the signed
  amount in credits — money in reads `+`, a charge reads `-` — and the content hash
  its proof verifies against. A hold that has not settled bills nothing yet and is
  not listed. A member sees the transactions their own keys paid plus the shared
  organization history; owners and admins see everything. The list pages a hundred
  at a time — **Older** and **Newest** under the table — and the position lives in
  the URL, so an older page is a link.
- **Verify a row**: every row's **verify** link opens `/verify` with the entry named;
  the page fetches that entry's proof bundle itself — inside your session and its
  scope — and runs the check on load.
- **Export**: **Download CSV** and **Download JSON** on the bills page save the
  organization's whole history as a file — every transaction, not just the page on
  screen. Both carry the same fields — `bookedOn`,
  `entryId`, `kind`, `amountMinor`, `description`, `contentHash`, plus the settled
  turn's parsed `request` in the JSON — so an archived record can be checked against
  the ledger later; the amount is the ledger's signed integer in minor units in both
  files, where the page shows the same amount in credits. The server sends the file with
  `Content-Disposition: attachment`, so a browser saves it instead of opening it, and the
  same two URLs work with `curl` and your session cookie as well.
- **Append-only archive**: the page keeps the organization's signed tree head in your
  browser's `localStorage` — automatically, nothing to do. Every visit verifies the
  operator's signature on the head the server serves; when the log has grown, the page
  fetches the consistency proof itself and checks that today's ledger still contains
  the head it archived, and a one-line status above the table says so in words. If the
  history ever stopped extending what was archived, the page would say the archive
  check failed rather than show a verdict — and the stored head is never overwritten
  on a failed check, so the discrepancy stays detectable. (Requires the deployment to
  sign heads — `OXSUM_HEAD_SIGNING_KEY`; without it the line says the check is
  unavailable.)
- **Requests**: the organization's settled requests, newest first
  (`/dashboard/requests`): when the settlement was booked, the request id (the
  `x-oxsum-request-id` the gateway answered with), the model, the key that paid, the
  status — how the turn was priced, in the bill's own words: `usage`, `estimated`,
  `capped`, `client_cancelled`, `upstream_error`, `upstream_unreachable`, `swept`
  or `unpriced` —
  and the input and output tokens it used and what it charged in credits. The
  filters live in the URL: `/dashboard/requests?key=<prefix>&model=<name>`. Type a
  key's prefix (the keys page shows it) or a model name and press **Filter**, or click
  a key or a model in a row to filter by that value and keep the other filter. A
  filtered view is a link: it survives a reload and can be shared, and a filter that
  matches nothing shows an empty table with a message rather than an error. The list
  pages a hundred at a time — **Older** and **Newest** under the table, keeping the
  filters in the link. A turn that is still in flight has not settled yet, so it is
  not here — the overview shows those live.

- **Organization**: the top-right corner of every dashboard page names the
  organization the session acts as. Choosing another one from the select switches it —
  the page reloads, and every page then shows that organization's balance, keys,
  members, bills and requests. **New** opens a small form that creates a team
  organization with you as its owner; it joins the list, and switching to it is one
  more selection. A fresh login acts as your oldest membership until you choose
  otherwise, and the choice is stored with the session: it survives reloads and new
  tabs, and another browser's session is not moved.

Notes:

- Amounts are in credits with six decimals on every page, the bills and requests pages
  included; the ledger keeps them as integers, and the CSV and JSON exports carry the
  ledger's own integers in minor units (`amountMinor`, signed, where 1 credit = 1,000,000).
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
- `user`, `metadata` (an object of string-valued pairs) and `service_tier` may ride a request for
  attribution — which of your own users spent, tagged your way. They are recorded on the turn's
  usage record and forwarded upstream unchanged; they never change the price. The bounds are a
  128-character `user`, ten metadata pairs of 64-character keys and values, and a 64-character
  `service_tier`: anything past a bound is refused, because a silently truncated id would bill
  under the wrong name.
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
| `unpriced` | Upstream's usage named a dimension the price book cannot bill — a tool call, a media token, a foreign event kind | The part the book covers, flagged as an anomaly; the rest is never billed at zero |

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
3. Read the verdict. "Verification passed" means the bundle matches the content hash and the inclusion proof links it to the tree head — and, for a usage settlement, one more line: the charge inside the entry is recomputed from the usage and rates it records (`charged = ceil(Σ units × pricePerM ÷ 1,000,000)`, capped at the freeze). A settlement written before descriptions were versioned, or by a schema newer than the page's verifier, reports that inclusion passed but the arithmetic is too old or too new to recompute — it is skipped, not guessed at. "The charge does not add up" means the entry is proven but its own numbers disagree with each other, which an honest settlement never does. "Verification failed" means something changed: altering any single number in the bundle fails the check. A bundle that does not parse, or a hash that is not 64 hex characters, gets its own error instead.

Note: verification proves "this record has not been altered since it was written", and for a versioned settlement that its charge agrees with the usage and rates it records; it cannot prove "upstream really returned that many tokens".

## Verifying the log's history

Purpose: confirm the ledger you see today is the ledger you saw last week, with entries appended and nothing rewritten.

The bills page does this check automatically on every visit (see "Append-only archive" above): it archives the signed head locally and verifies the consistency proof in the browser. What follows is the same check done by hand — useful for an independent audit, or for keeping the archive somewhere other than the browser.

The operator signs the head of your organization's log. Save the signed head from `GET /api/v1/log/head` (the `note` text and the `size`/`root` it carries) alongside your records. Later, `GET /api/v1/log/consistency?from=<that size>` returns the new signed head and the proof between them. Check the note's signature against the operator's key (published at `GET /api/v1/log/key` — get the key through a channel the operator does not control the first time, or the signature proves only that the server agrees with itself), then check the proof against the two heads. It passes only when the new log is the old log with entries appended: a second history at a size the operator already signed for cannot produce it.

## Platform admin

Purpose: manage the deployment itself — the upstream channels the gateway relays to and the price each model charges.

The admin pages are not part of the dashboard: they open with the deployment's operator token (`OXSUM_ADMIN_TOKEN`), not with a login, and they are reachable only in a deployment that configured one.

Steps:

1. Open `/admin` in the browser and enter the operator token. The page proves it against the real surface before remembering it — a refused token is reported, not stored — and keeps it in the browser's `localStorage`, the same place the chat page's key lives. A `401` on any later call forgets it and asks again.
2. The channels and prices page (`/admin/channels`) lists every channel: its name, upstream address, the last four characters of its upstream key (the only part ever read back), and the current version of each model's price — input and output per million tokens, and the output cap a freeze is sized by.
3. "Add or repoint a channel" writes a channel: a name that already exists is repointed to the new address and key, and its price history stays where it is.
4. "Append price" on a channel's card appends a new version for one model. Prices are append-only — a change is `v{n+1}`, never a rewrite — and "Show history" reads every version the channel has ever had, newest first. That history is what a bill's `priceVersion` is checked against later. The API can say more than the form today: a price set may also carry cached-input, cache-write (5m and 1h) and reasoning rates, a flat per-request fee, the upstream's own prices, and conditional `rules` — a `match` on `serviceTier` or an input-token window swaps in a whole different set, most-specific-wins; see `PriceRequest` in `openapi.yaml`.

Note: a request resolves its price when it starts and carries the version to its settlement, so a price change lands on later requests only; in-flight turns and old bills are untouched.

The organizations page (`/admin/organizations`) lists every organization the ledger holds money for — its name, kind (personal or team), headcount, and what its wallet shows: the available balance and the frozen sum of its outstanding holds. Each row carries an "Adjust" form: a signed amount in credits (a leading `-` deducts) and a required reason, which the new entry stores as its description — part of what a bill's proof covers. A deduction the balance cannot carry is refused with `INSUFFICIENT_FUNDS`; nothing in the page ever rewrites history.

A deployment can also start each new organization's wallet with credits: `OXSUM_SIGNUP_BONUS_MINOR` (default 0) grants that amount at self-registration, booked as an adjustment whose reason is "signup bonus". An invited account joins an existing organization and is granted nothing.

A forgotten password is recovered by the operator, not by the user — v1 sends no email, so there is no self-service reset. `POST /api/v1/admin/users/{userId}/password-reset` with `{"newPassword": "…"}` sets the replacement and revokes every session the user held at once; the answer's `sessionsRevoked` says how many died with it.

The in-flight page (`/admin/in-flight`) lists every hold the platform is currently reserving, across all organizations and newest first — the organization, the request id, the model and channel, the price version the turn froze at, the frozen amount and when it opened. A hold that settled leaves the list; one older than the hold timeout (30 minutes) is one the sweeper is about to release as `swept`.

The anomalies page (`/admin/anomalies`) reviews the settled turns that did not price cleanly, across all organizations and newest first: an `estimated` turn where upstream reported no usage, a `client_cancelled` one where the caller left mid-stream, a `capped` one whose upstream usage priced above the freeze, a `swept` one whose hold the sweeper had to release, and an `unpriced` one whose usage named a dimension the price book cannot bill — the computable part was charged and the rest is the platform's flagged loss, never a silent zero. Every row is the settlement record that turn's own ledger holds — organization, request id, model, channel, the price version in force, the charge and what had been frozen. A summary table on top counts the anomalous turns and their charged total per channel: where the platform is losing money upstream is the first thing the page answers.

The closing page (`/admin/closing`) runs the monthly closing: pick a month that has fully ended and close it — the month is sealed in every organization's ledger, which produces its closing record (a seal committing to the log's tree head and the month's closing trial balance, chained onto the seal before it) and refuses any new entry dated into it from then on. The table lists the sealed months newest first: organization, period, how many entries it covers, the log's size at sealing, and the three hashes the record is made of. Closing a month twice is safe — the second run answers the records that already exist.
