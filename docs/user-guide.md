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
   - Deployments that run the anti-bot check draw a small Turnstile widget on the
     register page — solve it once and submit; a missing or expired answer answers
     `FORBIDDEN`, so redo the widget and resend. Send its token as `turnstileToken`
     when calling the API directly.
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

- When the deployment configures GitHub OAuth, the login page also shows **Continue with GitHub**: it takes you to GitHub and back, signs you into the account your verified GitHub email belongs to — or registers you when the deployment allows open signup — and the session that lands is the same `oxsum_session` cookie a password login mints. An account created this way has no password until you set one through the reset flow. The page learns whether the button exists from `GET /api/v1/auth/methods`, the one unauthenticated read the auth surface offers.
- The session expires thirty days after login, and it stops working the moment your membership in the organization is gone.
- When the deployment sends email, registration and an invitation's redemption mail a verification link; the dashboard shows an unverified address a reminder with a resend (a minute between mails), and the link verifies the address for good. `GET /api/v1/session` reports the state as `user.emailVerified`. A deployment without a mailer skips all of it — nothing is mailed, nothing is reminded.
- Every `/api/v1` endpoint accepts the cookie wherever it accepts a Bearer key; the gateway (`/v1`) takes an API key only.
- Members see and revoke only the keys they created; owners and admins see and revoke every key of the organization. Keys you mint while logged in record you as their creator.
- Deleting the account: `DELETE /api/v1/users/me` with `{"password": "…"}` — the session cookie plus your password, since the act is irreversible. Your email is tombstoned, every session and every key you minted dies at once, your memberships go, and the `endUser`/`tags` attribution on usage your keys paid is erased. Your personal organization is suspended and renamed rather than deleted — the ledger under it cannot be orphaned — and any organization where you are the last owner among remaining members refuses until you transfer the seat. Your bills still verify: the ledger is never touched. An account created through GitHub has no password until one is set through the reset flow, so it cannot confirm the delete before that.

## Dashboard

Purpose: see and manage the organization in a browser instead of over the API.

Open `/app` in a browser pointed at the server and log in with your email and
password; the app opens on the overview. Log out from the app.

