# User guide

For callers integrating with oxsum; no technical internals. Field details live in `crates/server/openapi.yaml`. Protected routes require the credential class declared in OpenAPI: customer keys, user sessions, or narrowly scoped integration credentials. Public setup/invitation/verification links have their own token authority. The credential and selected context determine the customer or business scope; an identifier in a payload never grants access.

Each self-hosted deployment belongs to one business. Your organization is your personal or team customer billing account; its existing keys, balance and bill evidence continue to work after the ownership upgrade. Being an organization owner does not make you business staff. API fields such as `organizationId` keep their existing names.

Technical operators can collect a [sanitized deployment report](operator-diagnostics.md) with health, configuration presence and recovery queue counts. Personal/business sessions and consumption keys cannot access its protected aggregates. Customers should contact their operator with the original request identity.

## Accept initial business ownership

The deployer gives the intended owner a short-lived setup token. Open `/app/setup`, enter the token and business name, and explicitly accept ownership. New users register with the invited email and a password; existing users sign in first, including their normal second factor, and accept as the displayed identity. The token is kept only in the current page, so paste it again after leaving or refreshing.

On success, sign in and open business setup. Customer registration stays invitation-only. If a response was lost, reopen setup while signed in to check your business access. An invalid, expired or wrong-email token grants nothing; ask the deployer for a new token only when setup is still incomplete. A personal customer consumption key returned during registration is separate from business authority.

## Get an account and a key

Purpose: get a user, an organization to hold credits, and a credential to call the API with.

Steps:

1. `POST /api/v1/auth/register` with `email`, `password` (at least 12 characters) and optionally `organizationName`.
   - The response carries `user`, `organization` and `apiKey.secret`. **The secret is shown once and cannot be retrieved again**; store it now.
   - A personal organization is created with you as its owner, named after the address's local part unless you chose a name.
   - An existing address answers 409 `CONFLICT`.
   - Invite-only deployments answer 403 `FORBIDDEN` until the initialized
     business owner explicitly changes the persisted registration policy. The
     legacy `OXSUM_SIGNUP` setting cannot override it.
   - A business customer invitation reserves a new customer identity and its
     first owner. A customer-team invitation joins an existing account as a
     member. Both use `/app/invite?token=…`, bind the intended email and require
     registration or explicit signed-in acceptance before expiry. Neither
     grants business staff authority or purchased funds. A team invitation does
     not create a separate funded customer account.
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
- When the deployment configures enterprise OIDC providers (`OXSUM_OIDC_PROVIDERS`), the login page draws a button per IdP — Okta, Keycloak, Authentik — from the same `auth/methods` read (`oidcProviders` lists their `oidc:{name}` route names). The flow is the same shape as GitHub's — provider, verified email, a linked account or a registration when signup is open — running authorization-code + PKCE against the IdP's own discovery document, so your IT-issued login lands in the same session either way.
- You can add a TOTP second factor (any authenticator app) from your session: `POST /api/v1/auth/totp/enroll` answers a base32 secret and an `otpauthUrl` your app reads as a QR; `POST /api/v1/auth/totp/confirm` with a live code turns it on and shows ten `xxxx-xxxx` backup codes — once, so save them. From then on your password login answers `{"requiresTotp": true, "challengeToken"}` instead of a session, and `POST /api/v1/auth/totp` with the challenge plus a live or backup code finishes it. `POST /api/v1/auth/totp/disable` with a live code removes the factor — so a stolen session alone cannot strip it.
- The session expires thirty days after login. Customer access rechecks membership; Business access independently rechecks current staff authority. Removing one customer membership does not itself revoke unrelated staff authority.
- When the deployment sends email, registration and an invitation's redemption mail a verification link; the dashboard shows an unverified address a reminder with a resend (a minute between mails), and the link verifies the address for good. `GET /api/v1/session` reports the state as `user.emailVerified`. A deployment without a mailer skips all of it — nothing is mailed, nothing is reminded.
- Session-only account and Business routes do not accept a consumption key as a substitute. The gateway (`/v1`) uses consumption credentials, not the login cookie; scoped integration endpoints enforce their own declared grants.
- Members see and revoke only the keys they created; owners and admins see and revoke every key of the organization. Keys you mint while logged in record you as their creator.
- Deleting the account: `DELETE /api/v1/users/me` with `{"password": "…"}` — the session cookie plus your password, since the act is irreversible. Your email is tombstoned, every session and every key you minted dies at once, your memberships go, and the `endUser`/`tags` attribution on usage your keys paid is erased. Your personal organization is suspended and renamed rather than deleted — the ledger under it cannot be orphaned — and any organization where you are the last owner among remaining members refuses until you transfer the seat. Your bills still verify: the ledger is never touched. An account created through GitHub has no password until one is set through the reset flow, so it cannot confirm the delete before that.

## Getting started (first setup)

Purpose: reach a first settled request and a customer-verifiable bill without guessing the order.

The recommended operator entry is `/app/business` after one-time owner setup
in the [installation runbook](self-hosted-install.md). Configure the serving
route and customer sale terms, then the chosen collection/admission path. Customer
accounts use `/app` to obtain a permitted key and make a call against authorized
funding or entitlement. A fully entitled request can be ready with zero purchased
wallet balance; paid quota is not wallet cash.

