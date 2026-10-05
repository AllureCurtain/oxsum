# Product specification

Status: finalized (2026-10-01), audited against the code (2026-10-03, issue #61). The four open questions are all decided; the conclusions are folded into the body and the former "open questions" section is gone. Any behavior change starts with this document, then the code.

This document records product behavior: who can do what, and how every situation is billed. Technical implementation lives in architecture.md, technology rationale in decisions.md, progress in TODO.md.

Every page and flow below carries a status: **shipped** means this repository does it today, **planned** means the text describes the vision and names the issue that will build it. A planned page or flow is not a smaller feature that happens to be missing; it is documented behavior that no code implements. The shipped dashboard pages are `crates/web/src/app.rs` (shell, login, logout, overview, API keys, members, transaction log, bills, requests, verification), `crates/web/src/chat.rs` (chat) and `crates/web/src/admin.rs` (the platform-admin pages, #57); the REST and gateway behavior they call is in `crates/server`. Issue #61 is the audit that added these statuses; the resolutions are recorded in docs/decisions.md.

## One-liner

An OpenAI-compatible AI gateway. Users point base_url at oxsum and keep using any OpenAI client; every request freezes credit first, then settles against real usage. Every bill can be verified in the browser.

## Demo path

The piece must let a first-time visitor walk this path in minutes:

1. `docker compose up`, create the platform admin from the command line.
2. The admin configures one upstream channel (say DeepSeek) on the channels and prices page (`/admin/channels`, or `/api/v1/admin` with the operator token) and tops up an organization through its own credential (`POST /api/v1/topups`, the chat page's top-up form).
3. Create an API key, change base_url in any OpenAI SDK or ChatBox, and start chatting.
4. The dashboard shows each in-flight freeze with its streaming progress live, and the newest settled entries below it.
5. Open `/verify` — linked from every settled chat turn with both fields prefilled, and from every row of the bills page (`/dashboard/bills`), which lists each transaction with its content hash and exports the list as CSV or JSON — and browser-local verification passes; change one number in the bundle and it fails.

## Roles

| Role | Who | Can do | Status |
| --- | --- | --- | --- |
| Platform admin | Whoever deploys oxsum | Manage channels and prices, top up or adjust any organization, view all anomalous requests, view the charged-versus-upstream margin, run monthly closings | Channels and prices are shipped — the `/api/v1/admin` endpoints and the `/admin/channels` page, under the operator token; so are adjusting any organization's balance and the signup bonus |
| Organization owner | Organization creator | Everything inside the organization, including transferring ownership | Shipped: keys, balance, the ledger, and membership management with the ownership transfer |
| Organization admin | Appointed by the owner | Invite and remove members, manage all keys, view the organization's full bill history | Shipped: managing all keys, inviting by link or by adding an existing account, removing and re-roling members (but not the owner's own membership); the bill history ships as the bills page — the organization's transactions, exportable as CSV or JSON — which owners and admins read unfiltered |
| Organization member | People an owner or admin added | Create and revoke their own keys, view organization balance and their own request records | Their own keys, the balance and their own request records are shipped (the requests page, #55, shows the turns their own keys paid for); the bills page shows a member the transactions their own keys paid plus the organization's shared history — top-ups and adjustments carry no key, so every member sees them |
| API caller | Programs holding a key | Call the gateway, spending the key's organization balance | Shipped |

The platform admin is not an organization role but a deployment-level identity; users cannot apply for it.

Two shipped pages are readable more widely than the "Who" column suggests. The transaction log page and the members page take any live session (`get_log` and `get_members` in `crates/web/src/api.rs`, neither with a role check), so every member reads the organization's raw entries and the membership list. Reading who is in the organization is not a management action; the page's management actions — add a member, remove one, change a role, transfer ownership — are restricted to owners and admins. The requests and bills pages are scoped: a member reads the transactions their own keys paid for plus the key-less organization history — top-ups and adjustments — and nobody else's spend (`get_requests` and `get_bills` filter the organization's ledger rows through the keys the session may see, and the exports and the proof-bundle read apply the same scope).

## Accounts and organizations

### Registration and login (shipped)

- Login is email plus password; passwords are stored with argon2. v1 sends no email, so there is no email verification and no password recovery. The platform admin resets a forgotten password through `POST /api/v1/admin/users/{userId}/password-reset` (issue #91, shipped): the reset revokes every session the user holds, so the old credential dies with it, and answers how many sessions it revoked.
- A login mints a session: the response names the user, the organization and the role, and a cookie (`oxsum_session`, `HttpOnly`, `SameSite=Lax`, `Path=/`, plus `Secure` when the deployment is behind TLS) carries it from then on. The session acts as the user's organization in their role, expires thirty days after login, and stops authenticating the moment the user's membership in that organization is gone. Logging out revokes it. An unknown email and a wrong password answer the same 401 with the same message, so neither reveals whether an account exists.
- Sessions and API keys are two credentials for the same organization: every `/api/v1` endpoint takes either (an explicit bearer token wins over the cookie), while the gateway (`/v1`) takes an API key only. Role rules apply to sessions; a key keeps acting as the whole organization.
- The platform admin is whoever deploys oxsum: the operator token `OXSUM_ADMIN_TOKEN` opens `/api/v1/admin`, and there is no platform-admin user — see docs/decisions.md ("an operator token rather than a session"). The `/admin` pages open with the same token: channels and prices, organizations, in-flight requests, anomalies and closing are all shipped. Not "the first registrant becomes admin" — on a public instance, whoever registers first would own it.
- Registration mode is deployment-configured (`OXSUM_SIGNUP`):
  - `invite` (default): registration only through invitation links — `POST /api/v1/auth/register` is refused, and an owner or admin's link is the way in
  - `open`: anyone can register, and invitation links work too
  - Both modes ship, and the links ship with them.
- Registration takes a password of at least 12 characters. Length is the only rule: with no email recovery, composition classes and expiry would cost users more than they buy.
- Passwords are hashed before the database transaction opens, so a slow argon2 hash never holds a connection.
- GitHub login is post-v1. It suits a GitHub-hosted piece, but deployers would have to configure an OAuth app; v1 gets email login solid first.

### Organizations

- Shipped: signup creates the user, the personal organization, the owner membership and the first API key in one transaction, and returns that key's secret once.
- Shipped: the organization's ledger is not created at signup — it is created on first use, so an account that never spends costs nothing and a failed ledger migration cannot leave a half-registered user (docs/decisions.md).
- Shipped: the tenant id of an organization is its own UUID without dashes, so a ledger schema is `ledger_<32 hex characters>` and nothing has to be chosen, probed or made unique.
- Shipped: a second person makes the organization a team. When the first membership other than the creator's is added, the organization flips from `personal` to `team`, one way; the ledger does not migrate, because it belongs to the organization, not to the person.
- Shipped: a user belongs to several organizations. `POST /api/v1/orgs` creates a `team` organization — its own ledger, its own keys, its own balance — with the user as its owner, and `GET /api/v1/orgs` lists every organization they are a member of, oldest membership first, with the role they hold.
- Shipped: the acting organization is the session's choice, switched from the dashboard's top-right corner or `POST /api/v1/session/organization`. The choice lives on the session row, so it survives reloads and carries every request the cookie makes: the overview, keys, members, bills, requests and the transaction log all scope to it. Switching names a membership, nothing else — an organization the user is not a member of answers `NOT_FOUND`, so the endpoint does not say whether it exists. An API key cannot list, create or switch: a key belongs to exactly one organization.
- Shipped: invitation links (issue #59). An owner or admin mints one on the members page or through `POST /api/v1/org/invitations`; the token (`oxi-` plus 32 random bytes, stored only as a hash) is answered once and the link lives seven days. The invitee opens `/register?invite=<token>` — or calls `POST /api/v1/invitations/redeem` — picks an email and password, and lands in the inviting organization as a member with a first API key and no personal organization. Spent, expired and unknown tokens are all the same `NOT_FOUND`, and redemption takes the row's lock so two people holding one link cannot both win. v1 sends no email; the inviter passes the link along themselves. **Membership management is not invitations**: adding a member still means adding an account that already exists.
- Shipped: an owner cannot leave directly; ownership must transfer first. There is no leave action at all — removing yourself is the owner removing a member, and for the organization's last owner that is refused. See "Membership management" below.
- v1 does not support deleting organizations. The ledger is append-only; deleting an organization would orphan its billing history.

### Membership management (shipped)

An organization is a set of memberships, each one an account plus a role. Four actions change that set, from the members page or from `/api/v1/org/members` and `/api/v1/org/ownership`:

- **Add a member**: by email, and only for an account that already exists — the person registers first, then an owner or admin adds them. A newly added member is always a `member`; the response is the membership, so the page can show who was added.
- **Remove a member**: the membership goes, the account and its own personal organization stay. Naming someone who is not a member is a 404, so removing twice cannot look like it worked.
- **Change a role**: between `admin` and `member`. Promoting to `owner` is not a request the API can express — ownership is transferred, never assigned, so an organization always has exactly one owner.
- **Transfer ownership**: the target becomes the owner and the acting owner becomes an admin, in one transaction. The owner seat is never empty and never doubled.

Who may do what, and the gaps that are closed on purpose:

| Actor | Add, remove, change a role | Transfer ownership |
| --- | --- | --- |
| Owner | Yes, including on other admins | Yes — to any other member; to themselves is a conflict |
| Admin | Yes, except on the owner's own membership (403) | No (403) |
| Member | No (403): a member may read the list, not change it | No (403) |
| API key | No (403), even the owner's own key: a key is not a person and names no role | No (403) |

- The last owner cannot be removed or demoted: that would leave an organization nobody owns. The answer is a 409 saying ownership has to be transferred first. In practice the only reachable case is the sole owner acting on themselves, since admins may not touch an owner at all.
- Every new endpoint checks the acting role in the server, and so does the page: no control is rendered that the server would refuse, and a refusal is shown in the server's own words rather than silently swallowed. (docs/decisions.md, "membership management is a person's action".)


## Where credit comes from

v1 has no payment integration. Credit has three designed sources; the table says which of them the code implements today, and what the ledger actually records.

| Source | Who | Notes |
| --- | --- | --- |
| Top-up | Any credential of the organization — session or API key | **Shipped.** `POST /api/v1/topups` sits behind the same credential check as every other organization endpoint (`crates/server/src/routes.rs`, the `authenticated` router), so a logged-in user self-tops-up; the chat page's top-up button is exactly this call (`crates/web/src/chat.rs`). The request carries an amount and an idempotency key and **no reason**: the endpoint takes no description. Admin-gated top-ups with a required reason are the rejected alternative in docs/decisions.md. |
| Signup bonus | Automatic | **Shipped** (issue #60). `OXSUM_SIGNUP_BONUS_MINOR`, default 0: self-registration credits the new organization's bonus pool with the amount, booked as an adjustment carrying "signup bonus" as its reason — an invitation's redeem joins an existing organization and grants nothing. At 0, registration still writes no ledger entry. |
| Adjustment | Platform admin | **Shipped** (issue #60). `POST /api/v1/admin/organizations/{organizationId}/adjustments`, and an "Adjust" form on each row of `/admin/organizations`. Positive grants, negative deducts; the reason is required and becomes the entry's description, covered by its proof. A grant lands in the bonus pool; a deduction draws the bonus pool first and only spills into purchased credit once granted money is gone — the platform takes back what it gave before touching what the user paid for. A deduction deeper than the combined balance is the ledger's own no-overdraft refusal — `402 INSUFFICIENT_FUNDS`. |

- The unit is credit, 1 credit = 1_000_000 minor. What a credit maps to in real money is the deployer's choice; oxsum does not care. (Shipped.)
- The balance is one number to the organization but two pools in the ledger (issue #116, decision D1): granted credit — the signup bonus and admin grants — lives in `Equity:Bonus`, purchased credit in `Liabilities:Wallet`. Holds reserve bonus-first, settlements charge bonus-first, and every balance read sums the two; the pool split is how the ledger keeps granted and paid-for credit apart, never something a caller manages.
- Adjustments never modify history; they book a new entry. The original entry and its proof stay valid.

## API keys (shipped)

- Keys belong to organizations and hold no money themselves. The creator is recorded for permissions and audit; a key minted through a session records who minted it, a key minted through the API records no creator, because no person is acting there. Members see and revoke only the keys they created; owners and admins see and revoke all of them.
- The plaintext shows once at creation. The database stores only the SHA-256 hash and a prefix; the dashboard uses the prefix to help users recognize keys.
- Format: `oxs-` followed by 32 random bytes, so secret-scanning tools can recognize them.
- Optional name and expiry. Revocation is permanent.
- Per-key spend limits are implemented (backend, TODO B-8): a key's `spendLimitMinor` caps its committed spend, enforced atomically with the hold append. Model whitelists are still post-v1.

## Gateway (shipped)

### Supported endpoints (shipped)

- `POST /v1/chat/completions`, streaming and non-streaming
- `GET /v1/models`: lists only models with configured prices

v1 has no embeddings, images, audio, Responses API or Anthropic Messages format. Request content is text-only; messages containing images get a 400. This is decided: the input token upper bound must be computable for the freeze promise to hold (see "How much to freeze" below). Image support is re-evaluated post-v1.

### Channels and prices (shipped)

- Channels are configured by the platform admin: name, upstream base_url, upstream API key, served models, and the upstream protocol the channel speaks — `openai` today; the protocol selects the adapter that normalizes its usage reports, and a name with no adapter is refused at write time.
- In v1 one model maps to exactly one channel; no load balancing, no failover. The gateway never retries upstream automatically, because a retry might charge upstream twice.
- Upstream API keys are stored encrypted; the admin API and the channels page show only the last 4 characters.
- Each model's price is a full price set, not just two rates: the input and output prices (minor units per million tokens) and the model's max output, plus any of the priced dimensions — a cached-input rate, cache-write rates for the 5-minute and 1-hour provider tiers, a reasoning-token rate, and a flat per-request fee. A dimension with no rate folds into its side's base line: cached input bills at the input price, reasoning at the output price. The set may also carry the upstream's own prices for the same usage — the data the margin view reads; an organization's bill never shows it. Every price declares a billing `mode` (`chat` today; the field reserves embeddings, rerank and the rest).
- A price can also carry conditional rules: a `match` on the request's `serviceTier` or an input-token window picks a different whole set — a match swaps the set, never a single field. When several rules could apply the most specific one wins (the count of present conditions), and two rules that could match the same request at the same specificity are refused when the price is written — an ambiguous price book never reaches a bill.
- Changing a price never overwrites the old one; it creates a new version. The version in force when a request starts is the version used to settle it. So a price change only affects later requests; in-flight requests and old bills are untouched.

### How much to freeze (shipped)

When a request arrives, compute the most it could possibly cost, freeze that much, then contact upstream.

- Input upper bound = UTF-8 byte count of all text content in the request, plus a fixed per-message format overhead. Mainstream tokenizers are byte-level BPE, so one token spans at least one byte and the byte count never undercounts tokens. The cost is overshooting — roughly 4x for English, 2x for Chinese — and the excess is refunded in full at settlement.
- Output upper bound = the request's `max_tokens` or `max_completion_tokens`. If absent, the model's configured maximum output; if larger than that maximum, the maximum applies.
- Freeze = the most the turn could possibly cost under every set that could match it: input bound at the dearest applicable input rate (a cache-write tier prices above fresh input, so it prices the bound), output bound at the dearest output rate, plus the flat request fee — rounded up once. The request's own `serviceTier` narrows the lane: a rule that needs a tier the caller did not ask for cannot hit, and does not inflate the freeze.
- When relaying, the gateway always writes the output upper bound into the request. Upstream output can then never exceed the estimate the freeze was based on.
- If available balance cannot cover the freeze, return 402 without contacting upstream. The error states how much the freeze needs, the current available balance, and suggests lowering `max_tokens`.

No tokenizer-based freeze estimation: tokenizers differ per model, and an underestimate means actual spend can exceed the freeze. new-api's #7554 is the reverse failure — token estimation inflated tens of times, pre-holding 4M at once. Byte estimation overshoots, but the bound is guaranteed and the excess is always refunded.

### How settlement works (shipped)

When relaying a streaming request, the gateway always writes `stream_options.include_usage = true` so upstream returns usage in the final chunk.

| Situation | Charge | Settlement type |
| --- | --- | --- |
| Completed normally, usage received | usage × the price version from request start | `usage` |
| Upstream returned an error before emitting anything | 0, full release of the hold | `upstream_error` |
| Could not connect or timed out, no response received | 0, full release | `upstream_unreachable` |
| Partial output received, upstream dropped mid-stream, no usage | Local estimation, see below | `estimated` |
| Client disconnected mid-stream | Cancel the upstream call immediately; estimate locally from what was forwarded | `client_cancelled` |
| Usage-priced cost exceeds the freeze | Charge the freeze only; record the excess as an anomaly for admin review | `capped` |
| Hold timed out with no settlement (e.g. the gateway crashed) | 0, full release, recorded as an anomaly | `swept` |
| Usage names a dimension the price book cannot bill (a tool call, a media token, a foreign event kind) | Charge the part the book covers, capped by the freeze; the unpriced rest is the platform's flagged loss — never a silent zero, never folded into a rate it does not belong to | `unpriced` |

(The settlement kinds and the anomalies page that reviews them — `capped`, `estimated`, `client_cancelled`, `swept` and `unpriced`, read out of the ledgers — both ship.)

- Local estimation: count input and forwarded output with tiktoken's `o200k_base`; the result never exceeds the freeze. The bill is explicitly marked "estimated".
- The user never pays more than the freeze — that is the gateway's promise. Underestimates, missing usage and upstream overcharges are the platform's cost.
- The hold timeout defaults to 30 minutes and must exceed the longest possible single request. A background job sweeps timed-out holds. The sweeper and normal settlement share the same idempotency key, so both cannot succeed; whichever lands first wins.
- Every response carries an `x-oxsum-request-id` header; the bills page's settlement rows lead with that same request id, so a row joins the header a client logged to the ledger entry that billed it.

Two settled trade-offs:

- On client disconnect, cancel the upstream call immediately. Rationale: users must not pay for content they never received; letting upstream keep running means the platform pays upstream for nothing. The cost is settling on local estimation, marked on the bill.
- On mid-stream interruption with no usage, settle on local estimation — do not copy sandbase's free-transaction approach. Free is friendliest to users but rewards deliberately cutting the connection; the platform's cost becomes an exploitable loophole. An explicitly-marked "estimated" bill is the honest middle ground. The never-pay-more-than-the-freeze promise still holds.

### How one request is booked (shipped)

- One request maps to two entries: the freeze and the settlement. Idempotency keys: `req-<id>:hold`, and the settlement's key derived from it (`oxsum_core::settlement_key_for`).
- The settlement entry's description carries a compact versioned JSON (`"v":3`): the request id, model, price version and settlement kind, the metered usage dimensions, the itemized lines the charge sums from — each `[item, units, pricePerMillion]` — and the rule that priced the turn, when one did. The description is hashed into the entry, so what the user verifies is not just "how much was charged" but "by how many of what, at what rates, under which rule". The description caps at 512 characters, which is why a line is a tuple, not an object — see docs/decisions.md.
- Beside every settlement that lands, the same path writes the turn's normalized usage record (`oxsum.usage_records`, issue #102): the full token dimensions — cached and reasoning tokens as subsets of the totals they belong to — the caller's `user`/`metadata`/`service_tier` attribution, the settlement kind, and `usage_details.provider_raw`, upstream's usage object verbatim for the reconciliation window. The row is mutable-store data by design: attribution is pseudonymized when an organization is deleted and `provider_raw` is nulled after the retention window, so none of it can live inside a hashed ledger entry. The entry stays the source of truth for the charge; a swept turn leaves the same row at zero, still naming whose turn it was.
- The row also carries `upstream_cost_minor` (issue #112): what the price's `upstream` block made of the same usage — the platform's own cost for the turn, computed under the same set the customer priced with. `NULL` means untracked, not free: a price with no `upstream` block, a swept turn, or history from before the column. `GET /api/v1/admin/margin` sums charged against upstream cost per channel and model and counts the untracked rows; it is the deployer's view for spotting margin erosion and upstream misbilling, and no organization-facing endpoint ever shows it.
- The full request state (in flight, settled, anomalous) was going to live in oxsum's own `requests` table, which was never built. The requests page (#55) reads what already exists instead: each settled gateway request is a settlement entry whose description carries the model, the token counts and the prices, read back as `SettlementRecord` (`crates/core/src/billing.rs`) and listed by `Wallet::recent_requests`. In-flight gateway holds are the sweeper's `oxsum.open_holds` watch table, shown live on the overview, and the ledger stays the source of truth. Only what needs proving goes into the ledger. See docs/decisions.md, "the requests page reads the ledger".

## Bills and verification

### Bills page (shipped)

`/dashboard/bills` (`crates/web/src/app.rs`) lists the organization's transactions, newest first: top-ups, adjustments — the signup bonus among them — and settled requests. Each row carries the date it was booked, its kind, the detail (a settled gateway turn shows its request id, model, token counts, freeze and settlement kind — the fields the entry's own record holds; a top-up or an adjustment shows the reason it was written), what it moved in credits signed — money in reads positive, a charge negative — and the content hash its proof verifies against. A hold bills nothing yet and is not listed; the overview's in-flight list is where it shows. (Shipped.)

- Member scope is the requests page's rule: a member reads the transactions their own keys paid plus the key-less organization history — top-ups and adjustments — while owners and admins read everything. The exports and the proof-bundle read apply the same scope. (Shipped.)
- Every row links to `/verify` with its entry named (`?entry=<id>`); the verify page fetches the proof bundle for that entry — from the reader's own organization, in the session's scope — and runs the browser-local check on load. (Shipped.)
- CSV and JSON export: `GET /dashboard/bills/export.csv` and `GET /dashboard/bills/export.json` answer the page's own list as a file, with `Content-Disposition: attachment`, so a browser saves it and a command-line client fetches the same bytes with the same session cookie. Both files carry the same fields and the same rows — `bookedOn`, `entryId`, `kind`, `amountMinor`, `description`, `contentHash`, plus the settled turn's parsed `request` in the JSON — so a record can be archived and checked against the ledger later; the amount is the ledger's signed integer in minor units in both files, where the page renders the same amount in credits, and the CSV is RFC 4180. (Shipped.)
- The page lists a hundred transactions at a time: **Older** and **Newest** links page through the history, the position traveling as the URL's own `?before=` — a ledger list's cursor is the log index below which the page reads, so a page is a link and works in a browser that never hydrates. The exports are not pages: an archive means the whole history, so they walk the same read to the log's start and carry every transaction. (Shipped, issue #93.)

The transaction log page is still what it was (`/dashboard/log`, `crates/web/src/app.rs`): the organization's ledger entries, newest first, each with its index, id, description (for a settled request, the compact versioned JSON carrying the model, the metered usage, the priced lines and the settlement kind) and content hash. It lists twenty-five entries on the overview and pages a hundred at a time on its own page, the same `?before=` walk the bills page uses, and it is readable by every member — the raw log beside the bills page's transaction view of the same ledger.

### Browser verification

- Shipped: every entry verifies locally in the browser. The verification logic is the WASM build of `verify_bundle` — the same code the server runs. The public `/verify` page (`crates/web/src/app.rs`) takes a proof bundle and a content hash and runs the check in the browser; the chat page links to it with both fields prefilled after every settled turn. For a usage settlement the page goes further (#106): the description is a versioned billing credential, and `verify_charge` recomputes the recorded charge from the usage and priced lines inside the entry — a record from a schema the verifier does not know reports inclusion only, with the reason named.
- Shipped (#92): each visit to the bills page archives the organization's signed tree head in the browser's `localStorage`, keyed by the log's origin, after verifying the operator's signature on it. On a later visit, when the log has grown, the page fetches the consistency proof (`GET /api/v1/log/consistency?from=` through the session's server function) and verifies it plus the new head's signature in the browser — a one-line status reports the ledger append-only, a shrunk or rewritten log reports the failure in words and the archive is never overwritten on a failed check. The user does nothing. The public endpoint it mirrors is shipped (`GET /api/v1/log/consistency?from=`, B-7); the check also covers what #94 had left open: the operator's signature on the head is verified in the browser on every visit.
- Third-party witness cosignatures — witnesses beyond the operator attesting the same head — are not built; the signed-note wire format already admits them.

Verification proves: this record was not altered after being written, history was not rewritten, and — for a versioned settlement — the recorded charge agrees with the usage and rates inside the entry. It does not prove: upstream really returned that many tokens. The page states this plainly, without inflating it. (Shipped.)

## Pages

### Organization view (logged-in users)

| Page | Content | Who sees it | Status |
| --- | --- | --- | --- |
| Overview | Available balance, the frozen total and this month's spend; in-flight requests with their frozen upper bound and live streaming progress | All members | Shipped. The three figures are the overview's own integers (`DashboardData`, `crates/web/src/api.rs`), read from the organization's ledger: the frozen total is the wallet's reserved balance — the sum of the outstanding holds — and this month's spend is what settlements charged on or after the first of the current UTC month, so a top-up is not spend and an outstanding hold is not either. Progress is forwarded characters, not tokens (docs/decisions.md). |
| Chat | Top up, pick a model and chat; each turn's billing live, with a verify-this-bill link | All members | Shipped (`crates/web/src/chat.rs`) |
| Requests | The organization's settled gateway requests, newest first: booking date, request id, model, key, status (the settlement kind the bill records), input and output tokens, and the charge in credits. Filterable by key and by model, with the filters in the page's URL | All members; a member sees only the turns their own keys paid for | Shipped (`crates/web/src/app.rs`, `get_requests` in `crates/web/src/api.rs`, #55). Every row is read from the settlement entry the gateway wrote, so the numbers on it are the ones the entry's content hash covers; a turn still in flight has no settlement yet — the overview lists those live |
| Bills | The organization's transactions — top-ups, adjustments, settled requests — with the date booked, the signed amount in credits, the detail and the content hash, newest first; a per-row verify link; CSV and JSON export of the same list, carrying the amount as the ledger's signed integer | Every member, scoped to what their own keys paid plus the shared organization history | Shipped (`crates/web/src/app.rs`, `get_bills`/`get_entry_bundle` in `crates/web/src/api.rs`, `/dashboard/bills`) |
| API keys | Create, revoke | Members manage their own; admins manage all | Shipped |
| Members | Everyone in the organization, with their role and join date; add a member by email, remove one, change a role, transfer ownership | The table: every member. The four actions: owners and admins, with the controls rendered only for them | Shipped (`crates/web/src/app.rs`, `POST /api/v1/org/members`, `PATCH`/`DELETE /api/v1/org/members/{userId}`, `POST /api/v1/org/ownership`). An add names an existing account; someone without one comes in through an invitation link (`POST /api/v1/org/invitations`), minted from the same controls |
| Transaction log | The organization's newest ledger entries with their content hashes | All members | Shipped, but not part of the original spec; the bills page is now the settled, exportable view of the same ledger |

### Platform admin

The platform admin surface is the `/api/v1/admin` REST API under the operator token (`OXSUM_ADMIN_TOKEN`), and the `/admin` pages drive it directly from the browser — the token is typed in once, kept in `localStorage` like the chat page's key and sent as `Authorization: Bearer` (docs/decisions.md). All five pages are shipped, and so are the adjustments and signup bonus the first row needs.

| Page | Content | Status |
| --- | --- | --- |
| Organizations | Every organization's balance; top up, adjust | Shipped: the list paginates a hundred at a time (`?limit=`/`?cursor=` on `GET /api/v1/admin/organizations`, a Load more on the page), plus a per-row adjust form — `POST …/{organizationId}/adjustments` for the write |
| Channels & prices | Configure upstreams and model prices; view price version history | Shipped: `/admin/channels` on the `/api/v1/admin` endpoints — list, create or repoint a channel, append a price version, read a channel's whole history |
| In-flight requests | Every unsettled hold, globally | Shipped: `/admin/in-flight` on `GET /api/v1/admin/holds` — the sweeper's watch table joined to the organizations |
| Anomalies | The settled turns that did not price cleanly, summarized per channel | Shipped: `/admin/anomalies` on `GET /api/v1/admin/anomalies` — the `capped`, `estimated`, `client_cancelled`, `swept` and `unpriced` settlement records, read back from the ledgers themselves, with a per-channel count and charged total |
| Closing | Monthly closing; after closing, that month accepts no new entries and a closing record is produced | Shipped: `/admin/closing` on `GET`/`POST /api/v1/admin/closings` — closing seals the `YYYY-MM` period in every organization's ledger (doubleentry's period seals: the log's tree head, the closing trial balance, chained onto the seal before it), and the sealed watermark then refuses any entry dated into the month |

## Phase C: chat page (shipped)

- The chat page lives in the dashboard (`/dashboard/chat`), behind the login session. Top-up runs on the session; the chat itself goes through the gateway, which takes an API key only (#17) — so the page mints a key named `chat` and keeps it in the browser's local storage. The user never handles it by hand, but there *is* a key on the wire: product.md's earlier "no API key needed" is revised to match.
- Conversations live only in browser-local storage; the server keeps nothing. Simple to build, and no duty to store users' conversations.
- Each AI reply shows that turn's billing underneath: the frozen upper bound and streaming progress live, then the settled charge — amount, settlement kind, token counts, price version. "Verify this bill" opens `/verify` with the bundle and the content hash prefilled.

## Not in v1

- Payments: a payment gateway, and any top-up path other than the organization's own credential. Self-service top-up itself is shipped and is *not* on this list (see "Where credit comes from").
- Email: verification, password recovery, invitation emails
- Multi-currency
- Multi-channel load balancing and failover
- Anything beyond text chat
- Request-level deduplication: retrying with the same Idempotency-Key charges once. Streaming responses cannot be replayed verbatim, so this waits; instead, every request is individually traceable on the bill.
- Rate limiting: v1's only gate is balance. The sum of concurrent freezes cannot exceed the balance, which itself bounds concurrency.

## Planned, in one place

Everything this document describes but the code does not do yet, with the issue that tracks it:

- Pagination for the lists that hard-truncate today: bills and both exports at 100 rows, the transaction log at 100 (25 on the overview), requests and the admin lists at their caps — issue #93