The dashboard is the React app under `web/` (issue #234): overview, top-up, bills, requests, usage, pricing, keys, members, the transaction log, statements, webhooks, chat, settings and the admin console, built on vendored Cult UI components over the DESIGN.md palette, all reading the same `/api/v1` surface. The server-rendered Topcoat pages retired to it (issue #256), so an old `/dashboard/*`, `/app/login` or `/admin` bookmark answers `301` to its `/app` twin — the page names below are the `/app` routes. `/verify` keeps its server-rendered form on purpose — when the wasm bundle ships, the page's loader runs the same check in your browser instead (issue #284).

- **Overview**: the organization, who you are logged in as and in which role, the
  available balance, the frozen total (everything the organization's outstanding holds
  have reserved), this month's spend (what settlements have charged since the first day
  of the current month — the server's UTC month), the in-flight holds, and the newest
  ledger entries. When the organization carries a credit limit, the card also shows the
  limit, how much of it is used and how much remains — the drawn part is what a top-up
  repays first.
- **In-flight holds** update live: a hold appears when a gateway turn starts freezing,
  shows streaming progress while upstream answers, and leaves the list when the turn
  settles. The stream behind it is a WebSocket at `/ws/billing` (session login, like
  the pages); it carries only your organization's turns.
- **API keys**: list, mint and revoke. The same role rules as the API apply: members
  see and revoke only the keys they created, owners and admins see all. A freshly
  minted secret is shown once — store it then, it is never shown again.
  - A CLI or other tool without a keyboard-friendly way to paste a key uses the
    device grant: it prints a short code like `ABCD-EFGH` and a link to `/app/device`;
    sign in, open the page, type the code, confirm it names the organization you
    expect, and approve or deny. Approved, the tool receives a key named
    `device ABCD-EFGH` on its next poll — listed under API keys and revocable
    like any other. The code lapses fifteen minutes after the tool minted it.
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
  - **Member budget**: the same member PATCH (`/api/v1/org/members/{userId}`)
    takes `budgetLimitMinor` — a cap on what the member may commit across every
    key they minted, settled charges and outstanding holds counted together.
    Send `null` to clear it. The members page does not render the control yet;
    the API enforces it.
  - **Remove**: take the membership away. The person's account and their own personal
    organization stay, and if they were logged in, that session stops working.
    The member's keys keep their history but lose the cap a budget gave them.
  - **Make owner**: transfer ownership. You become an admin and the member you chose
    becomes the owner, in one step — the organization always has exactly one owner.
    Only an owner sees this control.
  - What you are not offered, you cannot do: an admin has no controls on the owner's own
    row, and nobody can remove or demote the last owner — transfer ownership first. A
    refused action says why, in words.
- **Transaction log**: the ledger entries, newest first: position, id,
  description and content hash — a hundred at a time, with **Older** and **Newest**
  links under the table walking further back.
- **Bills**: the organization's transactions, newest first, at `/app/bills`:
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
- **Verify an end-user report**: if your organization bills its own downstream
  customers out of this ledger, a customer cannot read the ledger to check a bill —
  every customer's usage mixes inside it. `POST /api/v1/org/usage-reports` with
  `{"endUser", "from", "to"}` mints a signed usage report for one `endUser` over an
  ended span (issue #261): the settlement entries attributed to them, plus a marker
  the organization committed into the ledger once and can never supersede. Hand the
  customer the bundle — it verifies offline with `oxsum_verify::verify_usage_report`,
  and the `/verify` page accepts it pasted (it runs the same check server-side).
  `GET /api/v1/org/usage-reports` lists what has been minted and
  `GET /api/v1/org/usage-reports/{reportId}` re-serves the frozen bundle; a
  re-request for the same span replays the first report, so the organization can
  never commit one subset and show another. What the report proves is membership
  and commitment — that the shown rows are in the ledger and bound to the digest
  the marker committed. Over a span the daily anchor covers (the `usage-anchors`
  job commits each customer's day-log head into the ledger), the bundle also
  carries the completeness proof: every day leaf of the customer's log, with the
  anchor entry that fixed the head, and the verifier rebuilds each leaf from its
  rows — a dropped day or a row moved to another day fails the check. Without an
  anchor the report still verifies, and completeness stays the organization's
  signed claim, which a customer's own request log spot-checks by comparing the
  report's `entryCount` and `chargedTotalMinor` against what it sent.
- **Share a bill as a link**: handing a customer a JSON file is not a bill. The
  dashboard's **Reports** page (`/app/reports`) mints a report and either link
  scope, shows the token once and lists what you shared. Enter an address in the
  link form and the customer is mailed the bill at creation — the plaintext token
  exists only then — and the notice says whether the mail went out. The same
  operations over
  the API: `POST
  /api/v1/org/usage-reports/{reportId}/links` mints a capability link (issue #300)
  and answers its `token` once — only the hash is stored. The link expires 30 days
  out by default (90 at most), stops working when you `DELETE …/links/{linkId}`,
  and `GET …/links` lists what you shared with `lastUsedAt` showing whether the
  customer opened it. `POST …/usage-reports/links` with `{"endUser": …}` is the
  other scope: one link that lists every report issued to that customer. The
  customer opens `/bill/{token}`: the page needs no account, renders the frozen
  figures beside the operator key and the ledger head, and works without
  JavaScript. It is never indexed and never cached. Pressing **Verify this bill**
  runs the verifier in the customer's own browser over the bundle the page embeds
  and reports what it checked — membership, the subset fold, the marker binding and
  the head's signature — and what it did not: completeness. It also checks the
  witness cosignatures stored for that head (issue #328): when pinned witnesses
  vouched for it the page says "witnessed by N"; when witnesses are configured but
  none covers this head, or when the deployment pins none, it says exactly that.
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
  (`/app/requests`): when the settlement was booked, the request id (the
  `x-oxsum-request-id` the gateway answered with), the model, the key that paid, the
  status — how the turn ended, in plain words: *settled*, *estimated*, *capped at
  freeze*, *client cancelled*, *upstream error*, *upstream unreachable*, *hold
  timed out*, *partly unpriced*, *released* or *refused*; hover the label for the
  one-line reason —
  and the input and output tokens it used and what it charged in credits. The
  filters live in the URL: `/app/requests?key=<prefix>&model=<name>`. Type a
  key's prefix (the keys page shows it) or a model name and press **Filter**, or click
  a key or a model in a row to filter by that value and keep the other filter. A
  filtered view is a link: it survives a reload and can be shared, and a filter that
  matches nothing shows an empty table with a message rather than an error. The list
  pages a hundred at a time — **Older** and **Newest** under the table, keeping the
  filters in the link. A turn that is still in flight has not settled yet, so it is
  not here — the overview shows those live.
- **Usage**: the organization's settled usage aggregated by day
  (`/app/usage`): a bar chart of what each of the last 30 days charged in
  credits — the day is the settlement's ledger booking date — a token-mix bar
  splitting the window's billed volume into fresh input, cached reads, output and
  reasoning, a table summing the window by the key that paid — labelled by the
  key's name, or its prefix when unnamed, with shared unattributed usage a row of
  its own — and a table summing the same window by channel and model, with turns
  and the token counts. A quiet day draws a zero-height bar, and a member's page
  sums their own keys' usage plus the organization's shared rows, the same scope
  the bills page applies — so a member's by-key table names only their own keys.

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
- An organization can also *render* its credits as a fiat unit (issue #257): on the
  settings page an owner or an admin sets a currency code plus a rate per credit
  (e.g. `USD` at `0.0014` — about $1.40 per thousand credits) and a decimals count,
  and every page then shows the converted figure beside the credits
  (`4.000000 cr ≈ 0.01 USD`), the statement PDF's money cells do the same, and the
  bills exports gain `displayAmount`/`displayCurrency` columns. Leaving the code
  empty renders credits alone again. This is a display only — what the ledger books,
  what a proof covers and what a charge computes are always the credits; the
  deployment can also name a fallback unit with `OXSUM_DISPLAY_CURRENCY` /
  `OXSUM_DISPLAY_RATE_PER_CREDIT` for organizations that have not chosen one.

## Chat in the browser

Purpose: top up, pick a model and chat without touching the API by hand, with each
turn's billing visible in real time.

Steps:

1. Log in at `/app/login` and open **Chat** in the dashboard (`/app/chat`).
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

- `POST /api/v1/org/keys` mints another key, with an optional `name`, an optional RFC 3339 `expiresAt`, and optional constraints: `spendLimitMinor` (the most the key may have committed — settled charges plus outstanding holds, in minor units; null is unlimited), `budgetDuration` (`"daily"`, `"weekly"` or `"monthly"` makes the limit periodic over the UTC calendar window instead of cumulative; it needs a limit), `modelAllowlist` (the gateway models the key may call; null allows all), `requestsPerMinute` (requests the key may start inside a rolling minute; null is uncapped) and `maxConcurrentHolds` (holds the key may keep open at once; null is uncapped). The secret comes back once.
- `GET /api/v1/org/keys` lists the organization's keys: id, name, display prefix, timestamps, and the constraint fields. Never a secret.
- `PATCH /api/v1/org/keys/{keyId}` replaces the constraint set — `spendLimitMinor`, `budgetDuration`, `modelAllowlist`, `requestsPerMinute`, `maxConcurrentHolds` — each cleared by `null`, under the same role rules as revoking. A hold that would push the key past its limit is refused with 429 `KEY_LIMIT_EXCEEDED` — on the gateway too, where it looks like OpenAI's `insufficient_quota` — so concurrent requests cannot exceed it. A limit of 0 means the key can never hold. A gateway request naming a model the allowlist does not carry is refused with 403 before upstream is contacted. A request past the key's minute allowance is refused 429 `RATE_LIMITED` with `Retry-After`, and a hold over the outstanding cap is refused 429 `TOO_MANY_HOLDS` until a settlement frees a slot.
- `DELETE /api/v1/org/keys/{keyId}` revokes one. It stops working immediately; the rest keep working.
- `GET /api/v1/org` describes the organization the credential belongs to.
- A key that is unknown, revoked or expired all answer the same 401 `UNAUTHORIZED`, so a leaked key's state is not revealed by probing.

### Delegating spend under a mandate

A mandate hands a bearer who does not hold the key a strictly narrower slice of it — a cap, an optional model subset, and a validity window — as a signed blob the delegate presents instead of the key's own authority being shared. Use it to let a script, a CI job or a sub-agent spend a bounded amount without ever seeing the key.

- `POST /api/v1/org/keys/{keyId}/mandates` mints one: `{"capMinor": …, "notAfter": "<rfc3339>"}`, plus an optional `models` allowlist and a `notBefore` to delay the window. The answer carries the `mandateId` and the `mandateBlob` — the signed, base64url string the delegate sends. A mandate may only narrow the key: a cap above the key's `spendLimitMinor`, a model outside its `modelAllowlist`, or an inverted window is refused `VALIDATION_ERROR` rather than minted.
- `GET /api/v1/org/keys/{keyId}/mandates` lists what the key has issued — id, cap, models, window, creation time. Members list only the keys they created; a mandate id on someone else's key answers `NOT_FOUND`, so ids are not probeable.
- The delegate sends the blob in `X-Oxsum-Mandate: <blob>` on the gateway (`/v1/*`) or on `POST /api/v1/holds`. A blob that fails to verify is 403 `INVALID_MANDATE`; outside its window is `MANDATE_NOT_YET_VALID`/`MANDATE_EXPIRED`; a hold that would push the mandate's committed spend past `capMinor` is 429 `MANDATE_EXCEEDED` — committed meaning settled charges plus still-open holds, counted from the ledger itself.
- A mandate is short-lived by design and is never revoked: to cut a delegate off, tighten the key's own constraint or revoke the key — the delegate's window just runs out.
- A settled delegated turn names the mandate in its settlement description (`{id, hash}` over the blob), so `/verify` proves which delegation authorized the charge.

## Top up

Purpose: add balance to the organization's wallet.

Steps:

1. `POST /api/v1/topups` with `idempotencyKey` and `amountMinor` in the body.
2. Save the returned `contentHash`; you will need it to verify the bill later.

With a redemption code instead: `POST /api/v1/redemptions` with `{"code": "oxr-…"}` —
no idempotency key, the code is its own. The answer is the credit's receipt plus
`amountMinor`; retrying the same call replays it with `isNew: false`, and a code that
is wrong, spent or past its expiry answers `NOT_FOUND` whichever it is. Ask your
operator for a code — they mint batches through `POST /api/v1/admin/redemption-codes`,
and the codes only exist in that one answer.

By card, on a deployment that offers Stripe checkout (the operator sets
`OXSUM_STRIPE_*`; without it the endpoint answers 503): `POST /api/v1/topups/stripe`
with `idempotencyKey` and `amountMinor` answers `{"checkoutUrl", "depositId"}` — open
the URL, pay on Stripe's hosted page, and the signed webhook credits the purchased
pool when the session completes. Repeating the call with the same key answers the
same checkout URL rather than opening a second session, and a session left unpaid
expires without moving money. The Airwallex rail (`OXSUM_AIRWALLEX_*`; issue #285)
works the same shape: `POST /api/v1/topups/airwallex` answers `{"checkoutUrl",
"intentId", "clientSecret", "depositId"}` — open the URL, pay on Airwallex's hosted
page, and the signed webhook credits the pool when the intent succeeds.

The Hyperswitch rail (`OXSUM_HYPERSWITCH_*`; issue #319) is the switch the payment
layer runs on: `POST /api/v1/topups/hyperswitch` answers `{"paymentId",
"clientSecret", "publishableKey", "depositId"}`. The browser confirms the payment
with the switch's own SDK using those handles, and the signed webhook credits the
pool when the payment succeeds. The dashboard's checkout widget for this rail is
the next step; until it lands, the handles are the integration point.

On a deployment running the real rail (`OXSUM_X402_FACILITATOR_*` — an x402-rs
facilitator sidecar verifies the authorization on-chain, settles it, and the
chain transaction hash lands on the deposit row) or pinning a stand-in payer key
(`OXSUM_X402_VERIFYING_KEY`; without either the endpoint answers 503), a signed
crypto-rail authorization credits the same way: `POST /api/v1/topups/x402` with
`{"payment": "<base64url authorization>"}` — the authorization names `scheme`,
`network`, `asset`, `amountMinor`, `payTo`, `nonce`, `payer` and `signature`,
and its `nonce` is the idempotency anchor, so a repeated call replays the
credit (`isNew: false`). Anything that does not verify — a bad signature, a
foreign scheme or network, a settle the chain refuses — is `FORBIDDEN`. The
stand-in verifies a test-scheme Ed25519 signature for development and tests
only; the facilitator configuration is what production runs.

Note: the unit is minor, 1 credit = 1_000_000.

## Paying for one AI call

There are two ways to pay: let the gateway do both halves for you, or drive the hold and the
settlement yourself. Both are billed the same way.

### Through the gateway (recommended)

Purpose: point an OpenAI or Anthropic client at oxsum and have every turn frozen and charged for you.

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

   Or Anthropic's SDK — `POST /v1/messages` speaks Anthropic's Messages format,
   served by the channels whose protocol is `anthropic`:

   ```python
   from anthropic import Anthropic

   client = Anthropic(base_url="http://127.0.0.1:3000", api_key="oxs-…")
   answer = client.messages.create(
       model="claude-sonnet",
       max_tokens=200,
       messages=[{"role": "user", "content": "Explain holds in one sentence."}],
   )
   ```

   Each surface serves only the channels that speak its protocol: a model behind an
   `openai` channel is not served on `/v1/messages`, and the reverse — there is no
   translation between the two. Errors come back in the surface's own envelope, and
   the oxsum key travels as `x-api-key`, exactly as the SDK sends it.

   The same key also opens the input-only surfaces. Embeddings go to
   `POST /v1/embeddings` in OpenAI's shape — `input` may be a string, an array of
   strings, or token arrays — and reranking to `POST /v1/rerank` in the shape Jina
   and Cohere speak (`query` plus `documents`). Neither streams, and neither is a
   chat price: a model is served there only when a channel prices it with
   `mode: embeddings` or `mode: rerank`. The freeze reserves the input bound alone —
   token arrays freeze at their exact count — and the turn settles against
   upstream's usage report, estimated from the request's input when none arrives.

2. Every response carries an `x-oxsum-request-id` header. That id is how the turn is found in the
   ledger: its entries are keyed `req-<id>:hold` and the settlement derived from it
   (`oxsum_core::settlement_key_for`).

   The cost rides along too: `x-oxsum-freeze-minor` is the most the request can charge and
   `x-oxsum-balance-minor` is the spendable balance left under that freeze. Once the turn
   settles, `x-oxsum-charged-minor` reports what it actually cost — a header on a
   non-streamed answer or a post-hold refusal, and an HTTP trailer (`Trailer:
   x-oxsum-charged-minor` announces it) on a streamed one, since the response head is sent
   before the stream settles.

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
- Retrying safely: send an `Idempotency-Key` header with the request, and a retry carrying the
  same key and body is the same turn — it replays the stored answer (a streamed turn answers
  its settled receipt, since a stream cannot be replayed) instead of freezing and charging
  again. Replays are marked `Idempotent-Replayed: true` and carry the original
  `x-oxsum-request-id`. Retrying while the first turn still runs is refused 409, and the same
  key under a different body is refused 422; a key is free again once its record is 24 hours
  old. A refusal that never reached the wallet does not hold the key: fix the request and
  retry under it.
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
| `refused` | Admission refused the request before any hold — a rate limit, an exhausted budget or key limit, insufficient funds, a model the deployment does not serve | Nothing — no hold existed to settle; a marker entry records the refusal with its reason so the call still shows on the billing page |

Beside the `kind` each billing record carries a terminal `status` — `settled` for every
settled outcome including the anomalous kinds, `errored` for the turns that served nothing
after their hold (`upstream_error`, `upstream_unreachable`, `swept`), `refused` for the
requests turned away before a hold — and, for the turns that carry one, a `failureReason`
in the caller-facing words the answer already gave. A 400 or 429 is no longer invisible
outside the server log: it is a row on the requests and billing pages, and its marker
entry verifies the same way every other record does.

Every settlement also records which channel served the turn and which price version priced it
(`priceVersion` beside the prices and the token counts), plus `upstreamAttempts` — how many
upstream calls the turn made under its one hold. Above 1 means the request failed over to another
channel serving the model, or waited out a short `Retry-After` on the only route left; the channel
named is always the one that actually answered. A version is never rewritten, so a price
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
The number is one balance: granted credit (signup bonus, admin grants), purchased
credit (top-ups) and an organization's credit line are separate pools inside the
ledger, the pools are drawn in that order, and the API sums the three.

When the platform admin has granted the organization a credit limit,
`creditLimitMinor` and `creditUsedMinor` come along: the limit is spendable credit
that keeps `availableMinor` positive past what the organization has paid in, a hold
draws it only after granted and purchased credit run out, and a top-up repays the
drawn line before it adds purchased balance. The admin lowers the limit only after
the drawn part is repaid — shrinking below the debt is refused, and a top-up is how
the debt shrinks.

If the platform suspends the organization, the balance still reads and money still
lands — but new holds refuse `FORBIDDEN`, so gateway calls and `POST /api/v1/holds`
fail until the operator reinstates it. Holds already open still settle, and a
subscribed webhook hears `org.suspended` at the moment it happens.

## Statements

Purpose: read the monthly bill for what the credit line carried.

A platform that bills monthly issues one statement per UTC month that had usage:
`GET /api/v1/statements` lists them newest first, and
`GET /api/v1/statements/{statementId}` answers one with its `lines` — the month's
charges itemized by channel and model, with turn and token counts. What the
statement is owed is `creditDrawnMinor` — the part of the month's charges the
credit line carried — and `outstandingMinor` is what is still open. `dueDate`
comes from the `paymentTermsDays` the organization carried when the statement was
issued.

For a document you can send on — an invoice for your own records or your
accounting — `GET /api/v1/statements/{statementId}/export.pdf` downloads the
statement as a PDF: the same header (your organization, the period, status, due
date), the itemized lines, and the totals, rendered from the stored lines so it
can never disagree with the API's own answer. A footer anchors the page to the
ledger — the `logFromIndex`–`logToIndex` window the lines prove — beside a QR
that opens `/verify`, so a paper copy still points back at the checkable
record. Only a finalized statement exports; a draft (the platform's working
document) answers 404, as does a statement that is not yours. The platform's
own copy is `GET /api/v1/admin/statements/{statementId}/export.pdf`.

Paying a statement needs no dedicated call: any top-up or redemption repays the
credit line first, and repayments always land on the oldest open statement — so a
`POST /api/v1/topups` while a statement is outstanding settles it. `paymentStatus`
reads `pending` until the due date has passed (then `overdue`), `suspended` while
the platform holds the bill, and `paid` once repayments cover it. When the
platform has opted the organization into dunning, a statement unpaid past
`dueDate` plus the configured grace also suspends the organization's *spend* —
new holds refuse `FORBIDDEN` until the line is repaid and the daily pass
reinstates it — so paying an overdue bill is what restores the key's calls.

## Webhooks

Purpose: be notified when money moves — a request settles, a deposit lands,
the balance crosses a line, a statement turns — without polling the API.

Steps:

1. Register an endpoint: `POST /api/v1/webhooks` with
   `{"url": "https://your-server/hook", "events": ["request.settled"], "balanceLowMinor": 2000000}`
   — pick the events it should hear (`request.settled`, `org.suspended`,
   `deposit.credited`, `deposit.failed`, `deposit.reversed`, `balance.low`,
   `statement.finalized`, `statement.payment_overdue`,
   `statement.payment_status_updated`, `org.credit_limit_exceeded`,
   `anomaly.detected`, `budget.exhaustion_forecast`,
   `budget.threshold_crossed`). `balanceLowMinor` is the
   available-balance line
   `balance.low` fires below — omit it and the endpoint stays deaf to the
   balance. The URL must be HTTPS — HTTP is allowed only to localhost for
   development. The answer carries `secret`, a `whsec-` string shown exactly
   once; store it, afterwards the API shows only `secretLast4`.
2. Receive `POST`s at that URL. Each is a JSON envelope
   `{id, type, created_at, org_id, data}`. For `request.settled` the `data`
   names the request (`requestId`, `model`, `channel`), the charge (`kind`,
   `chargedMinor`, `freezeMinor`, `settlementEntryId`) and the token counts;
   for `org.suspended` it is just `{organizationId}` — the platform suspended
   the organization's spend, and new holds will refuse `FORBIDDEN` until an
   operator reinstates it. `deposit.credited` carries `{rail, paymentRef,
   amountMinor, entryId}`; `balance.low` `{availableMinor, thresholdMinor}` —
   it fires once per crossing and re-arms when a deposit lifts the balance
   back over the line. `statement.finalized` carries `{statementId, period,
   totalMinor, creditDrawnMinor, dueDate}`, `statement.payment_overdue` and
   `statement.payment_status_updated` name the statement and its standing.
   `org.credit_limit_exceeded` fires when a credit-lined organization hits its
   line — a hold refused 402 while a `creditLimitMinor` is set.
   `anomaly.detected` fires on the odd settlements — capped, estimated,
   client-cancelled, swept, unpriced, released, dead-lettered holds — and on
   upstream price drift: when the price-sync job finds a stored `upstream`
   rate disagreeing with the channel's live catalog — the oxsum-compatible
   `GET {baseUrl}/api/v1/pricing`, since a real provider exposes no such
   catalog; its cost is compared by the upstream bill sync instead — `data` is
   `{kind: "upstream_price_drift", channel, model, field, storedMinor,
   liveMinor}`, broadcast to every subscribed endpoint across all organizations
   (the rates are the public catalog's, never organization data).
   `budget.exhaustion_forecast` carries `{keyId, spendLimitMinor,
   budgetDuration, windowStart, committedMinor, projectedExhaustionDate,
   daysRemaining}` — an early warning that a key with a `budgetDuration` window
   is on pace to hit its `spendLimitMinor` before the window closes. It fires
   once per key per window; PATCHing the key's constraints or rolling into the
   next window re-arms it, and the keys dashboard shows the same projection
   inline. `budgetThresholdsPercent` arms the threshold side of the same
   budgets: an integer list like `[50, 80, 95]`, and every settle re-checks
   the key's `spendLimitMinor`, the minting member's `budgetLimitMinor` and
   the organization's credit line — `budget.threshold_crossed` delivers
   `{scope, keyId|memberId|organizationId, limitMinor, committedMinor,
   consumedPercent, thresholdPercent, windowStart}` once per armed line per
   window, and re-arming the limit itself re-arms the line. The current
   figures without a webhook: `GET /api/v1/budget` answers the same
   limit/committed/percent per scope.
3. Verify every delivery before acting on it: read `x-oxsum-signature`
   (`t=<unix>,v1=<hex>`), recompute HMAC-SHA256 of `"{t}.{raw body}"` with the
   stored secret, compare in constant time, and reject timestamps older than a
   few minutes. `x-oxsum-delivery` is the delivery's id — dedupe on it, because
   a receiver that is slow or answers non-2xx sees the same delivery retried
   (10s, 1m, 5m, 15m, 30m, then hourly; ten attempts before the row is `failed`).
4. Inspect recent deliveries per endpoint with
   `GET /api/v1/webhooks/{endpointId}/deliveries` — status, attempt count, the
   receiver's last answer and the last error. `GET /api/v1/webhooks` lists the
   endpoints; `DELETE /api/v1/webhooks/{endpointId}` removes one and stops its
   queued deliveries.

## Reading billing data from scripts

Purpose: pull usage, bills and prices into a pipeline instead of scraping the
dashboard — all four are ordinary `GET`/`POST` calls under the organization's
API key, and a member session sees its own keys' rows plus the shared ones.

- `GET /api/v1/usage?from=YYYY-MM-DD&to=YYYY-MM-DD` — the daily rollup rows
  the usage page sums (bounded to a 92-day window).
- `GET /api/v1/billing-records?limit=…&before=…` — settled turns newest-first;
  follow `nextCursor` until it disappears, and any row's `requestId` names the
  settlement entry `req-<requestId>:settle` a proof can be fetched for. When the
  platform granted your organization a discount, the row carries
  `discountPercent` — the same snapshot the settlement's proof recomputes
  against, so a discounted bill verifies like any other.
- `POST /api/v1/estimate-price` — `{"model", "inputTokens", "outputTokens"}`
  answers `estimateMinor`: the most a real request of that shape could be
  charged, computed by the same arithmetic the gateway freezes with.
- `GET /api/v1/pricing` — every model's current price version, with the channel
  and protocol it is served through.

## Verifying a bill

Purpose: confirm a recorded transaction has not been altered since.

Steps:

1. `GET /api/v1/entries/{entryId}/proof` to fetch the proof bundle (the entry source, the inclusion proof and the tree head, as JSON).
2. Open `/verify` in the browser — no login needed — paste the bundle and the `contentHash` you saved, and submit. When the deployment ships the wasm bundle (the usual case), the check runs entirely in your browser: the page loads the same `verify_bundle` the server runs compiled to WebAssembly, so a passed check does not depend on trusting the server, and the bundle never leaves your machine. A browser without the wasm bundle — or JavaScript off — posts the same fields to the server, which runs the identical check and renders the verdict.
3. Read the verdict. "Verification passed" means the bundle matches the content hash and the inclusion proof links it to the tree head — and, for a usage settlement, one more line: the charge inside the entry is recomputed from the usage and rates it records (`charged = ceil(Σ units × pricePerM ÷ 1,000,000)`, capped at the freeze). A settlement written before descriptions were versioned, or by a schema newer than the page's verifier, reports that inclusion passed but the arithmetic is too old or too new to recompute — it is skipped, not guessed at. "The charge does not add up" means the entry is proven but its own numbers disagree with each other, which an honest settlement never does. "Verification failed" means something changed: altering any single number in the bundle fails the check. A bundle that does not parse, or a hash that is not 64 hex characters, gets its own error instead.

A passed check on a settlement also reports how the bill's *price* fared. The operator signs a checkpoint per `(channel, model, priceVersion)` — the same key that signs tree heads, under the origin `oxsum/price-catalog/<channel>` — and a fresh bundle carries that checkpoint in its `price` field: the canonical row the server hashed, the note attesting it, and the verifying key. "Bound to a checkpoint the operator signed" means the price your bill names is the catalog row the operator attested — a forged current price table cannot produce it. "No signed price binding" is information, not failure: a top-up names no price, a bundle made before this shipped carries none, and a deployment without `OXSUM_HEAD_SIGNING_KEY` signs nothing. A binding that *does not verify* is its own sentence — the note names a different price or its signature fails — and that never happens on an honest bill. You can also fetch any version's checkpoint yourself at `GET /api/v1/log/price-head?channel=<name>&model=<name>[&version=<n>]`; signing is deterministic, so an old version's note is identical whenever it is asked for.

Note: verification proves "this record has not been altered since it was written", and for a versioned settlement that its charge agrees with the usage and rates it records; the signed price binding extends that to "the price it names is the one the operator's catalog held". It still cannot prove "upstream really returned that many tokens".

## Verifying the log's history

Purpose: confirm the ledger you see today is the ledger you saw last week, with entries appended and nothing rewritten.

The bills page does this check automatically on every visit (see "Append-only archive" above): it archives the signed head locally and verifies the consistency proof in the browser. What follows is the same check done by hand — useful for an independent audit, or for keeping the archive somewhere other than the browser.

The operator signs the head of your organization's log. Save the signed head from `GET /api/v1/log/head` (the `note` text and the `size`/`root` it carries) alongside your records. Later, `GET /api/v1/log/consistency?from=<that size>` returns the new signed head and the proof between them. Check the note's signature against the operator's key (published at `GET /api/v1/log/key` — get the key through a channel the operator does not control the first time, or the signature proves only that the server agrees with itself), then check the proof against the two heads. It passes only when the new log is the old log with entries appended: a second history at a size the operator already signed for cannot produce it.

### Anchored heads (external witnesses)

The check above catches a rewrite between *two points you recorded* — but a signed head still attests *a* history the operator claims, not the one independent parties saw. Deployments that configure `OXSUM_WITNESS_URLS` close that gap: every hour the `witness-checkpoints` job hands each organization's signed head to external C2SP `tlog-witness` endpoints, which only cosign it when it extends the head they already vouched for — a rewritten history is refused at their side, not ours.

`GET /api/v1/log/checkpoint` answers the latest anchored head: `headSize`, the operator-signed `note`, `cosignatures[]` — one `{witnessUrl, note, firstSeen}` per witness that cosigned exactly that head — and `witnessKeys[]` (issue #259), the deployment's pinned keyset (`data` is `null` until the first pass anchors something). To verify by hand: check the operator's signature line in `note` against the key at `GET /api/v1/log/key`, and each cosigned note's additional signature line against the pinned `witnessKeys` entry for its URL — or your own pinset, if you do not trust the deployment's claim. The dashboard's transaction log page (`/app/log`) shows the anchored head and its witnesses when anchoring is configured, and runs the same check itself: it reports "anchored by N witnesses" only when the cosignatures verify under the pinned keys, and names the witness whose line fails. A verifier that wants the real guarantee pins the keys through a channel the operator does not control — the endpoint is convenience, the pin is the trust choice.

### Serving as a witness yourself

An oxsum deployment can *be* the witness another log anchors against — including another oxsum deployment's (issue #260). Set three variables and `POST /add-checkpoint` answers foreign checkpoints at the root of the same server:

- `OXSUM_WITNESS_SIGNING_KEY` — 32 bytes, base64: the Ed25519 seed the witness's `cosignature/v1` lines sign under.
- `OXSUM_WITNESS_KEY_NAME` — the name those lines carry (default `oxsum/witness`); this is what a verifier's key hash selects by.
- `OXSUM_WITNESS_TRUST` — comma-separated `origin=size:root_b64` pins: every log you vouch for and the head you start from. The other operator hands you their origin and a signed head out of band and you pin it here — a submission for an origin you never pinned is a 404, because a witness that accepts whatever arrives first could already be signing a fork. To onboard a brand-new log whose head is not worth pinning yet, pin `size=0`: the first non-empty head it is offered is taken on trust (every log extends the empty tree, so there is nothing provable earlier), and it is a real witness from then on.

Two oxsum deployments point their `OXSUM_WITNESS_URLS` at each other — `https://b.example/add-checkpoint` in A's list and vice versa — each pinning the other's `oxsum/ledgers/<tenant>` origin at a head it signed, and they mutually witness with no third software. Once a checkpoint is accepted the witness stores it and files every note it returned; a restart re-pinning a root the stored head contradicts at the same size refuses to boot rather than roll back onto a fork it already moved off.

## Verifying a statement's anchor

Purpose: confirm a finalized statement commits the *whole* covered set — every entry the month charged, in order — and that the figures the document shows are the figures the ledger committed, not just that the rows it chose to display add up.

A bill's own proof shows one entry was not altered. A statement's anchor closes the gap that leaves: the proof cannot say whether the statement's lines are *all* the lines. Finalizing a statement appends a marker entry to your organization's ledger whose document hash is the window digest and whose description carries the rollup digest, and the detail endpoint hands that marker back as the `anchor` object on `GET /api/v1/statements/{statementId}` (the platform's own copy is `GET /api/v1/admin/statements/{statementId}` — the same anchor). `verify_statement_anchor` in `oxsum_verify` re-runs the check locally, the same way `/verify` re-runs a bill's:

1. Fetch the detail: `GET /api/v1/statements/{statementId}` and keep the `data` object — it carries `statement`, `lines` and `anchor` (`{entryId, logIndex, windowHash, rollupHash, origin, note, keyName, publicKey, keyHash}`).
2. Collect the covered entries' `contentHash` values **in `logIndex` order** — the entries the statement's `logFromIndex`–`logToIndex` window spans. `GET /api/v1/billing-records` pages the settled turns (each row's `requestId` names its settlement entry `req-<requestId>:settle` for `GET /api/v1/entries/{entryId}/proof`), and the bills page's JSON export carries `entryId`/`contentHash`/`bookedOn` for the same archive. Pass them to the check as `window_hashes`; the fold recompute under `oxsum/statement-window/v1` must equal `windowHash` — a dropped, inserted or reordered entry changes it.
3. The rollup re-serializes the statement's own fields and lines to canonical bytes and must digest under `oxsum/statement-rollup/v1` to `rollupHash` — the totals the document shows are the ones the marker committed.
4. On a signing deployment (`OXSUM_HEAD_SIGNING_KEY`) the anchor's `note` attests the tree head *right after* the marker — the head at exactly `logIndex + 1`, fetched from `GET /api/v1/log/head` (the current head when the marker is the latest entry). Pass it as `expected_head` and the ledger's `origin` (`oxsum/ledgers/<tenant_id>`, the same string `log/head` answers) as `expected_origin`; the check verifies the note's signature against `publicKey`, so the marker's *inclusion* is proven, not merely claimed.
5. An unsigned deployment serves `note`/`keyName`/`publicKey`/`keyHash` as `null` — `expected_head` is then unused (the check returns before consulting it) and the anchor still binds the two digests. Either way a passed check names what failed otherwise: `Absent` (no anchor — the statement is not finalized), `WindowMismatch`, `RollupMismatch`, `Malformed`, `WrongOrigin`, `HeadMismatch`, `Unsigned` or `BadSignature`.

Note: like a bill's proof, the anchor proves the *record* — that the committed window and rollup are what the ledger holds — and on a signing deployment that the operator signed the head containing the marker. It does not prove upstream really returned those tokens; that honesty question sits one layer up.

## Platform admin

Purpose: manage the deployment itself — the upstream channels the gateway relays to and the price each model charges.

The admin pages are not part of the dashboard: they open with the deployment's operator token (`OXSUM_ADMIN_TOKEN`), not with a login, and they are reachable only in a deployment that configured one.

Steps:

1. Open `/app/admin` in the browser and enter the operator token. The page proves it against the real surface before remembering it — a refused token is reported, not stored — and keeps it in the browser's `localStorage`, the same place the chat page's key lives. A `401` on any later call forgets it and asks again.
2. The channels and prices page the `channels` section lists every channel: its name, upstream address, the last four characters of its upstream key (the only part ever read back), and the current version of each model's price — input and output per million tokens, and the output cap a freeze is sized by.
3. "Add or repoint a channel" writes a channel: a name that already exists is repointed to the new address and key, and its price history stays where it is.
4. "Append price" on a channel's card appends a new version for one model. Prices are append-only — a change is `v{n+1}`, never a rewrite — and "Show history" reads every version the channel has ever had, newest first. That history is what a bill's `priceVersion` is checked against later. A model may be priced on several channels: each route carries a `weight` (1–1000, default 100) — the relative preference for leading a request — so failover between channels is configured by pricing the same model again elsewhere, and the API takes the weight beside the price set. The API can say more than the form today: a price set may also carry cached-input, cache-write (5m and 1h) and reasoning rates, a flat per-request fee, the upstream's own prices, and conditional `rules` — a `match` on `serviceTier` or an input-token window swaps in a whole different set, most-specific-wins; see `PriceRequest` in `openapi.yaml`.

Note: a request resolves its price when it starts and carries the version to its settlement, so a price change lands on later requests only; in-flight turns and old bills are untouched.

Upstream-price drift is also watched automatically where the channel is another oxsum deployment: with `OXSUM_UPSTREAM_PRICE_SYNC` set (and `OXSUM_SECRET_KEY` present to unseal the channel credentials), a daily job probes each channel's `/api/v1/pricing`, and a stored `upstream` rate the live catalog contradicts opens a drift row — broadcasting `anomaly.detected` and listing in the reconciliation report — that resolves itself once the rates agree again. Every probe records an outcome (`ok`, `unreachable`, `invalid`, `auth`), so a channel that stops answering is visible too.

The organizations page the `organizations` section lists every organization the ledger holds money for — its name, kind (personal or team), headcount, and what its wallet shows: the available balance and the frozen sum of its outstanding holds. Each row carries an "Adjust" form: a signed amount in credits (a leading `-` deducts) and a required reason, which the new entry stores as its description — part of what a bill's proof covers. The same card carries a **Credit notes** form: a positive amount and a required reason, posting one ledger entry (Revenue debited, Wallet credited) and moving no money — a credit note is not a refund, and the list beneath the form shows what was issued. A deduction the balance cannot carry is refused with `INSUFFICIENT_FUNDS`; nothing in the page ever rewrites history.

A deployment can also start each new organization's wallet with credits: `OXSUM_SIGNUP_BONUS_MINOR` (default 0) grants that amount at self-registration, booked as an adjustment whose reason is "signup bonus". An invited account joins an existing organization and is granted nothing.

A forgotten password resets itself when the deployment sends mail: the login page's "Forgot your password?" opens `/forgot-password`, the mailed link lands on `/reset-password`, and setting the new password revokes every session the account held — the admin reset's semantics, self-served. The forgot endpoint answers the same whether or not the address has an account, so it reveals nothing. A deployment without a mailer keeps the operator path: `POST /api/v1/admin/users/{userId}/password-reset` with `{"newPassword": "…"}` sets the replacement and revokes every session the user held at once; the answer's `sessionsRevoked` says how many died with it.

The in-flight page the `in-flight` section lists every hold the platform is currently reserving, across all organizations and newest first — the organization, the request id, the model and channel, the price version the turn froze at, the frozen amount and when it opened. A hold that settled leaves the list; one older than the hold timeout (30 minutes) is one the sweeper is about to release as `swept`. A hold whose sweep keeps failing is marked `dead` after ten failed attempts — it retries hourly from then on, and the reconciliation page lists it under `holds_dead_lettered` for an operator to follow up.

The anomalies page the `anomalies` section reviews the settled turns that did not price cleanly, across all organizations and newest first: an `estimated` turn where upstream reported no usage, a `client_cancelled` one where the caller left mid-stream, a `capped` one whose upstream usage priced above the freeze, a `swept` one whose hold the sweeper had to release, and an `unpriced` one whose usage named a dimension the price book cannot bill — the computable part was charged and the rest is the platform's flagged loss, never a silent zero. Every row is the settlement record that turn's own ledger holds — organization, request id, model, channel, the price version in force, the charge and what had been frozen — plus a `score` (0–100) and a `severity` band (`low`/`medium`/`high`) the row earns from what it already records: how hard the charge pressed the freeze, what it cost the platform upstream, whether it had to fail over, and whether the same request id has shown up anomalous before (issue #263). The endpoint filters on the band (`?severity=`) and sorts by it (`?sort=score`), so the worst leaks surface first; scoring is read-only and never changes what was charged. A summary table on top counts the anomalous turns and their charged total per channel: where the platform is losing money upstream is the first thing the page answers.

The margin page (the `margin` section) puts the two sides of the business side by side, per channel and model: what organizations were charged, the `upstream` cost the same usage priced at, the margin between (`marginMinor`), the provider's own bill once the `upstream-bill-sync` pass has reconciled the channel (`not reconciled` means no bill pass has touched it yet, not that it billed nothing), and `untrackedTurns` — settled turns with no upstream cost recorded. A totals row sums the model-scoped figures and counts each channel's provider bill once, because the API repeats the channel-scoped figure on every model row. The same numbers answer over the API: `GET /api/v1/admin/margin` sums what organizations were charged against what upstream cost the platform, per channel and model, and reports the difference as `marginMinor`. `untrackedTurns` counts the settled turns no `upstream` price block covered (a swept turn, a price without one, or history predating the column): read it as the coverage gap the margin is blind to, because an untracked turn is never a free one — the number to watch before trusting `marginMinor`. `providerBilledMinor` is what the provider actually billed the channel — the `upstream-bill-sync` job's billed fact against `upstreamCostMinor`'s estimate — channel-scoped, so the same figure repeats on each of that channel's model rows and `null` means the pass has never reconciled the channel, not that the provider billed nothing.

The reconciliation page the `reconciliation` section answers whether the projections still match the books: the scan reads every organization's ledger against the usage rows, the deposits and the hold watches, and lists each drift class that counted nonzero — a usage row whose settlement entry is missing, a settled turn that wrote no record, a deposit marked credited with no entry behind it, a rail that paid a different amount than expected, a deposit confirmed but never credited, a hold watch with no reservation behind it, a pending hold the sweeper cannot see, a hold whose sweep failed ten times and was dead-lettered, a periodic job run that exhausted its attempts, a log whose positions are not dense, and — at platform scope — a stored `upstream` rate the channel's live catalog contradicts (`upstream_price_drift`, sampled as `channel:model:field`). Each class carries its count and a bounded sample of identifiers to follow: click the class to list every offender the scan kept (up to twenty, with "showing N of M" when the count is larger), and a class that counted zero says `none detected` rather than an empty cell. The classes now include `anchored_day_altered`: a day leaf that no longer folds to the map root its daily anchor committed — a settlement that bypassed the sealing rule, or a hand edit. Settlements into an anchored day are refused with `409 DAY_SEALED`, so the class should stay empty; it exists as the check that the seal held. `clean` means every class counted zero. The scan reports and never repairs — drift is for the operator to act on, not for a second writer to patch silently.

The closing page the `closing` section runs the monthly closing: pick a month that has fully ended and close it — the month is sealed in every organization's ledger, which produces its closing record (a seal committing to the log's tree head and the month's closing trial balance, chained onto the seal before it) and refuses any new entry dated into it from then on. The table lists the sealed months newest first: organization, period, how many entries it covers, the log's size at sealing, and the three hashes the record is made of. Closing a month twice is safe — the second run answers the records that already exist.

Like the margin view, the audit log answers over the API only: `GET /api/v1/admin/audit` reads back every mutating call this surface ever took, newest first — one row per write naming the action (`channel.set`, `discount.create`, `statement.finalize`, …), what it acted on, the request fields safe to show and the idempotency key it carried. Credentials are never in it — a channel's `apiKey`, a new password and a minted batch's codes stay out by construction — and a retried keyed write audits once. `?action=` narrows to one call kind, `?limit=` and `?cursor=` page it.

Billing a service of your own — one whose work no upstream LLM produced — runs through the metering surface, also API-only. Mint a service credential with `POST /api/v1/admin/service-credentials` (`{"name"}` names the reporter a settlement records), copy the `oxs-svc-…` secret out of the answer — it exists nowhere else — and revoke it later with `DELETE /api/v1/admin/service-credentials/{credentialId}`; `GET` on the collection lists what is live. A price the service bills by is a normal price version carrying `mode: event`, appended to a channel under a `billableCode` the callers name. The service then reports usage against an organization's wallet: `POST /api/v1/metering/holds` freezes a declared bound and answers the `holdKey`, `settle` charges the event's real usage at the version the hold pinned (never above the freeze), `release` lets it go at zero, and `settlements` does hold-plus-settle in one call for an event that already completed. The organization's bills page and `/verify` check the charge like any other — the credential can bill, but it cannot make an unverifiable one.