The `/app/getting-started` checklist uses your personal customer scope: a live
visible key, a permitted chat model with effective customer sale terms, a
clearly labeled sample funding snapshot, and actual first-call receipt evidence.
It requires no deployment token or report share link. Free and fully entitled
samples need no positive wallet balance. Missing or transition-dependent facts
remain explicit, and a completed checklist does not certify a provider or
payment configuration. See [the sample funding guide](#check-your-model-and-sample-funding).

**Business → Setup** now has its own five checks (#843). The owner name from
initialization satisfies the identity step; saving a logo or recruiting another
staff member is optional. Configure a supported chat route with an active
credential, then publish an immediately effective default sale book covering
that model. Explicitly select and enable the checkout merchant in
**Business → Integrations** and review **Business → Collection** for compatible
currency and conversion. The process also needs the original encryption key and
a public return URL. An invitation workflow is an available admission path even
before the first customer joins, and does not require SMTP.

Refresh the checks after configuring a section. An unavailable read shows an
error; a fact outside your role shows **restricted** and points you to the owner.
An admin can edit supply and default sale terms but cannot select a merchant.
Finance can read sale/collection facts; support and viewer have fewer readable
facts. A partially visible checklist never claims that the whole business is
ready. These checks make no upstream, payment or email request.

A Chat turn is a real billable request. Its settlement proof is available on
Requests/Bills immediately. Reports and period exports cover closed UTC spans;
a first-turn verification does not require a public end-user report link. Keep
the expected hash and trusted key/head with the export for independent checking.

## Business provider connections

Sign in as the business owner, enter **Business → Channels**, and use **Create
or replace connection**. Choose a stable channel name, protocol and base URL.
This operation sets the connection by name: an existing name replaces its
credential pool. Open the existing row and choose **Edit connection** when
preserving its pool. Configuration saves make no upstream request.

Public connections require HTTPS and public addresses. An internal service
requires the owner's explicit grant for every exact IPv4/IPv6 address; use
neither hostnames nor CIDR ranges in that list. Reserved and metadata addresses
remain blocked. An endpoint/protocol change needs a fresh key and clears the
billing credential. Previously admitted calls keep their pinned configuration.

To rotate a key, add its replacement first, then disable/remove the old row.
Only last-four metadata is returned. Removing the last active key is refused;
explicitly disabling it stops new requests on this connection. Billing query
credentials can be set or cleared separately. Each write collects an audit
reason. Keep the dialog open after an unknown network outcome and retry its
unchanged fields to reuse the same operation. Editing begins a new operation;
closing discards the draft and its retry identity. Secret drafts are held only
in memory, cleared on success/close and never stored by the app.

**Check reachability** asks separately before one authenticated HEAD request.
It generates no model tokens and does not certify inference, upstream billing
or customer sale prices. A pending result may mean the check was interrupted;
replaying it does not send again. A deliberate new check is a new attempt.
Channel base prices are the legacy billing rates when customer sale terms are absent; the separate `upstream` fields record supplier cost. Customers with published sale terms retain that separate pricing authority.
Admin staff can read the redacted channel list; finance/support/viewer cannot
open it, and only owners can change connections/credentials or run HEAD checks.
## Business models, aliases and routing

Owner/admin staff open **Business → Channels → a channel** to manage model
versions, aliases and routing through their current business session. Use **Add
model** for a new route, or **Append version** to start from an existing model's
complete price set and weight. The route weight (1–1000) selects relative
preference among channels serving that model. Saving appends a new immutable
version and never sends inference. Previously admitted calls retain their
pinned version. **Version history** reads complete retained versions.

Base channel rates and supplier cost are distinct. Customer sale books take
precedence over legacy channel billing rates. Every base and conditional set
can contain cache-read/write, reasoning and per-request prices plus a separate
supplier-cost block. Blank optional billing dimensions use their documented
base-rate fallback; missing supplier cost is untracked, while explicit zero
means known zero. Credit input accepts up to six fractional digits. Values
outside the browser's safe integer range refuse editing rather than round.
Conditional service-tier names retain their exact text, including whitespace.
Each conditional rule replaces the complete set, including supplier costs;
the server refuses equally specific overlapping matches. Switching a mode
requires reviewing its output price and maximum for every set.

**Add alias** binds a unique public name to a non-event model already served
on that channel. Changing the mapping requires explicit removal and addition;
the removal confirmation names the affected alias. **Fallback order** loads
its existing chain before editing. Enter up to sixteen distinct model or alias
names, one per line in order, or leave empty to clear. Saving requires business
ownership; actual serving also checks protocol, mode and customer permissions.

**Edit cache policy** explicitly chooses unconfigured (disabled), disabled or
enabled. A bounded lifetime must be positive; no expiry is a separate deliberate
choice. Reuse remains limited to eligible whole primary responses with reported
usage and an exact admitted request/permission/route/sale context. System text
always participates. A cache hit has no new supplier inference and uses the
applicable cache rate. Configuration saves do not test a provider.

All writes require a reason. Keep an uncertain submission open and retry
unchanged to reuse its operation identity. Editing starts a new intent; closing
discards the draft. Catalog and routing capabilities enable these controls;
provider secrets and destination changes remain owner-only. Finance/support
cannot acquire provider administration through these controls.

## Business identity and customer help

As the business owner, open **Business → Business profile**. Review the public
name, contact email, help page, logo and accent, then choose **Edit profile**.
The contact address and help link appear on the login page and both portals.
They are public; they do not configure outgoing mail or confirm that mail works.
Use an absolute HTTP(S) URL without embedded passwords. Leave optional fields
blank to clear them, and use a dark accent that keeps white button text readable.

The first save stores all fields together. Later environment changes cannot
overwrite them. Before saving, the page identifies unsaved setup/deployment
defaults. Other staff can read the profile; only the owner sees the editor.

A concurrent edit refuses the stale draft. Review the latest profile after
**Discard draft and reload**, then reopen the editor. For a network failure,
keep the draft open and retry unchanged to recover the same operation. Editing
any field starts a new operation; discarding loses that draft's retry identity.
Changing a public name does not rewrite old bills, currency or payment terms.

## Dashboard

Purpose: see and manage the organization in a browser instead of over the API.

Open `/app` in a browser pointed at the server and log in with your email and
password; the app opens on the overview. Log out from the app.

The console speaks English and 中文: it opens in your browser's language
(Chinese browser variants use the shipped Simplified Chinese translation), and
the EN/中文 switch in the header flips it — the choice is remembered across
visits. Public pages outside `/app` (`/verify`, the report links) stay English:
a verification artifact is read against the ledger, not localized.

Business operators enter `/app/business` using an owner or staff session and
explicit Business context. Capabilities determine which controls are available.
The deployment-token console remains a technical compatibility surface for
operations whose Business editor has not shipped. In its **Channels** page,
**Model fallbacks** lets an authorized operator choose a served model, read
its current ordered chain, add served model names or aliases, move entries,
remove entries, and save the replacement. An empty chain saved through the
console clears the configuration. The gateway tries the requested model's
channels first and follows the chain only when they all fail; each fallback is
still checked against the caller's channel group and model policies. A fallback
chain is a routing preference, not a guarantee that the fallback will answer.

The login page drives every auth surface the contract offers: a GitHub button
and one button per configured OIDC provider (drawn from `auth/methods`), and
the TOTP second leg — an account with a confirmed factor sees a code field
after the password, trading the `challengeToken` plus a live or backup code
for the session. **Settings** carries the two-factor card: enroll shows the
base32 secret and `otpauthUrl`, confirm with a live code shows the ten backup
codes once, and disable needs a live code.

The dashboard is the React app under `web/` (issue #234): overview, top-up, orders, bills, requests, usage, pricing, keys, members, the transaction log, statements, webhooks, chat, settings and the admin console, built on the single component layer under `web/src/components/ui/` — Radix primitives and vendored shadcn sources, styled with the DESIGN.md palette (DESIGN.md) — all reading the same `/api/v1` surface. The server-rendered Topcoat pages retired to it (issue #256), so an old `/dashboard/*`, `/app/login` or `/admin` bookmark answers `301` to its `/app` twin — the page names below are the `/app` routes. `/verify` keeps its server-rendered form on purpose — when the wasm bundle ships, the page's loader runs the same check in your browser instead (issue #284).

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
  - **Invite a member**: enter the intended email for a new or existing account.
    Choose **Create invitation link**, or enable email delivery when the deployment
    has a mailer. Save the link; it expires in seven days. The recipient previews
    the account and role, then either signs in and accepts or registers through it.
    Registration shows the first API key once. Existing users select the joined
    customer from the account menu afterwards; their personal account stays intact.
    Retrying creation does not reveal the secret or resend mail. Revoke an unused
    link with `POST /api/v1/org/invitations/{id}/revoke` and `{idempotencyKey,reason}`.
  - **Make admin** / **Make member**: change a member's role. This never grants
    ownership; see below.
  - **Member budget**: the same member PATCH (`/api/v1/org/members/{userId}`)
    takes `budgetLimitMinor` — a cap on what the member may commit across every
    key they minted, settled charges and outstanding holds counted together.
    Send `null` to clear it. The members page does not render the control yet;
    the API enforces it.
  - **Team budget**: an owner or admin assigns `teamId` through the same member
    PATCH and manages caps through `/api/v1/org/teams`. Moving a member carries
    their keys' settled and reserved spend into the new roster. Dissolving a
    team retains its memberships and their own budgets. These changes take
    effect for later admissions; already admitted requests keep their
    reservations through settlement.
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
  Keep your offline verifier current with the server: complete quota/cache bills
  can carry longer evidence than older verifiers accept. An older reader's size
  error is not evidence of a changed bill; use the compatible verifier and your
  originally retained content hash.
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
  Above the list, **Search the request log** runs the same record as a query
  (issue #505): every request the organization made — settled *or* refused at
  admission — narrowed by key, the model that served it, the channel, a time
  window (`From`/`To`), the terminal status and a minimum charge. Each row names
  the model asked for and the model that answered (the two differ only when a
  fallback served it), the time to the first upstream byte and the whole turn's
  clock, the tokens and the charge, and links to the settlement entry's verify
  page. A refused request is in the log too — `refused`, charge 0, with the
  reason — so the search is the place "which requests were turned away" gets an
  answer without reading bills one by one. Results page fifty at a time on the
  **Older** button.
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
2. **Funding**: use a configured purpose-specific checkout or ask authorized
   staff to record a verified receipt. An applicable paid entitlement or
   promotional grant may already cover the request. A customer cannot grant
   purchased credit by calling `POST /api/v1/topups`. Payment success comes from
   authenticated provider evidence and fulfillment, not the return page.
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
browser when the WASM assets are installed. The form fallback executes on the
server; use the packed SDK for an independent offline check.

Notes:

- The conversation is kept in the browser only; the server stores nothing. Reloading
  the page restores the conversation and re-reads each turn's bill.
- A 402 on send means the freeze does not fit your balance: top up, or pick a
  cheaper moment. The gateway's message states the freeze and the balance.

## Keys

Purpose: several integrations, one organization, revocable separately.

In **API keys**, open **Constraints** to edit a key's budget, rate limits or
model allowlist. Saving preserves its required guardrail policies, tag-scoped
token limits, and request-group assignment. Clearing a field in this form
removes that field's limit; policy and scoped-token configuration can be
changed through the API below.

- `POST /api/v1/org/keys` mints another key, with an optional `name`, an optional RFC 3339 `expiresAt`, and optional constraints: `spendLimitMinor` (the most the key may have committed — settled charges plus outstanding holds, in minor units; null is unlimited), `budgetDuration` (`"daily"`, `"weekly"` or `"monthly"` makes the limit periodic over the UTC calendar window instead of cumulative; it needs a limit), `modelAllowlist` (the gateway models the key may call; null allows all), `requestsPerMinute` (requests the key may start inside a rolling minute; null is uncapped), `maxConcurrentHolds` (holds the key may keep open at once; null is uncapped), `requiredPolicies` (the guardrail policies the key's turns must run — every name must already exist as a policy the platform admin wrote; null requires none) and `groupId` (the request group the key routes through — a named channel subset the platform admin wrote; null leaves it ungrouped and reaching every channel). The secret comes back once.
- `GET /api/v1/org/keys` lists the organization's keys: id, name, display prefix, timestamps, and the constraint fields. Never a secret.
- `PATCH /api/v1/org/keys/{keyId}` replaces the constraint set — `spendLimitMinor`, `budgetDuration`, `modelAllowlist`, `requestsPerMinute`, `maxConcurrentHolds`, `requiredPolicies`, `groupId` — each cleared by `null`, under the same role rules as revoking. Ordinary constraints also clear on omission; `groupId` omission preserves its assignment. Grouped bearers cannot create or patch keys, and member sessions cannot change `groupId`. A hold that would push the key past its limit is refused with 429 `KEY_LIMIT_EXCEEDED` — on the gateway too, where it looks like OpenAI's `insufficient_quota` — so concurrent requests cannot exceed it. A monetary limit of 0 refuses positive wallet reservations; free or fully quota-funded calls still pass if the current committed amount is 0 and every other constraint allows them. A gateway request naming a model the allowlist does not carry is refused with 403 before upstream is contacted. A request past the key's minute allowance is refused 429 `RATE_LIMITED` with `Retry-After`, and a hold over the outstanding cap is refused 429 `TOO_MANY_HOLDS` until a settlement frees a slot.
- `DELETE /api/v1/org/keys/{keyId}` revokes one. It stops working immediately; the rest keep working.
- `GET /api/v1/org` describes the organization the credential belongs to.
- A key that is unknown, revoked or expired all answer the same 401 `UNAUTHORIZED`, so a leaked key's state is not revealed by probing.

### Delegating spend under a mandate

A mandate hands a bearer who does not hold the key a strictly narrower slice of it — a cap, an optional model subset, and a validity window — as a signed blob the delegate presents instead of the key's own authority being shared. Use it to let a script, a CI job or a sub-agent spend a bounded amount without ever seeing the key.

- `POST /api/v1/org/keys/{keyId}/mandates` mints one: `{"capMinor": …, "notAfter": "<rfc3339>"}`, plus an optional `models` allowlist, an optional `requiredPolicies` subset and a `notBefore` to delay the window. The answer carries the `mandateId` and the `mandateBlob` — the signed, base64url string the delegate sends. A mandate may only narrow the key: a cap above the key's `spendLimitMinor`, a model outside its `modelAllowlist`, a policy outside its `requiredPolicies`, or an inverted window is refused `VALIDATION_ERROR` rather than minted.
- `GET /api/v1/org/keys/{keyId}/mandates` lists what the key has issued — id, cap, models, window, creation time. Members list only the keys they created; a mandate id on someone else's key answers `NOT_FOUND`, so ids are not probeable.
- The delegate sends the blob in `X-Oxsum-Mandate: <blob>` on the gateway (`/v1/*`). A blob that fails to verify is 403 `INVALID_MANDATE`; outside its window is `MANDATE_NOT_YET_VALID`/`MANDATE_EXPIRED`; a hold that would push the mandate's committed spend past `capMinor` is 429 `MANDATE_EXCEEDED` — committed meaning settled charges plus still-open holds, counted from the ledger itself.
- A mandate is short-lived by design and is never revoked: to cut a delegate off, tighten the key's own constraint or revoke the key — the delegate's window just runs out.
- A settled delegated turn names the mandate in its settlement description (`{id, hash}` over the blob), so `/verify` proves which delegation authorized the charge.

### Running rules on a key's calls (guardrails)

Purpose: your organization has a rule some calls must obey — a phrase a prompt
must not use, PII an answer must not leak, an external screening service a
security team already runs — and you want the gateway to enforce it, not a
client you do not control.

The platform admin writes the rules; you attach them to a key.

- The admin writes a **policy** through `PUT /api/v1/admin/guardrail-policies/{name}`: `keyword` (`{"patterns": [{"re": "…", "action": "flag"|"block"}]}` — a prompt or answer that matches a `block` pattern is refused, a `flag` is only recorded), `pii` (`{}` — the built-in scan over the answer's text for email addresses, card numbers and the like; a match always flags, never rewrites), or `moderation` (`{"endpoint": "https://…", "timeoutMs": …}` plus `apiKey`, the endpoint's bearer — stored sealed like an upstream key, never read back). Every verdict a policy returns names the policy, so a bill can say which rule acted.
- The admin binds policies through `PUT /api/v1/admin/guardrail-bindings/channel/{name}` (every turn the channel leads runs them) or `…/key/{keyId}` (every key). You can also pin them per key: `requiredPolicies` on `POST /api/v1/org/keys` or `PATCH /api/v1/org/keys/{keyId}` demands the named policies on that key's turns, and a mandate's `requiredPolicies` must be a subset of its key's — a delegate cannot be handed a rule the key never agreed to.
- What a `block` does depends on when it lands. Before the hold (`pre_call`) the request is refused 403 and costs nothing — only the `refused` record marks it. On the completed answer (`post_call`) the turn is still refused 403, but the hold is **refunded in full**: the upstream answer already arrived and is paid for on upstream's side, so the exposure is your monitoring's, not the caller's. A streamed answer cannot be un-sent — a `block` that lands after streaming started is recorded on the settlement with `streamed: true` instead of refusing.
- Recorded flags and blocks identify policy, verdict and evaluation phase in the bill's `guardrail` list. Complete short reasons stay inline; longer reasons carry a digest and byte length, with full text retained separately. A proof binds the digest; comparison with the full text checks its exact identity. A pre-call refusal's marker and usage row remain best-effort. Streamed verdicts also carry `streamed: true`.
- A moderation endpoint that cannot answer fails **closed** before the hold (a screening service down must not admit an unscreened prompt) and **open** after it — the upstream cost is real either way, so an outage must not eat a settled turn. A policy the admin deletes while it is still bound is skipped rather than an error; update the binding when a policy retires.

### Asking whether a key can call now

Purpose: know a request would be refused *before* spending it — a client
checking headroom, or you debugging why a delegate's calls fail.

`GET /api/v1/org/keys/{keyId}/entitlement` answers `{allowed, reason?, …}` —
the same checks admission runs, read-only: it consumes no request slot, no
window slot and no hold, and is never itself rate-limited. `?model=` names the
model the request would call, `?estimate=` a hypothetical charge in minor
units, and repeated `?tag=name:value` the `metadata` tags it would carry. When
`allowed` is false, `reason` names the first check that refused —
`key_inactive`, `org_suspended`, `model_not_allowed`, `rpm_exceeded`,
`tpm_exceeded`, `budget_exhausted`, `concurrent_holds`, or the mandate's own
`mandate_invalid`/`mandate_window`/`mandate_model`/`mandate_cap` — and the
headroom fields (`remainingRpm`, `remainingTpm`, `remainingBudgetMinor`,
`concurrentHolds`, `retryAfterSecs`) say how far each constraint stands. The
`checks` object gives every constraint's verdict at once, so a debugging view
shows everything failing, not just the first refusal. Presenting a delegate's
`X-Oxsum-Mandate` blob asks the delegation's question too — including "why
does this blob not verify", which is the `mandate_invalid` answer, not an
error. The read is a snapshot, not a reservation: a racing request may spend
the headroom it reports, so admission's own checks stay authoritative. The
keys dashboard polls it per key — the Live column shows `ready`, the refusing
reason, or `off` for a revoked key.

## Top up

Purpose: add balance to the organization's wallet.

Checkout and code redemption require a personal session in customer context.

Steps:

1. Open Top up and choose a configured checkout or redeem a code. Credit arrives after payment verification.
2. For a bank payment, ask the business to verify and record its payment reference. Customers cannot manually credit their own balance; the former `POST /api/v1/topups` returns 403.
3. Save the credited transaction's `contentHash` for later verification.

Deployment administrators use `POST /api/v1/admin/organizations/{organizationId}/manual-receipts` with `idempotencyKey`, `amountMinor`, `reference` and `reason`. Repeat identical terms after an interrupted response; never invent a second reference for the same payment. Use adjustments for promotions. Amounts are credit minor units; display currency does not convert them.

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

The Hyperswitch rail (`OXSUM_HYPERSWITCH_*`; issues #319/#320) is the switch the
payment layer runs on: `POST /api/v1/topups/hyperswitch` answers `{"paymentId",
"clientSecret", "publishableKey", "depositId", "sdkUrl"}`. In the dashboard the
**Hyperswitch** tab mounts the switch's Unified Checkout in place — the page
loads `HyperLoader.js` from `sdkUrl` and binds a payment element to the intent —
so no card data ever touches oxsum. Confirming the payment redirects back to the
top-up page, which watches the balance until the switch's webhook credits it;
cancelling returns the same way and credits nothing. Programmatic callers use
the same handles with the SDK of their choice.

On a deployment that promotes a rail's environment credentials to an explicit
merchant (`OXSUM_<RAIL>_MERCHANT_ACCOUNT_ID` + `OXSUM_<RAIL>_MERCHANT_MODE`), the
checkout and its signed webhook run through scoped intake instead (issue #649):
the expected payment is pinned before the provider call, the callback verifies
under the path's merchant/version before the body is read, and an event only
credits when it exactly matches the pinned amount, currency and customer — a
mismatch is held for review rather than applied, and a replayed or out-of-order
delivery never credits twice. A rail without the bootstrap keeps its ordinary
unscoped behavior.

The order form (issue #606) pins what the payment buys before any provider call:
`POST /api/v1/orders` takes the quoted `displayAmountMinor`/`displayCurrency`,
the `purpose` — `walletTopup`, `bundle`, `planPeriod` or `invoiceRepayment` — and
the `idempotencyKey` that anchors it, and answers the provider checkout to
complete (`{order, providerPaymentId, clientSecret, checkoutUrl, publishableKey,
sdkUrl}`). The server determines the ledger amount from the published bundle
offer, the sold plan-period quote, or the Business collection policy for other
purposes. Your personal display preferences cannot change the purchased amount.
Repeating the call
with the same key answers the standing order and the same checkout rather than
charging again; paying on the provider's page and returning does not itself mark
the order paid — only the provider's signed webhook does, and the fulfillment
that credits the wallet, activates the plan or repays the receivable lands right
after, or on the recovery sweep if the process stopped in between. A paid event
whose amount disagrees with what the order pinned is held for the business to
review, never credited. `GET /api/v1/orders` lists your orders and
`GET /api/v1/orders/{orderId}/fulfillment` shows the state and what the
fulfillment granted.

The Orders pages (issue #607) are where that journey lives in the dashboard:
`/app/orders` lists every order and manual receipt newest first, filterable by
purpose and status; `/app/orders/new` pins a top-up in the Business collection
currency or a bundle at its published offer's price and currency, then lands
on `/app/orders/{id}`, which mounts the provider's checkout element from
`GET …/checkout-session` — the recovery
read a refreshed or returned browser polls — and shows the order's truth as it
moves `created → provider_pending → paid → fulfilled` (or `failed`, `expired`,
`needs_attention`). A provider's return URL lands on the list page with
`?order={id}` and bounces to the detail page; the return itself never marks
anything paid — only the signed webhook does. A staff-recorded receipt appears
in the same list with `source: manual` and its evidence on the detail page.

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

## Publish and buy a credit bundle

Owners and finance staff open **Business → Credit bundles**, choose **Publish
bundle**, and enter a name, fiat price/currency, purchased credits, promotional
credits and an optional validity in days. Money fields reject excess decimal
precision instead of rounding. Review the amounts and reason before publication.
Prices must have an exact currency-per-credit ratio representable with at most
eight decimal places; an incompatible ratio is rejected with a field error.

To change a price, select the existing offer family and publish a new version.
Previous versions remain on sale until you explicitly retire them. Retirement
confirms the named version and reason; orders already created retain their sold
terms. The history displays both on-sale and retired versions.

Customers select **Orders → New → Bundle**, then a published version. Price,
purchased credit, promotion and validity are shown before checkout. Confirmed
payment starts promotional validity; a delayed fulfillment or refresh never
restarts it. The order detail retains the sold snapshot after retirement.
Promotional credit cannot become a cash refund. A partial or full confirmed
refund cancels the remaining promotion, including unused promotional amounts
released by an in-flight request. Refund review waits until bundle fulfillment
completes. A failed or declined refund does not itself cancel promotion.

An old pending bundle with no published offer snapshot is readable but cannot
resume checkout or grant credit. Ask the operator to resolve that historic intent;
the upgrade does not guess its terms. Already fulfilled legacy bundles keep their
original grants and purchased refund eligibility.

## Paying for one AI call

There are two ways to pay: let the gateway do both halves for you, or drive the hold and the
settlement yourself. Both are billed the same way.

### Through the gateway (recommended)

Purpose: point an OpenAI, Anthropic or Gemini client at oxsum and have every turn frozen and charged for you.

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

   The newer Responses API is the same key and the same host: `POST /v1/responses`
   speaks the successor wire — `input` items, `instructions`, `max_output_tokens` —
   and lands on the same channels, billed the same way:

   ```python
   answer = client.responses.create(
       model="deepseek-chat",
       input="Explain holds in one sentence.",
       max_output_tokens=200,
   )
   ```

   `store=True` and `previous_response_id` forward to upstream untouched: the
   response's state lives in the provider's store, not in oxsum, so a stateful
   chain works only where upstream keeps it.

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

   Or Google's SDK — `POST /v1beta/models/{model}:generateContent` and
   `:streamGenerateContent` speak the Gemini wire, served by channels whose protocol
   is `gemini`. Gemini puts the model in the path and streaming in the action, so the
   body carries neither: `stream: true` inside it is refused, and a `model` field may
   only repeat the path's. Attribution rides the request as `labels` — the same
   bounded string-map `metadata` is on the other surfaces:

   ```python
   import google.generativeai as genai

   genai.configure(api_key="oxs-…", transport="rest",
                   client_options={"api_endpoint": "127.0.0.1:3000"})
   model = genai.GenerativeModel("gemini-flash")
   answer = model.generate_content("Explain holds in one sentence.")
   ```

   The call travels to upstream as `x-goog-api-key`, the one credential spelling AI
   Studio and Vertex both accept.

   Each surface serves only the channels that speak its protocol: a model behind an
   `openai` channel is not served on `/v1/messages` or `/v1beta`, and the reverse —
   there is no translation between the three. Errors on `/v1` surfaces come back in
   the surface's own envelope, and the oxsum key travels as `x-api-key`, exactly as
   the Anthropic SDK sends it.

   The same key also opens the input-only surfaces. Embeddings go to
   `POST /v1/embeddings` in OpenAI's shape — `input` may be a string, an array of
   strings, or token arrays — and reranking to `POST /v1/rerank` in the shape Jina
   and Cohere speak (`query` plus `documents`). Neither streams, and neither is a
   chat price: a model is served there only when a channel prices it with
   `mode: embeddings` or `mode: rerank`. The freeze reserves the input bound alone —
   token arrays freeze at their exact count — and the turn settles against
   upstream's usage report, estimated from the request's input when none arrives.

   Long-running work goes to `POST /v1/tasks` — image renders, video generation,
   batch jobs on a `tasks`-protocol channel. The submit freezes `maxCostMinor` (or
   the price's `costPerRequest` when none is named) and answers
   `{taskId, submitted}`; the job then runs on a periodic pass that asks upstream
   every 30s and settles the hold when the state goes terminal. The caller polls
   `GET /v1/tasks/{taskId}` — a row read, never a second upstream call — or
   subscribes to `task.succeeded`/`task.failed` webhooks; `POST
   /v1/tasks/{taskId}/cancel` ends the billing side best-effort.

2. Every response carries an `x-oxsum-request-id` header. That id is how the turn is found in the
   ledger: its entries are keyed `req-<id>:hold` and the settlement derived from it
   (`oxsum_core::settlement_key_for`).

   The cost rides along too: `x-oxsum-freeze-minor` is the most the request can charge.
   Read account balance in your personal session; it is not returned to a consumption key. Once the turn
   settles, `x-oxsum-charged-minor` reports what it actually cost — a header on a
   non-streamed answer or a post-hold refusal, and an HTTP trailer (`Trailer:
   x-oxsum-charged-minor` announces it) on a streamed one, since the response head is sent
   before the stream settles. `x-oxsum-cache: hit` says the answer came from the response
   cache rather than upstream: on a channel whose operator enabled it, an exact repeat of an
   earlier request re-serves the stored answer and bills the cache-read rate instead of a
   fresh turn's. The match is exact — the same complete request and admitted context (text,
   tools, parameters, key permissions and route), not a similarity score; the settlement's
   `cache` block carries `similarity: 1.0` and names the original turn's entry, so the replay
   is provable on `/verify` like every other charge.
   Cached answers never cross organizations or sibling consumption keys, a `stream` request
   is never cached, and a
   hit's `upstreamAttempts` reads `0` because no upstream call happened.

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
route, or waited out a short `Retry-After` on the only route left; the channel named is always the
one that actually answered. A version is never rewritten, so a price
change affects later calls only: a bill written today can still be checked against the configuration
it was written under, however often the price changes afterwards.

A request may also serve under a *different model* than it named: the platform configures an ordered
fallback chain per model (issue #500), so when every route of the model you asked for fails, the
request tries the next model in the chain — each fallback resolved through its own channels and
priced by its own book, under the same one hold. A fallback your key's `modelAllowlist`, a presented
mandate's `models` or your organization's tier does not cover is skipped rather than served: your
permission to reach a model is re-judged per hop, never inherited. The settlement records
`servedModel` — what answered and priced the turn — and `model` means the same served name for
readers that predate the field; on a cross-model failover it also records `requestedModel`, the name
you asked for.

### Reservation and settlement authority

Send consumption through the gateway. It computes the upper bound, reserves credit and settles the recorded usage. Customer sessions and consumption keys cannot call `POST /api/v1/holds` or `POST /api/v1/settlements`: both return 403 `FORBIDDEN`, including requests to release a known hold or choose its charge.

Business services that report externally measured usage use the separate service credential and `/api/v1/metering` contract. Customer credentials cannot become metering authority. `GET /api/v1/holds` still lists in-flight reservations. Historical raw holds remain recoverable by the sweeper; contact the business about an unresolved reservation.

## Balance

Purpose: check the currently available balance.

Step: `GET /api/v1/balance`. The returned `availableMinor` already subtracts unsettled holds.
The number is one balance: promotional credit in live lots, granted credit
(signup bonus, admin grants), purchased credit (top-ups) and an organization's
credit line are separate pools inside the ledger, the pools are drawn in that
order — perishable money first — and the API sums the four.

When the platform admin has granted the organization a credit limit,
`creditLimitMinor` and `creditUsedMinor` come along: the limit is spendable credit
that keeps `availableMinor` positive past what the organization has paid in, a hold
draws it only after granted and purchased credit run out, and a top-up repays the
drawn line before it adds purchased balance. The admin lowers the limit only after
the drawn part is repaid — shrinking below the debt is refused, and a top-up is how
the debt shrinks.

If the platform suspends the organization, the balance still reads and money still
lands — but new holds refuse `FORBIDDEN`, so new gateway calls
fail until the operator reinstates it. Holds already open still settle, and a
subscribed webhook hears `org.suspended` at the moment it happens.

## Statements

Purpose: read the monthly bill for what the credit line carried.

Issuing a monthly statement commits its original figures, proof anchor and
notification together. If issuance fails, the draft remains available for retry.
A retry after success returns the same issued document. A retained legacy proof
anchor naming different source inputs refuses issuance and needs explicit
operator recovery; editing the bill does not repair that historical proof.


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
A verified payment or authorized manual receipt while a statement is outstanding repays it. `paymentStatus`
reads `pending` until the due date has passed (then `overdue`), `suspended` while
the platform holds the bill, and `paid` once repayments cover it. When the
platform has opted the organization into dunning, a statement unpaid past
`dueDate` plus the configured grace also suspends the organization's *spend* —
new holds refuse `FORBIDDEN` until the line is repaid and the daily pass
reinstates it — so paying an overdue bill is what restores the key's calls.

## Subscription plans

Quota-funded and free calls still obey the key's current permissions, customer
manual/dunning/archive restrictions, mandate time window and concurrency limit.
Plan-statement dunning separately restricts wallet overage; existing paid quota
remains usable. A quota-funded call occupies one concurrent slot until settlement,
cancellation or timeout recovery. A failed admission returns its entire quota
reservation, so topping up and retrying does not pay for the refusal twice.

The money budgets on keys, members, teams and mandates count the existing wallet,
bonus and credit-line spend. Subscription quota is not counted again. Exactly
reaching a cap still permits a call needing no wallet money; an already-exceeded
cap refuses. Explicit zero-priced terms require no credit and consume no quota.
The resulting receipt proves the recorded zero charge without adding wallet
balance or usage revenue.

Purpose: pay a fixed price for a bundle of credits that renews on a cadence —
the plan the operator bound to your organization draws first, and the wallet
only pays what the period's quota could not.

A subscription plan (issue #496) is a per-period credit quota, not a prepaid
balance: quota spends never move a ledger account — they run the period's
counter against the quota the plan carried when the binding was made (a plan
edit later does not re-meter a binding already running). While a subscription
is `active`, every gateway request draws from it first, up to the freeze, and
the wallet holds only the remainder — a request the quota covers in full takes
no wallet hold at all, so a zero-balance organization on a working plan still
serves and its bill carries a zero-cost receipt in place of a charge (issue
#602); on a plan with no `quotaMinor` at all the subscription admits every
draw in full and the wallet never sees that traffic.
`GET /api/v1/org/subscription` answers the live binding (`consumedMinor`
against the snapshotted `quotaMinor`, the `currentPeriodEnd` it rolls at, and
whether it may overflow), or `data: null` while none is bound.

When the period's quota is exhausted, the plan's `allowWalletOverflow` decides:
`true` holds the overflow against the wallet as usual, `false` refuses the
request with `402`. The quota resets on the plan's `resetPeriod` — daily at the
next UTC midnight, weekly on Monday 00:00 UTC, monthly on the first — by
closing the running period `boundary` and opening a fresh one, lazily on the
first request past the boundary and hourly from the platform's rollover job;
a `never` plan's counter runs until the subscription is cancelled — nothing
rolls, nothing expires. Every boundary, cancel and plan change files a
`subscription_periods` row, and `GET /api/v1/org/subscription/periods` lists
the history — open period first — so you can read what each period drew
against the quota it ran under.

On a deployment that offers Stripe checkout, buying a plan is
`POST /api/v1/org/subscription/checkout` with `{"planId"}`: the answer's
`checkoutUrl` opens the hosted payment page, and the webhook binds the plan
once Stripe reports the session complete — nothing meters until then. Plans
without a `stripePriceId` are bound by the operator only. The bill still
verifies: a settled turn's record names the `subscriptionId` and `periodId`
its draw ran under beside `drawnMinor`/`chargedMinor`, so what the period paid
is provable, not just claimed.

## Paid plans (sold catalogue)

In **Business → Plans**, enter price and quota as credit amounts with at most
six decimal places. Zero is explicit; a blank quota means unmetered usage.
Amounts above 9007199254.740991 credits, extra decimal places, negative values
and exponent notation are refused without publishing or clearing the draft.
The form never rounds an entered price or quota into another sold version.

Purpose: buy the plan a business actually sells — a priced entitlement with a
real lifecycle, not an operator-bound quota.

A business publishes its catalogue as immutable versions
(`GET /api/v1/business/plans` for staff; `POST` mints a new version, never
rewrites a sold one). `GET /api/v1/plans` is the on-sale read any credential
may make: each entry is one version's terms — `priceMinor`, `quotaMinor` per
period, `periodKind` (`calendarMonth` ends at the next UTC month boundary;
`anniversary` rolls on the activation date's day-of-month, clamped into
shorter months — a January 31 plan renews February 28, then March 31), and
`overageEnabled`, the opt-in that lets an exhausted period draw past quota
against the wallet.

The business assigns the plan, then you choose when to pay:

1. `PUT /api/v1/business/customers/{organizationId}/plan` with
   `{"planVersionId"}` binds you — `pendingPayment` for a priced plan, active
   immediately for a free one bound with `{"complimentaryGrant": "<uuid>"}`.
2. Open `/app/plans` and review the quoted plan payment. The page obtains
   your own period quote, shows its exact sold fee and provider currency,
   and sends the unchanged fingerprint with the order. You do not enter an
   internal plan UUID or choose how much entitlement to buy. The first
   service period starts when paid activation completes; the provider return
   alone never activates it. If a response is lost, refresh availability and
   resume the existing order instead of creating another payment.

The quoted fiat amount rounds up to the smallest supported currency unit;
the credit-denominated sold fee remains exact. An API client reads
`GET /api/v1/org/plan/payment-quote`, then posts `purpose: "planPeriod"`,
`purposeRef: terms.customerPlanId`, `expectedPlanQuote: fingerprint`,
`expectedCollectionRevision` and the exact quoted fiat fields to `/api/v1/orders`.
A changed quote conflicts before any provider request. Historical orders
retain their original terms, including after conversion changes.

A period only ever exists because its authorization does — the rollover timer
can never open an unpaid one. Manual renewal becomes payable only at the
current UTC boundary; paying cannot cut an active period short. An unresolved
period payment must finish before cancelling or changing the assignment.
When the boundary passes without a paid
renewal the plan goes `pastDue`: the ended period's unspent remainder still
serves your requests, but no new quota issues until a renewal order
fulfills — and that period chains off the previous end, so a late renewal
does not skip the gap. `POST /api/v1/org/plan/cancel` marks
`cancelAtPeriodEnd` — the paid period still serves to its end, then the plan
closes without renewing. A request at or after that boundary cannot use the
cancelled period's remaining quota, even before the rollover worker runs; it
must pass the usual wallet admission checks. A request already admitted before
the boundary still settles against its original period. `POST /api/v1/org/plan/change` books a swap to
another version at the next boundary; there is no mid-period pro-rating.

Each period leaves an invoice journal (v2 at paid activation, legacy v1 at close) — the bill for what that period
cost (issue #611). `GET /api/v1/org/plan/journals` lists yours newest first
and `GET /api/v1/org/plan/journals/{journalId}` reads one with its lines:
`baseFee` the version's price, `discount` a credit note or complimentary
write-off,
`promoCredit` promo that covered the fee — `totalMinor` their sum. A journal
is `posted` while it still owes (the unpaid receivable your `pastDue` plan
carries), `paid` once a `planPeriod` order's payment settles it, `void` if
staff cancelled it. A legacy v1 fee bills in arrears: your order prepays the period
it authorizes and the close bills it. New v2 versions issue the paid invoice
at activation and recognize income only as UTC service days complete. Each renewal fee funds that renewal
period in full. Existing journal debt stays visible and separate; the explicit
statement repayment workflow is tracked by #777. Partial prepayment coverage
at close is also shown on the customer statement, so the paid base fee does
not appear as debt again.
Usage paid from wallet funds or an approved credit line is settled with its
request and is never charged again by a new period close. A historical journal
may still show `usageOverage`; operators must use an explicit correction for
that duplicated charge (#809), preserving the original record.

If an older bill charged already-settled usage again, owner/finance staff can
use the [historical correction command](plan-usage-correction.md) after reviewing
its original evidence. Your bill then shows a negative usage correction. Any
amount already collected beyond the corrected bill becomes refund payable;
the payment line states that cash has not yet been returned. Original billing
lines remain visible, and unsupported combined histories require finance review.

The journal is the accounting bill; the statement is the document you
actually read (issue #612). `GET /api/v1/org/statements` lists yours newest
first and `GET /api/v1/org/statements/{statementId}` reads one with its
lines — `planFee`, `usage`, `discount`, `refundCredit` plus the
payment-cycle `paymentApplied`/`disputeChargeback` — `totalMinor` what the
period billed, `paidMinor` what your payments have covered, `dueAt` the
date payment is due (`netTermsDays` after issue). A statement is `issued`
while it waits, `paid` once its own allocations cover it, `pastDue`
past its due date, `void` if staff cancelled it. An unpaid statement past
`dueAt + graceDays` suspends your plan's new holds — the plan reads
`pastDue` the same way. Payment, a grace extension or financial void clears
debt suspension only when no other debt remains past grace. The overdue pass uses the current unpaid balance and due
date under the same plan/statement locks as lifecycle changes: a payment,
void or grace extension committed before that check is respected. A busy
statement is retried on a later pass while other customers can progress.
New wallet admission checks effective debt past grace immediately, for current
statements as well as retained legacy voids, even before that pass stamps a flag.
Staff may extend the selected statement's due date or financially void it.
A financial void reverses the matching original journal and statement in one
commit: remaining debt clears, Cash stays unchanged, and collected funds become
refund payable. This action does not send a refund or renew service. A bill
mixing another journal's chargeback, a reissue or inconsistent sources refuses
with a financial-reconciliation explanation. Historical document-only voids
retain their original lines and remain included in debt and spending checks;
a new explicit void request can reconcile the proven backing journal.
Replaying the same target, action, reason and key makes no second change.
`GET /api/v1/org/plan` carries the `credit` block beside
the binding: `creditLimitMinor` the facility's cap (null when uncapped),
`outstandingMinor` the remaining statement debt plus posted journals without a
live statement, `nextDueAt` the earliest effective debt due date. A retained
void uses its old due date; a journal without a document uses its posting time
plus the original binding's current configured net terms. Admission additionally counts current holds. A plan over its credit
limit refuses a wallet-billed overage draw — subscription-drawn quota is
never refused.

## Webhooks

Purpose: be notified when money moves — a request settles, a deposit lands,
the balance crosses a line, a statement turns — without polling the API.

Steps:

1. Register an endpoint: `POST /api/v1/webhooks` with
   `{"url": "https://your-server/hook", "events": ["request.settled"], "balanceLowMinor": 2000000}`
   — pick the events it should hear (`request.settled`, `org.suspended`,
   `deposit.credited`, `deposit.failed`, `deposit.reversed`,
   `deposit.disputed`, `balance.low`,
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
owner/admin session. A consumption key sees only its own rows, and a member session sees its own keys' rows plus shared unattributed history, excluding account-wide document markers.

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
2. Open `/verify` in the browser — no login needed — paste the bundle and the `contentHash` you saved, and submit. When the deployment ships the wasm bundle (the usual case), the check runs entirely in your browser: the page loads the same `verify_bundle` the server runs compiled to WebAssembly, so a passed check does not depend on trusting the server, and the bundle never leaves your machine. A browser without the wasm bundle — or JavaScript off — posts the same fields to the server, which runs the identical check and renders the verdict. The page's "Try an example bill" link (`/verify?sample`) pre-fills a captured public test fixture shared with the offline verifier example. It demonstrates the check without reading any customer's ledger and is labeled as evidence from a test environment, not this deployment (issue #637).
3. Read the verdict. "Verification passed" means the bundle matches the content hash and the inclusion proof links it to the tree head — and, for a usage settlement, one more line: the charge inside the entry is recomputed from the usage and rates it records (`charged = ceil(Σ units × pricePerM ÷ 1,000,000)`, capped at the freeze). A settlement written before descriptions were versioned, or by a schema newer than the page's verifier, reports that inclusion passed but the arithmetic is too old or too new to recompute — it is skipped, not guessed at. "The charge does not add up" means the entry is proven but its own numbers disagree with each other, which an honest settlement never does. "Verification failed" means something changed: altering any single number in the bundle fails the check. A bundle that does not parse, or a hash that is not 64 hex characters, gets its own error instead.

A passed check on a settlement also reports how the bill's *price* fared. The operator signs a checkpoint per `(channel, model, priceVersion)` — the same key that signs tree heads, under the origin `oxsum/price-catalog/<channel>` — and a fresh bundle carries that checkpoint in its `price` field: the canonical row the server hashed, the note attesting it, and the verifying key. "Bound to a checkpoint the operator signed" means the price your bill names is the catalog row the operator attested — a forged current price table cannot produce it. "No signed price binding" is information, not failure: a top-up names no price, a bundle made before this shipped carries none, and a deployment without `OXSUM_HEAD_SIGNING_KEY` signs nothing. A binding that *does not verify* is its own sentence — the note names a different price or its signature fails — and that never happens on an honest bill. You can also fetch any version's checkpoint yourself at `GET /api/v1/log/price-head?channel=<name>&model=<name>[&version=<n>]`; signing is deterministic, so an old version's note is identical whenever it is asked for.

When a bill priced under your *customer sale terms* (a sale book rather than the raw channel catalog), the report adds a `sale` line beside `price`: the operator signs a checkpoint of the resolved customer snapshot under `oxsum/sale-catalog/<business>`, and the bundle carries it in its `sale` field. The snapshot binds only what you agreed to pay — your organization, model, the book and window the terms came from, and the sale price plus discount — never the supplier's own rates, so the evidence cannot leak what the operator paid upstream. "Bound to a checkpoint the operator signed" means the customer terms your bill names are the snapshot the operator attested. A bill that priced under the wholesale catalog instead reports no sale binding at all — and deliberately so: an old channel checkpoint is never offered as proof of your customer terms. You can fetch the checkpoint yourself at `GET /api/v1/log/sale-head?digest=<hex>` (the value your bill commits) or `?organizationId=<uuid>&model=<name>[&at=<rfc3339>]`.

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

## Exporting a whole period's evidence (v2 bundle)

Purpose: hand an auditor — or your own archive — one file that commits *everything* a closed billing period contains, not just one bill or one customer's subset.

A version-2 evidence bundle (issue #616) is minted against a fully ended `[from, to]` day span or against one plan statement, and it carries every artifact the scope commits: usage records, plan statements, invoice journals, payment orders and fulfillments, refunds, disputes, credit notes, and the day's seal digests. The ledger commits the bundle's *manifest* — the list of every artifact's digest — through a marker entry, once ever per span: re-minting the same period replays the standing bundle, so the committed set can never grow a second version.

Minting and downloading is an owner/admin session surface (`POST /api/v1/org/evidence-bundles` with `{"from","to"}` to mint, `GET /api/v1/org/evidence-bundles` to list, `GET /api/v1/org/evidence-bundles/{bundleId}` to download the frozen bytes; `GET /api/v1/org/statements/{statementId}/evidence` for the single-statement export). Sharing works like bill links but is stricter: `POST /api/v1/org/evidence-bundles/{bundleId}/share` mints an `oxe-…` token you may bound with `expiresAt` and `maxDownloads`; the recipient downloads through `GET /api/v1/evidence/{token}` with no account, and `DELETE` on the same path revokes. Expired, revoked, exhausted and guessed tokens all answer `410`, and every download is counted — `GET …/share` shows `downloadCount` and `lastDownloadAt`.

The check runs offline with `verifyEvidenceBundle` from the `verify-oxsum-bill` package (or `oxsum_verify::verify_evidence_bundle` in Rust): every artifact the manifest names re-digests to the committed figure, the manifest re-digests to what the marker bound, the money rows' ledger entries sit inside the supplied head, and a present note verifies under the selected operator key. An unsigned bundle verifies digests/inclusion only; inspect signature presence rather than interpreting a digest pass as an authenticated attestation. A dropped row, a changed amount or a bundle minted for a different organization each fail by name. The bundle's `scope` block states what it does *not* cover — the platform's own supplier costs (redacted unless the operator sets `OXSUM_EVIDENCE_UPSTREAM_COST`; `costBasis` always exports), other organizations' rows, history outside the span, and in-flight operations. See [Verifying an oxsum bill](verify-your-bill.md) for the customer-facing walkthrough.

## Platform admin

Purpose: manage the deployment itself — the upstream channels the gateway relays to and the price each model charges.

The admin pages are not part of the dashboard: they open with the deployment's operator token (`OXSUM_ADMIN_TOKEN`), not with a login, and they are reachable only in a deployment that configured one.

Steps:

1. Open `/app/admin` in the browser and enter the operator token. The page proves it against the real surface before remembering it — a refused token is reported, not stored — and keeps it in the browser's `localStorage`, the same place the chat page's key lives. A `401` on any later call forgets it and asks again.
2. Open **Channels**. The table lists every channel's name, upstream address, protocol, upstream key suffix (the only part ever read back), and model count.
3. **Save channel** writes a channel: a name that already exists is repointed to the new address and key, and its price history stays where it is.
4. Choose **Prices** on a channel, enter the model and billing mode, then **Set price**. For `chat`, both token rates and a positive whole-number **Maximum output tokens** are required. The ceiling clamps the caller's output limit and sizes the initial credit reservation. Rates and the optional per-request fee are entered in credits and sent as integer minor units. The input-only `embeddings` and `rerank` modes omit output fields; `event` permits an output rate but has no output ceiling.
5. Prices are append-only — a change is `v{n+1}`, never a rewrite. Read current prices through **Pricing** and full history through `GET /api/v1/admin/channels/{channelName}/prices`. That history is what a bill's `priceVersion` is checked against later. A model may be priced on several channels: each route carries a `weight` (1–1000, default 100) — the relative preference for leading a request — so failover between channels is configured by pricing the same model again elsewhere, and the API takes the weight beside the price set. The API can say more than the form today: a price set may also carry cached-input, cache-write (5m and 1h) and reasoning rates, the upstream's own prices, and conditional `rules` — a `match` on `serviceTier` or an input-token window swaps in a whole different set, most-specific-wins; see `PriceRequest` in `openapi.yaml`.

Note: a request resolves its price when it starts and carries the version to its settlement, so a price change lands on later requests only; in-flight turns and old bills are untouched.

Upstream-price drift is also watched automatically where the channel is another oxsum deployment: with `OXSUM_UPSTREAM_PRICE_SYNC` set (and `OXSUM_SECRET_KEY` present to unseal the channel credentials), a daily job probes each channel's `/api/v1/pricing`, and a stored `upstream` rate the live catalog contradicts opens a drift row — broadcasting `anomaly.detected` and listing in the reconciliation report — that resolves itself once the rates agree again. Every probe records an outcome (`ok`, `unreachable`, `invalid`, `auth`), so a channel that stops answering is visible too.

The organizations page the `organizations` section lists every organization the ledger holds money for — its name, kind (personal or team), headcount, and what its wallet shows: the available balance and the frozen sum of its outstanding holds. Each row carries an "Adjust" form: a signed amount in credits (a leading `-` deducts) and a required reason, which the new entry stores as its description — part of what a bill's proof covers. The same card carries a **Credit notes** form: a positive amount and a required reason, posting one ledger entry (Revenue debited, Wallet credited) and moving no money — a credit note is not a refund, and the list beneath the form shows what was issued. A deduction the balance cannot carry is refused with `INSUFFICIENT_FUNDS`; nothing in the page ever rewrites history.

Traffic splitting inside one organization runs through **request groups** (issue #508). A platform operator manages them under Admin → Channels: select an organization, name a group, and choose channels by name. Saving replaces the whole set; an empty set denies every channel. The Keys page lists group names and their effective channel names, and an owner or admin can assign a group when creating a key or change/clear it from that key's Access control. Ungrouped keys reach every channel. Deleting a group assigned to any key returns 409; reassign or explicitly ungroup those keys first. The API exposes the same operations through `GET /api/v1/admin/groups?organizationId=`, `PUT`/`DELETE /api/v1/admin/organizations/{organizationId}/groups/{name}`, and the read-only `GET /api/v1/org/groups`.

A group is a named subset of the organization's channels — "the paid OpenAI channel" versus "the cheaper Ollama mirror". The API writes `{channelIds: […]}` as the complete set atomically and checks every id against real channels. A key's `groupId` on `POST /api/v1/org/keys` or `PATCH /api/v1/org/keys/{keyId}` limits gateway requests to that group's channels, while `modelAllowlist` limits models. Grouped bearers cannot create or patch keys; member sessions cannot change `groupId`; trusted member sessions are not isolated by these bearer restrictions. Key creation/assignment and constraint updates are atomic, and new requests read saved scope while in-flight requests keep their admission snapshot. PATCH omission preserves `groupId`; explicit `null` ungroups. The group binds nothing into a bill — a settlement names the channel that actually served, which is the fact a proof must carry; which group admitted the request is operational detail, not a billing credential.

A deployment can also start each new organization's wallet with credits: `OXSUM_SIGNUP_BONUS_MINOR` (default 0) grants that amount at self-registration, booked as an adjustment whose reason is "signup bonus". An invited account joins an existing organization and is granted nothing.

A forgotten password resets itself when the deployment sends mail: the login page's "Forgot your password?" opens `/forgot-password`, the mailed link lands on `/reset-password`, and setting the new password revokes every session the account held — the admin reset's semantics, self-served. The forgot endpoint answers the same whether or not the address has an account, so it reveals nothing. A deployment without a mailer keeps the operator path: `POST /api/v1/admin/users/{userId}/password-reset` with `{"newPassword": "…"}` sets the replacement and revokes every session the user held at once; the answer's `sessionsRevoked` says how many died with it.

Guardrails are configured from **Admin → Guardrails** (issue #562) instead of by hand against the API. The page has two halves. **Policies** creates or replaces one named rule — `keyword` with one or more regex patterns, each `flag` or `block`; `pii`, the built-in scan over the answer's text that always flags; or `moderation`, an absolute http(s) endpoint plus an optional timeout and a bearer that is sealed on write and never shown again (leave the bearer blank when editing to keep the stored one). **Bindings** replaces the ordered policy set for a channel (every turn that channel leads) or for a key id (every turn of that key); the key id comes from the customer's Keys page, because the admin API has no key list. Saving a binding takes effect on the next turn.

What a block does depends on when it lands, and the page says so rather than implying otherwise. Before the hold (`pre_call`) the request is refused 403 and costs nothing. On the completed answer (`post_call`) the hold is refunded in full and the turn is still refused — the upstream answer already exists. A turn already streaming cannot be un-sent: its verdict is only recorded, marked `streamed` on the settlement's `guardrail` list. Deleting a policy that a binding still names is allowed; turns skip the dangling name (fail-open) until the binding is updated, which is why the delete dialog points back at the bindings.

The in-flight page the `in-flight` section lists every hold the platform is currently reserving, across all organizations and newest first — the organization, the request id, the model and channel, the price version the turn froze at, the frozen amount and when it opened. A hold that settled leaves the list; one older than the hold timeout (30 minutes) is one the sweeper is about to release as `swept`. A hold whose sweep keeps failing is marked `dead` after ten failed attempts — it retries hourly from then on, and the reconciliation page lists it under `holds_dead_lettered` for an operator to follow up.

The requests page (the `requests` section) is the operator's all-organization read of the same log the customer's Requests page shows, settled or refused. It narrows to one organization (or by clicking a row's organization id), a model, a channel, a status and a minimum charge, and to a time window; the newest page answers first and **Older** pages back. Each row names the request, the model asked for and the one that answered, the channel, its status and error kind, the token counts and the charge — and a row that names its entry links to `/verify` for that entry's proof. It only reads: nothing on this page writes or repairs.

The anomalies page the `anomalies` section reviews the settled turns that did not price cleanly, across all organizations and newest first: an `estimated` turn where upstream reported no usage, a `client_cancelled` one where the caller left mid-stream, a `capped` one whose upstream usage priced above the freeze, a `swept` one whose hold the sweeper had to release, and an `unpriced` one whose usage named a dimension the price book cannot bill — the computable part was charged and the rest is the platform's flagged loss, never a silent zero. Every row is the settlement record that turn's own ledger holds — organization, request id, model, channel, the price version in force, the charge and what had been frozen — plus a `score` (0–100) and a `severity` band (`low`/`medium`/`high`) the row earns from what it already records: how hard the charge pressed the freeze, what it cost the platform upstream, whether it had to fail over, and whether the same request id has shown up anomalous before (issue #263). The endpoint filters on the band (`?severity=`) and sorts by it (`?sort=score`), so the worst leaks surface first; scoring is read-only and never changes what was charged. A summary table on top counts the anomalous turns and their charged total per channel: where the platform is losing money upstream is the first thing the page answers.

The margin page (the `margin` section) puts the two sides of the business side by side, per channel and model: what organizations were charged, the `upstream` cost the same usage priced at, the margin between (`marginMinor`), the provider's own bill once the `upstream-bill-sync` pass has reconciled the channel (`not reconciled` means no bill pass has touched it yet, not that it billed nothing), and `untrackedTurns` — settled turns with no upstream cost recorded. A totals row sums the model-scoped figures and counts each channel's provider bill once, because the API repeats the channel-scoped figure on every model row. The same numbers answer over the API: `GET /api/v1/admin/margin` sums what organizations were charged against what upstream cost the platform, per channel and model, and reports the difference as `marginMinor`. `untrackedTurns` counts the settled turns no `upstream` price block covered (a swept turn, a price without one, or history predating the column): read it as the coverage gap the margin is blind to, because an untracked turn is never a free one — the number to watch before trusting `marginMinor`. `providerBilledMinor` is what the provider actually billed the channel — the `upstream-bill-sync` job's billed fact against `upstreamCostMinor`'s estimate — channel-scoped, so the same figure repeats on each of that channel's model rows and `null` means the pass has never reconciled the channel, not that the provider billed nothing.

The deposits page the `deposits` section lists the money-in record newest first — when, which organization, rail, status, the amount the deposit was opened for, what the rail reported paid when it differs, the rail's reference and the ledger entry once credited — filterable to one organization. A `credited` row carries a **Reverse** action (issue #356, the ledger half of a refund): it asks for a required reason — the reversal entry's description — and posts one ledger entry that debits the wallet and credits cash, flipping the row to `reversed` with its own entry named beside the credit's. One reversal per deposit ever, so a retried click answers the standing reversal rather than debiting twice; a reversal the wallet cannot cover is refused (`INSUFFICIENT_FUNDS`) when the customer already spent the credit. Reversing moves no money off-platform — the PSP refund call is a separate step — so the books are right before the rail is asked for anything.

For Hyperswitch, the same action queues bounded recovery using the payment's
original merchant version. `unknown` means no matching external handle has been
confirmed; `pending` means a handle is known, but no exact signed terminal event
has arrived. `succeeded` and `failed` describe authenticated external outcomes.
The local row being `reversed` does not establish a successful external refund.
Retries and restarts retain the same request identity. After five automatic
attempts the unresolved status remains visible; inspect the retained merchant
inbox and switch records before taking a separate provider action. A late exact
callback still updates the outcome. Old unbound refunds require reconciliation;
repeatedly clicking Reverse cannot create a new operation for them.


The reconciliation page the `reconciliation` section answers whether the projections still match the books: the scan reads every organization's ledger against the usage rows, the deposits and the hold watches, and lists each drift class that counted nonzero — a usage row whose settlement entry is missing, a settled turn that wrote no record, a deposit marked credited with no entry behind it, a rail that paid a different amount than expected, a deposit confirmed but never credited, a hold watch with no reservation behind it, a pending hold the sweeper cannot see, a hold whose sweep failed ten times and was dead-lettered, a periodic job run that exhausted its attempts, a log whose positions are not dense, and — at platform scope — a stored `upstream` rate the channel's live catalog contradicts (`upstream_price_drift`, sampled as `channel:model:field`). Each class carries its count and a bounded sample of identifiers to follow: click the class to list every offender the scan kept (up to twenty, with "showing N of M" when the count is larger), and a class that counted zero says `none detected` rather than an empty cell. The classes now include `anchored_day_altered`: a day leaf that no longer folds to the map root its daily anchor committed — a settlement that bypassed the sealing rule, or a hand edit. Settlements into an anchored day are refused with `409 DAY_SEALED`, so the class should stay empty; it exists as the check that the seal held. `clean` means every class counted zero. The scan reports and never repairs — drift is for the operator to act on, not for a second writer to patch silently.

The closing page the `closing` section runs the monthly closing: pick a month that has fully ended and close it — the month is sealed in every organization's ledger, which produces its closing record (a seal committing to the log's tree head and the month's closing trial balance, chained onto the seal before it) and refuses any new entry dated into it from then on. The table lists the sealed months newest first: organization, period, how many entries it covers, the log's size at sealing, and the three hashes the record is made of. Closing a month twice is safe — the second run answers the records that already exist.

The audit page (the `audit` section) lists every mutating call this surface ever took, newest first — one row per write naming the action (`channel.set`, `discount.create`, `statement.finalize`, …), what it acted on, and the request fields safe to show. Credentials are never in it — a channel's `apiKey`, a new password and a minted batch's codes stay out by construction — and a retried keyed write audits once. The page shows the newest page; the same rows answer over the API at `GET /api/v1/admin/audit`, where `?action=` narrows to one call kind and `?limit=`/`?cursor=` page further.

Billing a service of your own uses an owner-issued integration credential. In business context the owner calls `POST /api/v1/business/integration-credentials` with `idempotencyKey`, `name`, `scopes: ["meteringWrite"]`, explicit `customerIds`, `expiresAt` within 365 days and a `reason`. Save the returned `oxs-int-…` secret once; a retry returns null secret. List metadata with `GET` on the collection, revoke through `POST /{credentialId}/revoke` with an idempotency key and reason. Add `customersRead` only when the service needs selected customer profile list/detail. No credential grants personal or gateway access. See [the runnable example](../examples/scoped-integration/README.md).

A price the service bills by is an owned channel's `mode: event` version under a `billableCode`. `/api/v1/metering/holds` freezes a bound and returns an opaque `holdKey`; save that exact value. `settle` charges observed usage at the original price, capped by the freeze; `release` closes at zero; `settlements` bills an already-completed event once. Retry the same request with the same credential; changed input conflicts. Only the originating credential can close or replay its hold. `eventId` is a label and never selects another customer's funds. Legacy global service credentials are disabled at upgrade: ask the owner for a scoped replacement. Previously outstanding legacy holds remain recoverable by the sweeper.

## Accepting initial business ownership (#629)

If the deployment administrator has invited you as the initial business owner, use their setup token within 30 minutes. Existing customers log in and accept with their own session; a new owner registers using the invited email and a new password. The token chooses the intended owner, so owning a customer organization alone does not grant business access. Setup asks for the business name and records invite-only policy. It cannot be claimed twice. The customer account and its API key remain separate from staff authority; the staff console and role/session workflow are dependent release tasks.

## Business staff API workflow (#630)

For normal onboarding, the owner opens **Business → Staff → Invite staff by
email**, selects the role and intended address, and shares the returned link or
requests one email. Before consent, the recipient sees the business name and
the invited staff role. A new recipient registers a staff-only identity; an
existing user signs in and explicitly accepts. Neither path creates customer
access, an API key or funds. After acceptance, open the Business console. The
owner can revoke an unaccepted invitation or remove the accepted staff seat;
replaying an accepted invitation does not restore a revoked role.
Business section links such as `/app/business#business/customers` retain the
selected section when reloaded or opened directly. Current role capabilities
still determine which section is available.

After initial setup, sign in using the ordinary password/TOTP flow. Select business context with `POST /api/v1/session/context` and `{"idempotencyKey":"select-business-1","context":"business"}`. Read `/api/v1/business` to see your business role and capabilities. The staff console at `/app/business` (issue #615) drives these APIs from the browser — the sidebar shows the console's item once your session holds a business principal, the console itself switches your session into business context when it opens, and each section is gated on the capability your role carries.

The owner can appoint an existing registered user with `POST /api/v1/business/staff`, passing `idempotencyKey`, `userId`, `role` (`admin`, `finance`, `support`, or `viewer`) and `reason`. Staff status is separate from customer team membership. Use `/business/staff/revoke` to remove staff access. Use `/business/ownership` with the successor's existing staff user ID to transfer ownership; your role then becomes admin. Transfer before deleting an owner account.

Only the business owner can open self-registration: `POST /api/v1/business/registration` with `idempotencyKey`, `mode: "open"` and a reason. `mode: "invite"` closes it again. Fresh and upgraded deployments begin closed; changing `OXSUM_SIGNUP` does not change this policy. Password/GitHub/OIDC registration follow the same setting, while existing users can still log in.

Select customer context before visiting customer balance/team/bill APIs, using `context: "customer"` and your own `organizationId` when needed. Business context grants no customer impersonation. If you enable TOTP, sign in again with password and the second factor before using business operations; an older session or an OAuth-only login cannot supply that proof. Revoked staff can still return to their own customer membership.

## The business console at /app/business (#615)

The staff console is a sectioned surface inside the same dashboard, not a second app: once a session selects business context the sidebar gains a **Business** item, and the console shows your staff role beside the business name on every page. Sections group by job — **operations** (a setup checklist for an uninitialized business, customers, payments, refunds, disputes), **receivables** (statements, the aging list, reports, the reconciliation-exception work list), **catalog** (channels, prices, plans) and **access** (staff, integrations, audit). The nav hides a section your role's capabilities cannot serve — support sees customers and the overview, finance sees the money surfaces, viewer sees the reports — but the API still enforces every capability, so a hidden item is convenience, never the guard.

Every mutating control on the console — a refund decision, a journal void, an exception close or repair, a staff appointment, a credential mint — asks for a reason and sends a fresh idempotency key, and the answer lands in `finance_audit` (or the resource's own audit) before the page confirms it; the audit section lists those rows through `GET /api/v1/business/audit` with an action filter. Reads page with load-more cursors; every id in a table is a real foreign key you can drill — a refund row names its order and its customer, a statement names the journal and organization it covers.


## Manage customer profiles and admission (#633)

In business context, list `/api/v1/business/customers` and read a customer by `organizationId`. Owner/admin can PATCH its display `name` and `contactEmail` with an idempotency key and reason. The external ID is a permanent reference: renaming a customer does not move keys, bills or balance.

POST `/api/v1/business/customers/{organizationId}/lifecycle` with `state: "archived"` to stop new consumption while keeping history and debt. Use `state: "active"` to restore it. POST the corresponding `/suspension` with `suspended: true` or `false` to control the manual reason; support staff may perform this action. Each request also requires `idempotencyKey` and `reason`.

Customer state reports manual and dunning timestamps separately. Clearing a manual restriction or restoring an archived account does not clear overdue suspension. Paying an overdue bill can lift dunning while a separate manual or archive restriction still prevents new consumption. Existing requests still settle; the customer can read bills and make payments during restrictions. The console's `/app/business` customers section lists every customer, drills into a detail (profile, live plan binding, statements and orders) and carries the lifecycle/suspension actions with their required reasons.

### Inviting a new customer as business staff

In business context, owner/admin calls `POST /api/v1/business/customer-invitations` with a unique `idempotencyKey`, intended `email`, customer `name`, stable `externalId` and a `reason`. Save the returned token/link for the intended customer; this API does not mail it. The invitation reserves its identity for seven days, and the customer account is created only when the recipient consents. New users register; existing users sign in and accept. They become the initial customer owner, never business staff. Revoke before acceptance through `/business/customer-invitations/{id}/revoke` with a new idempotency key and reason. After acceptance, use customer lifecycle or membership operations.


### Updating clients for scoped consumption keys (#637)

Keep your existing key for SDK inference and its own receipts, usage and entitlement preview. Sign in personally to create or revoke keys, change constraints, mint mandates, view account balance, start checkout or redeem a code. Customer-wide statements, reports/share links, experiments and webhook configuration require an owner/admin session. A consumption key receives 403 on these account operations; a sibling or foreign evidence/task ID returns 404.

Use the same consumption key to poll/cancel an asynchronous task and to retry an inference request. Replays also require the same protocol, path model and body. After upgrading, retry IDs whose records used the old fingerprint conflict until their original 24-hour expiry; choose a new ID only for a genuinely new operation, never to blindly repeat uncertain inference. Response caches warm separately for each key. Existing balances and downloaded proofs remain valid.


### Manage business provider configuration

The current business owner can use the [business provider API](api.md#business-provider-configuration-apiv1businesschannels) to add or rotate an encrypted provider pool and billing credential. Owner/admin can configure models, aliases, cache and fallback order. Select business context first; customer and integration keys do not authorize these operations. Supply a saved idempotency key and reason, and retry the same body if the connection is interrupted. The returned receipt stays stable. Repointing a channel requires explicitly supplied provider keys and clears its old billing credential; existing request snapshots remain unchanged. Saving configuration makes no external call. The connection policy and explicit check are described below; the console's `/app/business` channels section mounts these writes.


For an internal provider, explicitly list its server IP addresses in the owner connection request's `destinationPolicy`. Public providers use HTTPS by default. A changed destination needs fresh keys and fresh internal approval. Use `POST /api/v1/business/channels/{name}/connection-check` with a reason and saved idempotency key to run a HEAD connection check; saving does not contact the provider. A reachable response confirms only connectivity. Redirects are refused, and interrupted pending checks do not repeat on retry. Model tests may incur provider cost and remain deliberate separate actions.

### Model visibility and routing permissions

Your model list and price catalog show routes allowed by your customer tier. A consumption key can narrow that list with its own model allowlist and request group. To use an alias with a restricted key, include both its public name and the provider model it resolves to; the request group must permit the alias's channel. Experiments and fallback models need the same grants. The bill retains the original requested and actually served names. Generic asynchronous tasks currently use direct task model names and do not accept signed spend mandates; see the [protocol matrix](roadmap/self-hosted-v1/PROTOCOL-MATRIX.md) for the current scope.


### Prepare customer sale books

Open **Business → Prices** in business context, or use the [sale-book API](api.md#customer-sale-books-apiv1businesssale-books-599). Owner/admin creates a named book, saves its full model map with `expectedRevision: 0` for the first draft, and publishes that exact draft revision. Later edits save a new revision; the previous publication is unchanged. Each model has customer rates and one optional percentage discount. Provider costs belong to provider configuration and are refused here.

Schedule a published version as the business default, or have owner/finance assign a version to one customer. Owner/finance may override an individual customer model. Previews use the override, then the assigned book, then the default for a missing model. Supply normalized usage to see line items, the matched conditional rule, the selected terms and the final integer charge. A draft preview can compare a proposed change before publishing.

Use a future `effectiveFrom` to schedule a change. `replaces` ends an existing schedule of the same scope at that instant and starts its replacement atomically. To cancel a future schedule, append an `endSchedule` at its start. Saves cannot backdate or overlap terms. Save each mutation's idempotency key; retry the same request after a connection failure instead of issuing a new change. An already ended schedule cannot be extended or have its end rewritten.

The editor shows the loaded draft revision and retains its input on conflicts or
network failures. Retry an unchanged intent with the same form; changing its
payload creates a new action identity. Loading the latest saved draft asks before
replacing local edits. Rates are entered in credits with at most six fractional
digits, parsed exactly into integer minor units. Blank optional cache/reasoning
rates use their documented base-rate fallback; explicit zero stays free.
Conditional matches replace the complete rate set and are edited separately.

Publication names the exact saved draft. It does not activate prices. Use
**Activation and customer terms** to select an immutable version, UTC start/end
and any schedule to replace. The dropdown includes defaults from other books;
customer selection loads its assignments and model overrides. Ending/canceling
a schedule confirms the named window. All boundaries display UTC; an open-ended
window is only active after its start, and its end is exclusive.

The preview shows the selected customer, model, instant, source, book/version,
schedule, condition, discount and exact charge. Compare two customers to confirm
intentional differences. A successful preview sends no model request and does
not reserve funds. Gateway admission pins its resolved sale snapshot; later
edits cannot reprice an already admitted turn. Exported settlement evidence
carries the associated signed sale binding. Historical channel-priced entries
keep their original pricing identities.

### Configure a business payment merchant

In business context, open **Integrations → Add merchant** as the owner. Choose a name, provider account ID, rail and test/live mode; identity fields cannot change after creation. Hyperswitch with the Stripe connector is the primary path. Supply the API base URL, write-only API and webhook secrets, and the fields for the chosen rail. Hyperswitch additionally requires the public key and public HTTPS SDK script URL; configure its SDK API backend and allowed browser origin in the switch deployment too. Airwallex additionally requires its write-only client ID and public HTTPS checkout URL. Direct Stripe uses API and webhook credentials only. Internal API destinations require explicit IP allowlisting. Configuration starts disabled and reports local validation only: saving makes no network or payment test. Preserve the deployment's `OXSUM_SECRET_KEY` backup.

Open a merchant's name to inspect its paginated versions and event inbox. The owner can **Add credential version**, supplying every secret again; redacted reads never populate a credential placeholder. Rotation retains checkout enablement and all old callback versions. **Enable future checkout** and **Disable future checkout** require an audit reason and explicit confirmation. Each version shows its callback URL under the current site origin; configure the exact URL at the provider and ensure that origin is reachable. Retain old callback coordinates for in-flight orders. Settings, version history and events are readable by owner/admin/finance/viewer; only the owner sees integration-key controls and merchant mutations.

An unchanged failed form retries with the same idempotency key. Editing creates a new intent; closing or losing owner permission clears its secret draft. The equivalent [merchant API](api.md#business-merchant-configuration-apiv1businessmerchants) accepts explicit keys for scripted recovery. In the merchant detail, **Checkout selection** shows the selected merchant for
its rail. Select this enabled merchant or disable the whole rail, then confirm
the old/new merchant, test/live mode and reason. Creation/enablement alone does
not change the selection. An untouched rail retains its environment bootstrap;
a disabled explicit selection blocks new checkout without fallback. The legacy
callback URL continues to identify its original bootstrap merchant.

Customers select an available route for wallet/bundle and plan-period payments.
Test routes use sandbox money; live routes may charge real money. The selected
merchant/version remains fixed through background refreshes and stale choices
are shown explicitly. An unchanged retry recovers the original full payment
payload and key even if terms or the offer later change. Editing or explicitly
choosing another route starts a new intent; inspect existing orders first if a
previous outcome is unknown. Existing orders retain their pinned versions and
callbacks. Availability is not browser/PSP certification. Explicit statement
repayment remains the separate #777 work unit.

### Switching between staff and customer authority

Opening the Business console selects the signed-in staff member's business
authority. The business shell shows staff identity and capability-filtered
sections, without requesting customer balances or opening a customer billing
socket. Use **Customer portal** to select the existing customer membership
again. A staff identity without a customer membership sees the backend's
refusal and remains in the console; it does not gain a customer account.

The order detail refreshes its paid and fulfilled state automatically while a
checkout is active. The provider checkout stays mounted while its handles are
unchanged. On narrow screens, scroll inside a data table to reach amounts and
row actions; the table region can receive keyboard focus. The staff setup
checklist lists only steps the current role can read. Customer detail shows
financial tables only to staff with reporting authority.

Verified payment callbacks durably mark orders paid. The leader-protected
`payment-fulfillment` job runs at startup and every five seconds (plus the
worker poll interval), recovering accepted events and draining up to 100 paid
orders through the existing idempotent fulfillment writer. Customer credit
therefore becomes available promptly without waiting for daily reconciliation.
An interrupted pass retries from durable state; the order-derived ledger key
prevents duplicate credit. Daily reconciliation remains an additional fallback.


### A refund awaiting provider reconciliation

A refund can remain `provider_pending` with no `nextRetryAt` after five recovery
attempts. Its amount is still reserved and its credit note remains issued. Do
not start another refund to work around a lost response. Finance should retain
the order, refund and merchant/version IDs, inspect the exact provider operation
in that merchant account, and arrange a signed terminal event/replay using the
retained credential version. A missing merchant credential or an HTTP timeout
does not prove that money was not returned. A historical operation without an
external ID requires provider-side reconciliation before any new operation.


### Set collection terms before accepting payment

Open **Business → Collection settings** as the owner. Enter the settlement code
and the exact currency amount paid for one credit, for example USD and 1 for the
isolated fixture. Review the retained payment currencies, enter a reason and
save. The page shows its revision and whether terms are compatible with history.
Other staff can read the settings. Customer display preferences remain cosmetic.

Checkout and manual receipts show these collection terms before submission. A
changed quote returns an error and preserves the entered amount; refresh the
terms, review them, then retry. An unchanged retry keeps its request identity.
Existing orders keep their original rate. Currency with payment history cannot
be relabeled; mixed historical currency requires reconciliation. A saved rate
alone does not mean the payment provider is working. Current provider amounts
use two decimal minor digits; zero/six-decimal currency support is not implied.

### Understanding a plan refund

A confirmed plan refund records the full returned cash. Before a period closes,
it reduces that period's prepaid fee; its closing bill includes the refund once.
After close, the original statement shows a refund credit and a reversal of the
returned part of its payment. The bill and amount paid decrease together, so
other outstanding debt is unchanged. A voided bill's refund pays the money
already owed back to you. Quota is not a cash wallet grant.

Finance can review source-linked historical corrections using the
[plan-refund recovery runbook](plan-refund-recovery.md). A refund that intersects
an already-lost payment dispute can retain an explicit amount needing review;
that status is not proof that every part has been allocated to the bill.


### Retrying or reviewing a payment dispute

Keep a failed resolution's fields unchanged to retry its same operation. Changing
its outcome or reason under the same key returns a conflict. A completed dispute
cannot be changed to another terminal outcome. New decisions commit the money,
documents and audit together, so an audit failure does not leave an unrecorded
cash effect.

If resolution reports that ledger or audit history requires source review, retain
the dispute/order/merchant identifiers and follow the
[review procedure](dispute-resolution-review.md). Do not create a replacement
payment, dispute or refund to clear the error. New wallet/bundle/credit-repayment
losses record the complete Cash loss. Only funds demonstrably retained from that
original Wallet top-up are taken back. Previously spent money, credit repayment
and uncertain source history require review in the finance audit; the application
does not guess a replacement debt. A lost order cannot receive new refund approval
or an unperformed fulfillment grant. Bundle promotion is cancelled without cash
reimbursement. Historical wrong-direction entries use the [exact local chargeback
correction](nonplan-chargeback-recovery.md), which previews and compensates a proven
source while preserving the original debt and history. Non-plan refund corrections use the [exact recovery workflow](nonplan-refund-recovery.md); retrying a dispute resolution does not perform either correction.

The dispute list displays credit amounts in `cr` and shows the original collection
currency separately in the expanded detail. The current Close action ends the
case without moving funds; it is not the Lost financial resolution.

### Reading daily-plan financial facts

The plan page states its revenue-recognition policy. Existing v1 versions remain at period close; an explicitly selected v2 version recognizes completed UTC service days. A paid invoice shows the fee collected for its full interval. Its paid state does not mean the business has already earned the whole fee.

Business Reports separates net billed, recognized income, deferred movement, collection, cash returned, noncash discounts and receivable. Select booking, service or payment-cohort dates to inspect the documented source clock. Without a date filter, deferred is the remaining net deferred fee; with a filter it is the signed change inside that interval. Refunds recompute the net fee's earned/deferred split and leave the authorized service period and quota unchanged. A financial void leaves returned money payable until an actual refund occurs.

Statement evidence includes the listed daily effects and original policy inputs captured at export. Saved exports stay identical when later days or refunds occur. They establish integrity of captured facts and ledger entries, not proof that the service was delivered or that the export contains future events.

Owners configuring an embedded application can create an application through
`/api/v1/business/applications`, choose its retained customer limit and issue a
credential selecting that `applicationId`. Select only required scopes; leave
`customerIds` empty for application credentials. The owner portal exposes these choices in Business → Integrations. Keep each application's credentials in its server,
never in a browser or its customers' devices.

Provision through `/api/v1/integration/customers` with a stable operation key and
external customer ID. Save the returned organization ID in the application's
store. A replacement credential for the same application retrieves the same
mapping and retries the original operation. Archive requires the separate
`customersLifecycleWrite` scope and a reason; it preserves balances and records
while blocking new calls. A provisioning retry returns its original receipt, so
use the GET route for current archived/suspended state. Application metering
retries preserve the original operation across credential replacement as well.

Owners manage application namespaces and integration credentials from **Business →
Integrations**. Create or select an application, choose only the scopes its runtime
needs, set an expiry and provide a reason. The application account limit includes
archived accounts. Existing-customer grants use named selections; legacy
provision-only grants remain separate and do not survive credential rotation as
an application namespace does.

The form freezes the selected identities, scopes, absolute expiry, reason and
operation key when submitted. Retry sends that same request. Starting a new
operation is deliberate: if the previous response was lost, inspect issued
credentials first to avoid keeping unwanted authority. Credential secrets appear
only in the first successful response and are not saved by the portal. If a retry
returns metadata without a secret and you did not retain the original secret,
revoke that credential and issue a replacement. Refreshing a page does not
reissue a credential. The list distinguishes active, expired and revoked records.


### Retain and check a customer ledger head

On **Bills**, the **Retained ledger trust** card records the current customer's
organization, ledger origin, operator public key, signed head and timestamp after
local WASM signature verification. Storage is isolated by site, organization and
ledger. The former unscoped `oxsum.head-archive` is deliberately not imported.
Compare the full public key through a separate trusted channel: fetching and
checking a key on the same server establishes only a first observation.

**Check consistency** checks the original signature, the current signature and,
when the log has grown, the actual consistency proof against the original root.
A successful HTTP request alone cannot produce a pass. Changed keys/origins,
rollback, same-size forks, invalid proofs and missing WASM fail visibly. An empty
retained log can establish a signature but cannot anchor a previous history.
Checks preserve the original record; another tab cannot silently replace it.

**Download retained inputs** saves the original record and the latest checked
head/proof for the [offline guide](verify-your-bill.md). No browser-stored key or
newly served verifier proves an independently trusted distribution or honest
upstream metering. Clearing browser data loses this local record; keep the export
outside the browser. Storage errors leave verification/retention visibly failed.


### Download period and plan-statement evidence

Open **Evidence** from the customer navigation or Bills. Customer owners/admins
can browse frozen bundles, select inclusive UTC dates over at most 366 ended
days, and explicitly freeze a period. This appends a marker to the ledger; the
same organization/date span always replays its original bundle. After an
ambiguous network response, **Retrieve or retry this span** keeps the original
dates. **Choose another span** starts a distinct request. The page never creates
share links or sends mail implicitly.

An actual plan statement links to **Download and verify statement evidence**.
The explicit action freezes or retrieves that issued document's bundle; it does
not run automatically on navigation, and an open statement cannot be exported.

Select **Download bundle JSON** to keep the frozen evidence, and **Download
expected inputs** to retain its organization, expected origin, bundle id,
manifest digest and available trust record separately. Bundle bytes are retained
without rewriting integer values through JavaScript. **Verify locally** executes
the existing WASM evidence checks plus the customer's expected signed-head origin:

- A retained key verifies the signature under the original customer trust record.
- Without retained trust, a bundle-supplied key is explicitly labeled as supplied
  by that bundle; it does not authenticate itself independently.
- An unsigned bundle can establish committed digests/inclusion only, even if a
  key was previously retained. Missing or invalid WASM never becomes a pass.

The page shows the committed scope and exclusions. It does not establish every
settlement's price arithmetic, honest upstream usage, unprovided supplier books,
historical continuity or witness independence. Keep the bundle and expected
inputs outside the browser and follow [the offline guide](verify-your-bill.md).

### Confirmed cash beside an old refund state

The order's refunds and business refund/credit-note lists show a **Confirmed cash
return** when the retained provider event has a complete ledger effect. The old
approval or note state can remain declined, failed, released or reversed after a
late success. Finance review does not authorize another payout; the recorded
cash still consumes the original order's refundable amount.

Purchased credit can be reclaimed only from the original retained funding.
Amounts already used, held or originally used to repay a credit line are shown
for source review; later top-ups are not substituted. A historical correction
may separately show purchased credit restored. That restores a proved erroneous
Wallet debit and does not mean the provider sent a second cash refund.

Operators reviewing historical missing or wrong-source effects use the
[exact non-plan refund recovery workflow](nonplan-refund-recovery.md), with a
current owner/finance session and trusted event receipt. A source-review refusal
must be investigated using original evidence; new payment/refund identities do
not repair it.

### Compare financial sources with the books

In **Business → Reports**, choose the start/end dates and booking, service or
payment clock, then request a reconciliation. Finance write authority is required
to start a run; reports readers can inspect it. A failed request can be retried
with its same original dates, clock and request identity.

Open a run and select a **Retained attempt**. The table compares captured source
assertions with amounts reconstructed from verified official ledger postings.
Read the coverage description before interpreting a match: old v1 journals use
their original posting cohort; v2 uses signed movements under the chosen clock.
The legacy statement-issued-date receivable filter on the financial-facts
screen has a different cohort. Excluded fields are listed explicitly. The
source evidence expands to show exact entries, dates and differences, including
separate refund-payable and suspense amounts.

Matching totals do not remove source exceptions. In particular, an older zero-money
service day may have no retained sequence proving whether it preceded a refund.
The affected period's recognition schedule then appears in exclusions with
incomplete coverage; neither the run nor its evidence claims a verified allocation.

The **Current exception work list** records review work. Its resolution state
can change after the selected attempt, while that attempt's original differences
remain in captured evidence. Fix the exact underlying source through its
supported audited workflow, then request a new comparison. Reconciliation
itself never changes money or invents missing source links. Historical reports
marked **Legacy report** did not perform an independent ledger comparison.

In **Business → Staff → Application access**, the **Issue and revoke bounded
customer gateway keys** scope is available only for a selected application. A
service uses it for customer accounts it provisioned, not existing unrelated
portal accounts. Required key limits include a finite cumulative budget, allowed
models, request/concurrency caps and expiry. Secrets are returned once; exact
retries give metadata without plaintext. Retrieve the operation receipt, revoke
the old key and deliberately create a replacement if the first secret response
was lost. Revoking the management grant stops future management but does not
silently revoke keys already issued; those keep their own expiry/revocation.
### Long policy reasons

A version-15 bill may show a reason commitment instead of inline text when the
complete explanation is long. The hash and byte length identify the separately
retained full text; an ordinary proof check establishes the committed bytes,
not that the policy's judgment was correct. An over-capacity guardrail setup is
refused before the model call. See [the evidence contract](gateway-evidence-budget.md).
### Read a corrected historical chargeback

Expand a business dispute row to see its **Confirmed chargeback loss**, original
false cash receipt cancelled and total appended correction. The total combines
both components and is not a second provider loss. Original purchased credit,
review suspense and the correction entry are shown separately. The original
entry, terminal resolution and debt documents are preserved, including the old
credit benefit that still requires finance review.

Authorized owner/finance operators use the [exact local chargeback workflow](nonplan-chargeback-recovery.md)
with one retained dispute receipt. Preview and inspect the complete source before
applying its unchanged revision. A refusal needs source investigation; another
key, replacement order or changed terminal outcome does not repair history.


### Guardrail refund and request fees (#899)

A whole-response guardrail refusal, including a cache hit, charges zero customer usage and zero fixed request fee and releases the entire wallet reservation and subscription draw. Streamed content already delivered remains billed; a successful zero-token answer still owes its configured request fee. A customer refund does not imply zero provider cost: retained provider usage remains on the actual attempt and prices operator cost; absent provider counts remain untracked. A refused cache hit makes no new provider attempt. Exact request replay retains the same receipt and does not repeat a charge or refund.
## Verify your first successful call

On **Get started**, the request and bill steps become ready when a successful
call in your current customer scope has consistent receipt evidence. Free and
fully entitled calls count. Later failed requests do not hide an earlier success.
Choose **Verify this receipt** to open its proof and content hash, then run the
verification. The page explains that this freshly loaded hash comes from the
same server; retain evidence and trusted heads for later independent comparison.

No closed-day report or share link is needed for this step. Period reports are
available separately after their UTC period ends. If evidence is incomplete or a
read fails, the checklist shows a failed check and never a completed step;
refresh to retry. Members see only their permitted history.
### Application entitlement and customer sale reads (#898)

An owner can explicitly grant application-only `entitlementsRead`. Existing
credentials gain no permission. `GET /api/v1/integration/customers/{organization_id}/entitlement`
returns recorded customer restrictions, sold-plan lifecycle, quota window/counters
and shared plan debt. Recorded remainder includes pending draws and is never a
promise of admission after cancellation, expiry or a restriction.

`GET /api/v1/integration/customers/{organization_id}/models` filters configured
public models by current customer, alias-target and optional application-issued
key/group permissions before pagination (`limit` 1–50, default 20; `after` is the
last public name). `keyId` must be live and belong to this exact application and
customer. Effective published customer sale terms use override, assigned-book,
then default precedence. `saleTerms: null` means unavailable, without substituting
wholesale channel prices. Provider names, destinations, secrets and costs stay
private. Every page chooses its own database snapshot.

These reads never reserve, roll quota, refresh dunning, publish a sale digest,
write a bill or contact a provider. They show current recorded/configured facts;
actual request admission remains authoritative. Personal sessions and consumption
keys cannot use these application management routes.

### Accepting an application billing account (#920)

Open the link supplied by your application and sign in to your own oxsum identity.
Verify the exact recipient email in account settings if prompted, then return to
the link and refresh. Complete a new second-factor login when required. Review
the application name, customer account ID, recipient and retained application
access, select the confirmation checkbox, then accept ownership. You can sign
out from the consent page to choose another identity.

Your current account remains selected. Use the account menu to select the linked
account. Its existing funds and history stay with that account; other personal
accounts are not exposed to the application. The application retains the scoped
permissions previously granted by the business. A repeated acceptance is a
receipt and cannot restore subsequently removed membership. The link is retained
only in this browser tab for at most seven days while signing in; acceptance or
Discard removes it. If tab storage is disabled, reopen the original link after
sign-in. The application must replace a lost, revoked or expired invitation.

### Granting application evidence access (#924)

In Business → application access, select the application and explicitly choose
“Read original frozen customer evidence” when the integration needs it. The grant
covers only accounts created in that application. It lets the integration list and
download already published customer-safe evidence; it does not publish evidence
or grant staff authority. Revoke the management credential to stop future reads.
Files that include supplier costs or fail retention checks require operator
recovery and are never silently changed for the application.
### Pay a plan statement's debt

Open **Billing**, choose the statement and review **Statement repayment**.
The card shows outstanding debt, pending payments and the amount available to
pay. Leave the amount blank for full payment, or enter a partial credit amount
and choose **Update amount**. Review the exact credit amount and fiat price
before continuing to checkout. No wallet credit or plan renewal is included.

After a lost response, retry the unchanged request or resume the existing
payment listed on the card. Refreshing the page preserves the server's pending
order. A timeout or cancelled browser page does not release the reservation.
The order page distinguishes applied debt from late funds retained for refund
review. A source-review message keeps the debt visible and requires operator
review; repeated payment attempts cannot repair inconsistent history.

To request a refund, use the payment order's existing refund request control.
Finance approval and confirmed provider success restore only that payment's
original debt, or consume its own payable after a financial void. Repeated
callbacks do not repeat cash or debt changes. Clearing overdue debt removes
only the associated debt restriction; an independent manual suspension or
cancelled plan remains unchanged.

Finance opens **Payments**, expands a statement-repayment order and follows
its statement and original-journal links without entering IDs. Those exact
documents open independently of list pagination. The expanded order also shows
each incoming payment, refund and dispute allocation with its original source,
Cash/debt/payable/review amounts and canonical entry hash. Reconciliation snapshots
retain per-allocation entries, hashes, original return links and review suspense.
A source-exclusive journal and its chargeback follow-up documents can be voided
together after all pending payment intents are resolved. For a mixed statement,
select the original journal and use the source-specific correction described
below; the whole-statement void action continues to refuse mixed sources.

### Correct one source in a mixed statement

In the finance journal view, open the original journal that should be cancelled
and use its reasoned void action. The statement may contain chargebacks from
several periods: the action cancels only the selected journal's lines and keeps
other periods' debt and repayment history. A document with remaining sources
stays visible and payable as appropriate. Resolve any pending checkout on that
document first; a timeout or browser cancellation is not proof of nonpayment.

Voiding records refund payable for collected money; use the existing refund
workflow to return cash. The payment's original allocation determines which
source is restored or refunded. A new bill is a separate operator decision; void
does not issue another charge, renew a plan or remove manual account restrictions.

### Allow an external application to initiate payment

In application access, select the explicit checkout grant only when that
application should create wallet, bundle, assigned-plan or named-statement
checkout for its own customers. It
can quote and recover those orders; it cannot mark payments successful, write a
manual receipt, approve refunds or read merchant secrets. A linked personal
account keeps its identity; portal-created orders are outside this grant.

The application must display whether the merchant is in test or live mode and
the exact quoted fiat/credit amounts. A provider timeout leaves a recoverable
order rather than proof of failure. Revoking a credential stops later authorized
requests; rotating it within the same application preserves order recovery. An
archived account remains readable but cannot start or resume checkout.

An external application with checkout permission can present payment for your
already assigned plan (#931). Review its exact period, price and test/live
merchant mode. Payment activates that plan's entitlement; it does not top up
your wallet or repay an older bill. An incomplete payment should be resumed as
the same order. A cancelled plan or a renewal before its boundary refuses a new
payment, while a legitimate older payment remains recoverable.

External applications may be granted evidence publication separately from evidence
reading (#934). Publication captures an ended period or an issued statement with
its original proof scope and excludes supplier costs. An interrupted export can
be resumed without producing a new history. Exports exceeding application limits
or already containing private costs require operator assistance. The receiving
application must verify the unchanged file with its retained trust inputs.
An application with your business's explicit checkout permission can also present
payment for one named outstanding statement (#935). Check the statement and
amount before paying: this clears existing debt and does not renew a plan or
fund your wallet. A partial payment leaves the remainder payable. If checkout
times out, recover the same order instead of starting another. A refund or lost
payment restores only its original source debt, with the existing finance review
and source evidence available to authorized staff.

### Statements in an embedded application

An operator may grant an application billingRead to display its customers' retained
statements and source-linked payment/reversal lines. The application can discover
the bill itself; a customer does not need to find or type its UUID. Archived accounts
retain authorized history. Statement reads do not renew a plan or initiate payment;
those actions require a separate checkout grant and review of the current quote.
A document over the export bounds returns an explicit recovery error rather than
showing a partial statement as complete.
## Verify your first successful call

On **Get started**, the request and bill steps become ready when a successful
call in your current customer scope has consistent receipt evidence. Free and
fully entitled calls count. Later failed requests do not hide an earlier success.
Choose **Verify this receipt** to open its proof and content hash, then run the
verification. The page explains that this freshly loaded hash comes from the
same server; retain evidence and trusted heads for later independent comparison.

No closed-day report or share link is needed for this step. Period reports are
available separately after their UTC period ends. If evidence is incomplete or a
read fails, the checklist shows a failed check and never a completed step;
refresh to retry. Members see only their permitted history.

## Check your model and sample funding

On **Get started**, use your personal customer session. The page selects a live
key you may see, a permitted chat model and that model's current customer sale
terms. The sample is one message, **Hello**, requesting up to 32 output tokens.
It shows the selected key ID and the conservative reservation split between
recorded quota and wallet funding. A zero wallet balance is fine for a free or
fully entitled sample. The key secret is never displayed by this check.

Read the reason before adding funds: a budget limit, exhausted no-overflow plan,
manual restriction, missing sale terms or pending period transition needs its
own action. Contact the business operator for model/pricing configuration and
your account administrator for member/team limits. A new account without an
initialized ledger is marked unconfirmed; this read does not create its ledger.
Normal first use initializes it. Refresh after resolving a problem.

The search is bounded to eight keys and sixteen chat models per key; a limit
notice means other choices may exist. The snapshot does not send a call or
certify provider, policy or rate-limit availability. Send a deliberate actual
call through Chat or your SDK, then open its immediate receipt. Business setup
remains in the separate Business console.
### Configure an application's signed notifications

Select an application in Business → Application access. In Notifications, enter
its receiving HTTPS URL, select lifecycle events, and record the reason. Only
approve internal IPs for an exact destination you operate. Save the signing
secret from its first response; it cannot be read again. After a lost creation
response, the same operation recovers the endpoint metadata; configure a
replacement if you did not receive the secret.

Each target lists delivery states and sanitized failures. Enter a reason before
retrying a failed delivery or disabling the target. Confirm the exact destination
before disabling it. An unresolved retry keeps its original operation; use the
explicit new-action control to start another intent after checking current state.
Retry keeps the original
delivery ID and body. Disabling stops new and unclaimed sends, although an already
claimed attempt can finish. Rotate by creating a replacement and disabling the
old target. Applications need the corresponding current-read grants to process
signals; notification status access alone does not disclose bills or keys.

### Try an application integration locally

The [external application example](../examples/embedded-application/README.md)
walks through local signup, an owner-granted limited customer key, payment,
a small AI turn, receipt/statement exports and closing the application account.
Configure the application and grants once in the operator portal; its runtime
uses no owner cookie. Keep the private store for exact retries. The local payment
page never treats a browser return as proof of payment. Verification requires
independently retained customer/origin/key inputs and runs offline against the
original export. The automated walkthrough uses local stubs and does not certify
an actual model provider or payment service.

## Manual receipt refund boundary

A manually recorded payment never passed through a payment provider. Its order
therefore offers no automated provider refund: contact the business to reconcile
an out-of-band return. New requests and approvals without complete pinned
provider coordinates return `409` before reserving a refund or issuing a note.
Retained manual requests remain readable and may be declined while requested;
old pending records stay unresolved for financial review, without provider retry
calls. Their approval/note state does not confirm that cash was returned.

## Approve monthly wallet credit

In **Business → Customers**, open a customer and locate **Monthly credit terms** (#997). This card is separate from the plan credit settings. Reports readers can inspect the approved total and the portion already drawn or reserved. Owners and finance staff can enter an exact credit amount (up to six decimal places), payment terms in days, optional monthly dunning grace and a reason. Review the customer, amount and terms in the confirmation dialog before approving.

A blank grace value disables automatic monthly dunning; zero gives no additional grace. Both day counts accept 0–3650. Payment terms apply when future monthly statements are finalized. Issued documents, sold-plan terms and manual/archive restrictions retain their own history. Increasing credit does not receive a payment or immediately remove suspension.

A total below already drawn or reserved credit is refused and leaves every term unchanged; close the error dialog to correct the input. If the connection fails or the outcome is uncertain, the form retains the original request and offers an exact retry, even after closing and reopening its dialog. Complete that retry before making another decision. A historical retry returns its original receipt and cannot overwrite a later approval. Archived customers can be inspected but cannot receive new credit decisions. Monthly statement issue/payment management remains separate from this approval card.


## Inspect monthly wallet-credit statements

Open **Business → Customers**, select a customer, and find **Monthly wallet-credit
statements** (#1007). This list is separate from the customer's sold-plan
statements. Owner, admin, finance and viewer roles with reports access can read
it; support and ordinary customer sessions cannot use this staff surface.

Open a month to see usage charges, the portion funded by credit, recorded
repayment and outstanding credit debt. The detail includes original payment
terms, UTC due/issue dates and itemized model/channel charges. **Recorded proof
window** exposes the stored anchor and hashes; displaying those fields does not
independently verify the bill. Drafts have not been issued and may lack proof
identifiers. A paid or suspended standing is preserved; a pending unpaid issued
bill displays overdue once its due date has passed.

Use **Next page**, **First page** and **Refresh** to inspect a larger history.
The list is ordered by immutable statement ID. Money reflects recorded payment
bookkeeping; this read does not run reconciliation. Oversized detail refuses
instead of silently omitting lines. Owner/finance actions for issuance and
verified repayment are described below.


The Plans page shows quota reserved or consumed in the displayed period. A running request reserves quota; unused quota returns when it settles. Paid-plan quota is separate from wallet spending, and a new period starts its own counter.


In Business → Disputes, choose Resolve and select the actual provider outcome. Evidence due keeps the case open; won and closed do not record cash loss; lost records the confirmed loss against original funding and any resulting debt or review amount. Enter the supporting reason. A failed request keeps the dialog and exact decision for retry; this action does not initiate a provider refund.


### Generate and issue a monthly statement

In Business → Customers, open the customer and find **Monthly statements**.
An owner or finance staff member can select a fully ended UTC month, enter a
reason and confirm **Generate or refresh draft**. An empty month creates no bill.
Open the resulting draft, review the usage total, credit draw, itemized models
and payment-term days, enter an issuance reason, then confirm **Review and issue**.
The due date starts from the current UTC issuance date. Existing repayments
continue to cover the oldest credit draws first.

If usage or payment terms changed after review, no issuance is committed. Generate
or refresh the draft again and review the current detail. Once issued, original
lines, terms and proof identifiers remain unchanged. If the response is uncertain,
keep that form open and **Retry the same request**; the form retains the exact
intent and reason while its outcome is unknown. Do not submit a second issuance
with a new identity to guess whether the first worked. Archived customers reject
new actions; current authority is still required to recover an earlier result.


Monthly action feedback follows the selected language. Refreshing a draft after
changed source figures clears the old review warning; successful issuance keeps
its confirmation visible beside the issued document.


## Retry a recorded monthly payment

After a deployment administrator records a verified monthly payment, retain the
selected statement, exact amount and idempotency key. If the response is lost,
repeat those same values, including when the statement now shows paid. A new
payment cannot exceed remaining credit debt through the selected month; earlier
unbilled debt is repaid first. The operation never turns excess into wallet funds.
A source-audit conflict requires reconciliation of the original payment; do not
invent a new key to work around it.





### Record verified monthly repayment

In **Business → Customers**, select the customer and open an issued monthly
statement. Owner or finance can use **Record a monthly repayment**. Enter the
verified amount in credits (at most six decimal places), original bank/payment
reference and reason, then review the customer, month, amount and reference
before confirming. This records money already received; it does not initiate a
payment. Earlier credit debt is repaid first, even if that earlier month was not
billed. The server refuses an amount exceeding unpaid debt through this month.

Keep the original entry identifier and hash shown after success. If the response
is interrupted, leave the form open and use **Retry the same request**. The
amount, reference and reason remain locked until its outcome is recovered; do
not invent a different reference to bypass a duplicate-reference error. The same
reference cannot also be recorded as a manual top-up. Customer statements show
the updated repayment and outstanding amount. Archived customers accept exact
recovery of past successful requests but no new payment.
