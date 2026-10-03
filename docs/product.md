# Product specification

Status: finalized (2026-10-01), audited against the code (2026-10-03, issue #61). The four open questions are all decided; the conclusions are folded into the body and the former "open questions" section is gone. Any behavior change starts with this document, then the code.

This document records product behavior: who can do what, and how every situation is billed. Technical implementation lives in architecture.md, technology rationale in decisions.md, progress in TODO.md.

Every page and flow below carries a status: **shipped** means this repository does it today, **planned** means the text describes the vision and names the issue that will build it. A planned page or flow is not a smaller feature that happens to be missing; it is documented behavior that no code implements. The shipped dashboard pages are `crates/web/src/app.rs` (shell, login, logout, overview, API keys, members, transaction log, bills, verification) and `crates/web/src/chat.rs` (chat); the REST and gateway behavior they call is in `crates/server`. Issue #61 is the audit that added these statuses; the resolutions are recorded in docs/decisions.md.

## One-liner

An OpenAI-compatible AI gateway. Users point base_url at oxsum and keep using any OpenAI client; every request freezes credit first, then settles against real usage. Every bill can be verified in the browser.

## Demo path

The piece must let a first-time visitor walk this path in minutes:

1. `docker compose up`, create the platform admin from the command line.
2. The admin configures one upstream channel (say DeepSeek) at `/api/v1/admin` with the operator token — the admin *pages* are planned, #57 — and tops up an organization through its own credential (`POST /api/v1/topups`, the chat page's top-up form).
3. Create an API key, change base_url in any OpenAI SDK or ChatBox, and start chatting.
4. The dashboard shows each in-flight freeze with its streaming progress live, and the newest settled entries below it.
5. Open `/verify` — linked from every settled chat turn with both fields prefilled — and browser-local verification passes; change one number in the bundle and it fails. The bills page (`/dashboard/bills`) lists each settled entry with its content hash and exports the list as CSV or JSON; fetching an entry's proof bundle from that page is planned, #54.

## Roles

| Role | Who | Can do | Status |
| --- | --- | --- | --- |
| Platform admin | Whoever deploys oxsum | Manage channels and prices, top up or adjust any organization, view all anomalous requests, run monthly closings | Channels and prices are shipped (`/api/v1/admin`, operator token); topping up or adjusting another organization and the closings are planned (#57 pages, #60 adjustments and the signup bonus) |
| Organization owner | Organization creator | Everything inside the organization, including transferring ownership | Keys, balance and the ledger are shipped; transferring ownership is planned (#56) |
| Organization admin | Appointed by the owner | Invite and remove members, manage all keys, view the organization's full bill history | Managing all keys is shipped; inviting and removing is planned (#56); the bill history ships as the bills page — the organization's settled entries, exportable as CSV or JSON — which applies no member filter yet |
| Organization member | Invited people | Create and revoke their own keys, view organization balance and their own request records | Their own keys and the balance are shipped; their own request records are planned (#55 requests page), the shipped bills page lists the whole organization's settled entries with no member filter (planned, #54), and so is being invited (#59) |
| API caller | Programs holding a key | Call the gateway, spending the key's organization balance | Shipped |

The platform admin is not an organization role but a deployment-level identity; users cannot apply for it.

Two shipped pages are readable more widely than the "Who" column suggests. The transaction log page, the bills page and the members page take any live session (`get_log`, `get_bills` and `get_members` in `crates/web/src/api.rs`, none with a role check), so every member reads the organization's entries, its bills and the membership list; none of the three offers a management action. The role-scoped bill view is still planned (#54: the bills page shipped without a member filter), the requests page with #55, and membership management with #56.

## Accounts and organizations

### Registration and login (shipped)

- Login is email plus password; passwords are stored with argon2. v1 sends no email, so there is no email verification and no password recovery. The platform admin resets forgotten passwords by hand today — there is no operator endpoint for it, and none is planned in an issue yet.
- A login mints a session: the response names the user, the organization and the role, and a cookie (`oxsum_session`, `HttpOnly`, `SameSite=Lax`, `Path=/`, plus `Secure` when the deployment is behind TLS) carries it from then on. The session acts as the user's organization in their role, expires thirty days after login, and stops authenticating the moment the user's membership in that organization is gone. Logging out revokes it. An unknown email and a wrong password answer the same 401 with the same message, so neither reveals whether an account exists.
- Sessions and API keys are two credentials for the same organization: every `/api/v1` endpoint takes either (an explicit bearer token wins over the cookie), while the gateway (`/v1`) takes an API key only. Role rules apply to sessions; a key keeps acting as the whole organization.
- The platform admin is whoever deploys oxsum: the operator token `OXSUM_ADMIN_TOKEN` opens `/api/v1/admin`, and there is no platform-admin user — see docs/decisions.md ("an operator token rather than a session"). Today that surface serves channels and prices only; the rest of the platform-admin role is planned, #57. Not "the first registrant becomes admin" — on a public instance, whoever registers first would own it.
- Registration mode is deployment-configured (`OXSUM_SIGNUP`):
  - `invite` (default): registration only through invitation links
  - `open`: anyone can register
  - Both modes ship; the invitation *links* they are for do not (#59). Until they exist, `invite` refuses self-registration outright (`403 FORBIDDEN`) and the operator creates accounts; the mode is not a half-built flow, it is the switch the flow will hang off.
- Registration takes a password of at least 12 characters. Length is the only rule: with no email recovery, composition classes and expiry would cost users more than they buy.
- Passwords are hashed before the database transaction opens, so a slow argon2 hash never holds a connection.
- GitHub login is post-v1. It suits a GitHub-hosted piece, but deployers would have to configure an OAuth app; v1 gets email login solid first.

### Organizations

- Shipped: signup creates the user, the personal organization, the owner membership and the first API key in one transaction, and returns that key's secret once.
- Shipped: the organization's ledger is not created at signup — it is created on first use, so an account that never spends costs nothing and a failed ledger migration cannot leave a half-registered user (docs/decisions.md).
- Shipped: the tenant id of an organization is its own UUID without dashes, so a ledger schema is `ledger_<32 hex characters>` and nothing has to be chosen, probed or made unique.
- Planned, #59: personal organizations can invite members too. When the first person joins, the organization flips from `personal` to `team` automatically; the ledger does not migrate. Nothing flips today: signup is the only code path that inserts a membership, so every organization still has exactly one member.
- Planned, #58: users can create team organizations and join several. The current organization switches from the top-right corner. There is no way to create a second organization or to switch, and the dashboard always acts as the session's own organization.
- Planned, #59: invitations: one link, valid 7 days, usable once. v1 sends no email; the inviter passes the link along themselves. The registration mode that reads them (`OXSUM_SIGNUP=invite`) is shipped; the links are not.
- Planned, #56: an owner cannot leave directly; ownership must transfer first. Today nobody can leave an organization at all — there is no leave and no transfer action.
- v1 does not support deleting organizations. The ledger is append-only; deleting an organization would orphan its billing history.

## Where credit comes from

v1 has no payment integration. Credit has three designed sources; the table says which of them the code implements today, and what the ledger actually records.

| Source | Who | Notes |
| --- | --- | --- |
| Top-up | Any credential of the organization — session or API key | **Shipped.** `POST /api/v1/topups` sits behind the same credential check as every other organization endpoint (`crates/server/src/routes.rs`, the `authenticated` router), so a logged-in user self-tops-up; the chat page's top-up button is exactly this call (`crates/web/src/chat.rs`). The request carries an amount and an idempotency key and **no reason**: the endpoint takes no description. Admin-gated top-ups with a required reason are the rejected alternative in docs/decisions.md. |
| Signup bonus | Automatic | **Planned, #60.** Amount configured by the deployer, default 0. Nothing is credited today: registration writes no ledger entry at all. |
| Adjustment | Platform admin | **Planned, #60.** Can add or subtract; reason required. A deduction cannot push available balance negative. No admin endpoint for it exists — `/api/v1/admin` serves channels and prices only. |

- The unit is credit, 1 credit = 1_000_000 minor. What a credit maps to in real money is the deployer's choice; oxsum does not care. (Shipped.)
- When adjustments ship (#60), they never modify history; they book a new entry. The original entry and its proof stay valid.

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

### Channels and prices (shipped; the pages are planned, #57)

- Channels are configured by the platform admin: name, upstream base_url, upstream API key, served models.
- In v1 one model maps to exactly one channel; no load balancing, no failover. The gateway never retries upstream automatically, because a retry might charge upstream twice.
- Upstream API keys are stored encrypted; the admin API shows only the last 4 characters (the page that would show them is planned, #57).
- Each model's price: input price, output price (credit per million tokens), plus a max-output-token count.
- Changing a price never overwrites the old one; it creates a new version. The version in force when a request starts is the version used to settle it. So a price change only affects later requests; in-flight requests and old bills are untouched.

### How much to freeze (shipped)

When a request arrives, compute the most it could possibly cost, freeze that much, then contact upstream.

- Input upper bound = UTF-8 byte count of all text content in the request, plus a fixed per-message format overhead. Mainstream tokenizers are byte-level BPE, so one token spans at least one byte and the byte count never undercounts tokens. The cost is overshooting — roughly 4x for English, 2x for Chinese — and the excess is refunded in full at settlement.
- Output upper bound = the request's `max_tokens` or `max_completion_tokens`. If absent, the model's configured maximum output; if larger than that maximum, the maximum applies.
- Freeze = input upper bound × input price + output upper bound × output price, rounded up.
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

(The settlement kinds above all ship; the anomalies page an admin would review them on is planned, #57. The record is what exists today.)

- Local estimation: count input and forwarded output with tiktoken's `o200k_base`; the result never exceeds the freeze. The bill is explicitly marked "estimated".
- The user never pays more than the freeze — that is the gateway's promise. Underestimates, missing usage and upstream overcharges are the platform's cost.
- The hold timeout defaults to 30 minutes and must exceed the longest possible single request. A background job sweeps timed-out holds. The sweeper and normal settlement share the same idempotency key, so both cannot succeed; whichever lands first wins.
- Every response carries an `x-oxsum-request-id` header; users use it to find the matching entry in the transaction log (whose descriptions carry it). The bills page lists settled entries by date, charge and content hash, not by request id, so a row that links the two is planned, #54.

Two settled trade-offs:

- On client disconnect, cancel the upstream call immediately. Rationale: users must not pay for content they never received; letting upstream keep running means the platform pays upstream for nothing. The cost is settling on local estimation, marked on the bill.
- On mid-stream interruption with no usage, settle on local estimation — do not copy sandbase's free-transaction approach. Free is friendliest to users but rewards deliberately cutting the connection; the platform's cost becomes an exploitable loophole. An explicitly-marked "estimated" bill is the honest middle ground. The never-pay-more-than-the-freeze promise still holds.

### How one request is booked (shipped)

- One request maps to two entries: the freeze and the settlement. Idempotency keys: `req-<id>:hold`, and the settlement's key derived from it (`oxsum_core::settlement_key_for`).
- The settlement entry's description carries a compact JSON: request id, model, input and output token counts, both prices, settlement type. The description is hashed into the entry, so what the user verifies is not just "how much was charged" but "by how many tokens at what price". The description caps at 512 characters — enough.
- The full request state (in flight, settled, anomalous) will live in oxsum's own `requests` table — planned, not built; the requests page that reads it is issue #55. Until then, in-flight gateway holds are tracked in the sweeper's `oxsum.open_holds` watch table and the ledger stays the source of truth. Only what needs proving goes into the ledger.

## Bills and verification

### Bills page (shipped in part; the rest is #54)

`/dashboard/bills` (`crates/web/src/app.rs`) lists the organization's settled entries, newest first, each with the date it was booked, its id, what it charged in credits and the content hash its proof verifies against. A settled entry is one that released a hold — a gateway turn, or a hold and a settlement driven through the wallet API by hand. Top-ups and holds that have not settled are not bills and are not listed. The page is readable by every member. (Shipped.)

- CSV and JSON export: `GET /dashboard/bills/export.csv` and `GET /dashboard/bills/export.json` answer the page's own list as a file, with `Content-Disposition: attachment`, so a browser saves it and a command-line client fetches the same bytes with the same session cookie. Both files carry the same fields and the same rows — `bookedOn`, `entryId`, `chargedMinor` and `contentHash` — so a record can be archived and checked against the ledger later; the charge is the ledger's integer in minor units in both files, where the page renders the same amount in credits, and the CSV is RFC 4180. (Shipped.)
- The page and both exports carry the newest hundred settled entries; nothing paginates yet, like every other list in the product. (Shipped.)

The transaction log page is still what it was (`/dashboard/log`, `crates/web/src/app.rs`): the organization's newest ledger entries, newest first, each with its index, id, description (for a settled request, the compact JSON carrying model, token counts, prices and settlement type) and content hash. It lists twenty-five entries on the overview and a hundred on its own page, and it is readable by every member. It is not the bill page, and neither is the bills page yet:

- Lists the organization's every transaction newest-first: top-ups, bonuses, adjustments, requests. A request row shows model, token counts, freeze, actual charge and settlement type. (Planned, #54 — the shipped bills page lists settled entries alone; the rest of the same facts are only inside the entry description.)
- Members see only requests from their own keys plus organization-level top-ups. Owners and admins see everything. (Planned, #54 — the shipped bills page and the shipped transaction log both apply no member filter.)
- A row links to that entry's proof bundle, so a bill can be verified from the page. (Planned, #54 — the shipped page lists each entry's content hash and id, and the proof endpoint takes the id, but nothing on the bills page fetches the proof.)

### Browser verification

- Shipped: every entry verifies locally in the browser. The verification logic is the WASM build of `verify_bundle` — the same code the server runs. The public `/verify` page (`crates/web/src/app.rs`) takes a proof bundle and a content hash and runs the check in the browser; the chat page links to it with both fields prefilled after every settled turn.
- Planned (no issue filed yet): each visit to the bill page stores the tree heads seen into browser-local storage. On the next visit, consistency proofs are fetched automatically to confirm the log only ever appended — history was not rewritten. The user does nothing. The consistency endpoint it would call is shipped (`GET /api/v1/log/consistency?from=`, TODO B-7); the browser-local archive that calls it is not.
- Witness-signed tree heads follow later, so multiple users see the same log.

Verification proves: this record was not altered after being written, and history was not rewritten. It does not prove: upstream really returned that many tokens. The page states this sentence verbatim, without inflating it. (Shipped.)

## Pages

### Organization view (logged-in users)

| Page | Content | Who sees it | Status |
| --- | --- | --- | --- |
| Overview | Available balance, the frozen total and this month's spend; in-flight requests with their frozen upper bound and live streaming progress | All members | Shipped. The three figures are the overview's own integers (`DashboardData`, `crates/web/src/api.rs`), read from the organization's ledger: the frozen total is the wallet's reserved balance — the sum of the outstanding holds — and this month's spend is what settlements charged on or after the first of the current UTC month, so a top-up is not spend and an outstanding hold is not either. Progress is forwarded characters, not tokens (docs/decisions.md). |
| Chat | Top up, pick a model and chat; each turn's billing live, with a verify-this-bill link | All members | Shipped (`crates/web/src/chat.rs`) |
| Requests | Each request's status, usage and cost, filterable by key and model | All members; members see only their own | Planned, #55 |
| Bills | The organization's settled entries with the date booked, the charge in credits and the content hash, newest first; CSV and JSON export of the same list, carrying the charge as the ledger's integer | Every member, with no member filter yet | Shipped in part: the list and both exports ship (`crates/web/src/app.rs`, `/dashboard/bills`); the full transaction view, the member filter and the per-row proof link are planned, #54 |
| API keys | Create, revoke | Members manage their own; admins manage all | Shipped |
| Members | Everyone in the organization, with their role and join date. Invite, remove, change roles, transfer ownership are the documented additions | Read-only table: every member. Management: owners and admins | The read-only table is shipped and open to every member; invite, remove, change roles and transfer ownership are planned, #56 |
| Transaction log | The organization's newest ledger entries with their content hashes | All members | Shipped, but not part of the original spec; the bills page is now the settled, exportable view of the same ledger |

### Platform admin

None of these pages exists. The platform admin surface today is the `/api/v1/admin` REST API under the operator token (`OXSUM_ADMIN_TOKEN`), and it serves channels and prices only (`crates/server/src/admin.rs`): list and repoint a channel, append a price version, read a channel's version history. Every page below is planned under issue #57, and the adjustments and signup bonus the first row needs are #60.

| Page | Content | Status |
| --- | --- | --- |
| Organizations | Every organization's balance; top up, adjust | Planned, #57 (adjustments: #60) |
| Channels & prices | Configure upstreams and model prices; view price version history | Planned, #57 — the endpoints behind it are shipped |
| In-flight requests | Every unsettled hold, globally | Planned, #57 |
| Anomalies | Requests with settlement type `capped`, `estimated`, `client_cancelled` or `swept`, summarizable per channel to see where the platform loses money upstream | Planned, #57 |
| Closing | Monthly closing; after closing, that month accepts no new entries and a closing record is produced | Planned, #57 |

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

- The requests page — #55
- Membership management: invite, remove, change roles, transfer ownership — #56
- The platform-admin pages: organizations, channels and prices, in-flight requests, anomalies, closing — #57
- Team organizations and switching the acting organization — #58
- Invitations: one link, seven days, usable once — #59
- Platform-admin adjustments and the signup bonus — #60
- The bill page's browser-local tree head archive with automatic consistency proofs — no issue filed yet
