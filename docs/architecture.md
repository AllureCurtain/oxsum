# Architecture

## Overview

```
Browser ──→ Leptos pages (admin dashboard, bill page, chat page) ──┐
          WASM verification (same code, hydrated, runs locally)     │
OpenAI client ──→ Gateway /v1/chat/completions ────────────────────┤  one process, one binary
API callers ──→ axum /api/v1 ──────────────────────────────────────┤
                                                                   ▼
                     oxsum-core (Wallet / Tenants / Db)  ──→ upstream LLM (gateway relay)
                                                                   ▼
                     doubleentry (bookkeeping, Merkle log)
                                                                   ▼
                     PostgreSQL: one ledger schema per organization, plus one `oxsum` schema
                     holding users, organizations, memberships and API keys
```

The axum API, the core, doubleentry and the gateway exist today, with oxsum's own identity tables in one `oxsum` schema. The Leptos pages are built too: the dashboard (TODO B-5), the bill page, the verification page and the chat page are all served by the same binary, so TODO.md's phases A, B and C are closed.

## Modules

| Module | Location | Responsibility |
| --- | --- | --- |
| doubleentry | `crates/doubleentry` | Double-entry bookkeeping, balance limits, pending layer, Merkle inclusion and consistency proofs, period closing. Vendored, see docs/decisions.md |
| Wallet | `crates/core/src/wallet.rs` | One tenant's wallet: top-up, hold, settle, balance, proof bundles, and the transactions the bills page lists and exports (`recent_transactions`). Built on the shared pool, never on a pool of its own |
| Pricing | `crates/core/src/billing.rs` | What a turn costs and what the ledger records: the input upper bound, the freeze (the dearest set that could match), the itemized price book — conditional rules resolved most-specific-wins — the priced lines a settlement decomposes into, the fail-closed check that names a metered dimension no set can bill (`unpriced`), the local `o200k_base` estimate, and the settlement record. No I/O, so it is the same arithmetic in a test and in a request |
| Usage records | `crates/core/src/usage.rs` | The normalized `UsageRecord` every upstream is read into, the bounds on the caller's attribution, the `oxsum.usage_records` row written beside each landed settlement (issue #102), the `oxsum.usage_daily` rollup the write maintains transactionally for the usage dashboard (issue #126), and the charged-versus-upstream sums the admin margin view reads (issue #112) |
| Usage adapters | `crates/core/src/adapters.rs` | The protocol-keyed `UsageAdapter` registry: `adapter_for` resolves the channel's `protocol` column to the adapter that reads its usage reports into the record. OpenAI is the only implementation today; a protocol the registry does not know is refused at channel write (D9, issue #104) |
| Channels | `crates/core/src/channels.rs` | The channels and their prices: the append-only price versions, the resolution a request prices itself by (channel, version, price, upstream address), and the sealed upstream credential. Sealing is AES-256-GCM under `OXSUM_SECRET_KEY`, see docs/decisions.md |
| Tenants | `crates/core/src/tenants.rs` | Facades over the shared pool, cached per tenant: a tenant costs a facade, not connections. See docs/decisions.md "all tenants share one connection pool" |
| Db | `crates/core/src/db.rs` | The process's one pool, plus oxsum's own tables: `migrate` creates the `oxsum` schema and applies `crates/core/migrations/` in order, each file and its recorded version in one transaction |
| Identity | `crates/core/src/users.rs`, `orgs.rs`, `keys.rs` | Registration (user, personal organization, owner membership and first API key, one transaction), organizations and memberships — reading the members, adding an existing account, removing one, changing a role, transferring ownership — and API keys: mint, resolve, list, revoke. The credential names the organization |
| proof | `crates/verify/src/lib.rs` + `charge.rs`, re-exported by `crates/core/src/proof.rs` | Proof bundle structure and the client-side verify functions: `verify_bundle` on the `/verify` page, `verify_signed_head`/`verify_consistency` for the bills page's archived-head check, and `verify_charge` — the version-dispatched recompute of a settlement description's arithmetic (issue #106) |
| Tree heads | `crates/core/src/heads.rs`, `Wallet::{signed_head, consistency}` | The operator's signed tree heads: the per-tenant origin (`oxsum/ledgers/<tenant_id>`), the C2SP signed-note signed under `oxsum/tree-heads`, and the key publication. The seed is `OXSUM_HEAD_SIGNING_KEY`; signing is on demand and stateless. See docs/decisions.md |
| HTTP | `crates/server/src/` | Routing, API-key middleware, error mapping, and the dashboard's own routes outside `/api/v1` (the billing WebSocket and the bills export) |
| Gateway | `crates/server/src/gateway/` | The OpenAI-compatible `/v1` surface: model list, chat completions, OpenAI-shaped errors, and the relay that freezes before upstream and settles however the turn ends. Its own auth middleware, because a refusal here has to look like OpenAI's. It resolves its channel and price version from the rows in `crates/core` once per request, and records both in the settlement; the channel's `protocol` resolves into the `UsageAdapter` that reads its usage reports. Pricing is in core, see docs/decisions.md. It notes each hold in the sweeper's watch table before taking it, and clears the row when the turn settles |
| Hold sweeper | `crates/core/src/holds.rs`, spawned in `crates/server/src/main.rs` | The background job that settles watched holds older than `OXSUM_HOLD_TIMEOUT` at 0 with kind `swept`, releasing the whole freeze. The watch table (`oxsum.open_holds`) is a finding aid only: the ledger stays the source of truth, and the derived settlement key is the atomic guard against a late settlement landing alongside the sweep. See docs/decisions.md |
| Admin | `crates/server/src/admin.rs` | The platform admin's `/api/v1/admin` surface: channels and their price versions, organizations and adjustments, anomalies and the margin view, redemption-code minting — behind `OXSUM_ADMIN_TOKEN` in a middleware of its own, because this is not an organization's credential. Store and rules are in core, see docs/decisions.md |
| Deposits | `crates/core/src/deposits.rs` | The single record of money arriving: one `oxsum.deposits` row per rail payment, keyed `(rail, organization_id, payment_ref)`, status-walking `pending → confirmed → credited | reversed | expired` with `entry_id` naming the ledger entry once credited. Rails today: `manual` (every `/topups`) and `redemption` (`oxsum.redemption_codes` — SHA-256-only codes, `FOR UPDATE` claim, ledger credit under `redemption:<code_id>`, crash-resumable). Stripe and chain rails join the same table (decision D2, issue #118) |
| Statements | `crates/core/src/statements.rs` | The monthly billing documents: `oxsum.statements` (one per organization and UTC `YYYY-MM`, `draft → finalized`, payment standing `pending | paid | overdue | suspended`, terms and due date snapshotted at issue, `log_from_index`/`log_to_index` pinning the ledger window the lines prove) and `oxsum.statement_lines` (the period's usage aggregated by channel and model). A statement's `paid_minor` is derived from the ledger — credit-line repayments settle the oldest draw first — so any money-in reconciles the book and there is no allocation table. `organizations.payment_terms_days` (migration 0013) feeds the due date (issue #124) |
| Rate limiting | `crates/core/src/ratelimit.rs` | The per-key `requestsPerMinute` window: a `RateLimiter` trait with the in-process sliding-window implementation the gateway and `/api/v1/holds` consult at admission, behind the trait so a shared backend drops in later. The `maxConcurrentHolds` cap is not process state — it is counted from the ledger's pending entries inside `hold_for_key`'s per-key lock (issue #130) |
| Request idempotency | `crates/core/src/idempotency.rs` | The `Idempotency-Key` claims table (`oxsum.idempotency_records`, migration 0016): one row per `(organization, key)` carrying the request fingerprint, the minted request id and the stored answer. The gateway claims before the turn runs; a replay of a streamed turn is answered by the settled receipt the usage-row write completes the record with — the same write the turn, a disconnect's `Drop` and the sweeper all share, so a crash resolves the claim too (issue #132) |
| Pages | `crates/web` | Leptos admin dashboard, bills page (`/dashboard/bills`), chat page, verification page. SSR plus hydration, mounted through the official `leptos_axum`, one binary; the CSV and JSON export beside the bills page is a page route of the server's, because a download has to be a response (docs/decisions.md) |

## Directory plan

What exists today (`crates/doubleentry`, `crates/core`, `crates/verify`, `crates/server`, `crates/web`) stays as is; names and ownership are settled here so no decision is needed while coding:

```
crates/
  doubleentry/          Vendored ledger engine (only changed per the rules in docs/decisions.md)
  core/                 Domain layer: wallet, tenants, proof, identity (users, orgs, keys), pricing
    src/
      wallet.rs         exists
      tenants.rs        exists, shared-pool facade over many ledger facades
      billing.rs        exists: the input upper bound, the freeze, priced usage, the local
                        estimate, the settlement record, and the price book
      channels.rs       exists: channels, append-only price versions, credential sealing, and
                        the (channel, version, price, upstream) a request resolves once
      proof.rs          exists
      heads.rs          exists: operator-signed tree heads (C2SP signed-notes) and the
                        verifying-key publication; the seed is OXSUM_HEAD_SIGNING_KEY
      db.rs             exists: the one pool plus oxsum's own migration runner
      users.rs          exists: registration, password hashing, login/logout sessions (B-4)
      orgs.rs           exists: organizations, memberships, roles; invitations come later
      keys.rs           exists: API key mint, resolve, list, revoke
      sessions.rs       exists: session table reads, writes and renewal; login, logout,
                        and the Principal (key or session) the auth middleware resolves
      usage.rs          exists: the normalized UsageRecord, its attribution bounds,
                        and the oxsum.usage_records persistence beside the settlement
      adapters.rs       exists: the protocol-keyed UsageAdapter registry that reads
                        each channel's usage reports into the record (OpenAI first)
    migrations/         exists: oxsum's own tables (users/orgs/memberships/keys, channels
                        and their price versions, usage records). Create-table SQL
                        only, applied by oxsum's own migration runner; ledger schemas
                        stay owned by doubleentry's migrate, never mixed
  verify/               Pure verification shared with the browser: the proof bundle,
                        verify_bundle, the signed-head and consistency checks, and the
                        money SCALE; wasm32-clean, re-exported by
                        oxsum-core so one implementation serves the server and the page
  server/               HTTP assembly: axum Router, error mapping, auth middleware,
                        exposing core as /api/v1 and the gateway as /v1
    src/gateway/        exists: the /v1 surface — router, OpenAI error shape, request
                        parsing, the SSE relay, and the turn that owns its own settlement
    src/admin.rs        exists: the platform admin's /api/v1/admin surface and its token
    src/bills.rs        exists: the bills export — the dashboard's two GET routes that
                        answer the page's list as CSV and JSON (issue #54)
  web/                  The Leptos pages (B-5 done): `src/app.rs` the components,
                        `src/api.rs` the server functions, `src/bills.rs` the bills row
                        shape and the two exports built from it, `style/main.css` the
                        tokens (see DESIGN.md); built with cargo-leptos, server and
                        browser code separated by feature
```

Principle: **domain logic belongs in core; the gateway does protocol and orchestration only; server only assembles**. The litmus test — if the logic survives replacing the axum layer, it is in the right place. That is why the gateway is a module of the server crate rather than a crate of its own: everything it does is HTTP — routing, an OpenAI-shaped error body, request deserialization, and a relay whose lifetime is the response body's — while every number it charges by is computed in `crates/core/src/billing.rs` and every row it charges by is read through `crates/core/src/channels.rs`. Migrations follow the crate that owns the table (core's tables in `core/migrations/`), applied in dependency order at startup.

## Frontend/backend boundary

- External callers use REST/JSON; the contract is `crates/server/openapi.yaml`.
- Leptos pages live in the same process as the server: SSR calls `oxsum-core` directly, and browser interactions call server code through server functions, with no hand-written page API. Two kinds of call are the exception: the bills page's exports (`GET /dashboard/bills/export.csv` and `.json`) are routes of the server's own, because a download has to be a response rather than a server function's return value; and the platform-admin pages call `/api/v1/admin/*` straight from the browser with the operator token, because that bearer credential is the deployment's — it is not a session, and no server function could authorize with it (docs/decisions.md).
- The verification page runs in the browser on hydrated WASM, calling `oxsum_core::verify_bundle` directly — the same code the server runs. doubleentry is verified to compile for `wasm32-unknown-unknown` (uuid's wasm32 randomness source solved with the `js` feature, see docs/decisions.md).

## Auth and permissions

- API: an organization's API key in `Authorization: Bearer oxs-…`, or a session cookie from web login — both resolved to a principal (the organization, or the user plus their role in it) by middleware; handlers read the principal from the request, never from the path. Neither credential's plaintext is stored: the database keeps SHA-256 hashes and display prefixes.
- Web login: self-built session table (token hash, user id, organization id, expiry, revocation), cookie with HttpOnly, SameSite=Lax, Secure when the deployment says so; password hashing with password-auth (argon2). A session dies with its membership. Rationale in docs/decisions.md.
- Role rules: members list and revoke only the keys they created (anything else is 404), owners and admins see and revoke all; roles constrain sessions, not keys. One exception, and it is deliberate: managing memberships needs a session in the owner or admin role, so a key is refused there even though it acts as the whole organization — a key is not a person and names no role (docs/decisions.md, "membership management is a person's action"). The member list itself stays readable by every member.
- Between tenants: physical isolation, one schema per tenant, no filter columns in queries. The connection layer shares one pool; `SET LOCAL search_path` at the start of each transaction picks the tenant schema (mechanism in docs/decisions.md). oxsum's own tables are not in those schemas: they are schema-qualified (`oxsum.users`) in the one `oxsum` schema, so nothing about a request's organization can change which identity rows a statement sees.
- Planned:
  - Team organizations and multi-org membership: the schema holds many memberships per user already, and an owner or admin can add an existing account to their organization (#56, the members page); what comes later is creating a second organization and switching between them, and a session acts as the oldest membership until the dashboard adds switching.
  - Last-used timestamps on API keys, see TODO.md.
  - See docs/decisions.md and TODO.md.

## Core data flows

### Billing for one streaming call

1. Before relaying an LLM request, `POST holds` freezes an upper bound.
   - The pool accounts book a debit in the pending layer, split bonus → wallet → credit — `Equity:Bonus` funds what it can, `Liabilities:Wallet` carries what it can, and `Liabilities:CreditLine` carries the rest while undrawn credit remains; available balance drops accordingly.
   - All three pools carry a `FundedReservations` limit; the limit check and the write happen in one database transaction, with the pending layer included in the calculation, so concurrent holds cannot overdraw the pools or the credit line. A hold whose split a racing append invalidated retries with balances re-read rather than refusing a request the combined balance could have served.
   - A gateway request does this itself, before it opens the upstream connection: the freeze is computed in `crates/core/src/billing.rs` from the request text and the output ceiling, and its refusal is answered as OpenAI's 402.
   - Key constraints are enforced inside `Wallet::hold_for_key`, in the transaction that holds the per-key advisory lock and the locked key row (issue #120): the model allowlist is checked against the model the gateway names (`POST /api/v1/holds` passes none and is not governed), and the spend limit reads the constraint row in force — a `budget_duration` makes the committed read periodic, counting the current UTC window's settled charges plus every outstanding hold whatever its age. A replayed hold answers before either check, so a retry cannot be refused by a constraint that changed after its first landing.
2. When the stream ends, `POST settlements` books one entry that does two things:
   - Books a reversal in the pending layer, releasing each pool's slice of the hold
   - Charges actual usage in the settled layer, bonus pool first, moving pools → revenue
   - The same limit checks the release as a backstop, so the amount it gives back cannot exceed what holds reserved. The pairing itself is one-to-one: a settlement names the hold it releases, and the hold's entry is the amount — see docs/decisions.md.
   - The gateway settles when the relayed stream ends, and the settlement is awaited before the stream closes, so a client that read a stream to its end reads a settled bill. The write runs in a task of its own, so a client that hangs up while it is being appended cannot cancel it; a client that hangs up before that cancels the upstream call and settles what had been forwarded from a local estimate.
   - Beside a settlement that landed, the same path writes the turn's normalized usage row to `oxsum.usage_records` (`crates/core/src/usage.rs`, issue #102): the full dimensions pricing looks at, the caller's attribution, and `usage_details.provider_raw` — the mutable-store data an immutable entry description cannot hold. The entry stays the source of truth; a failed row write is drift for the reconciler to report, not a reason to retry the charge.
   - The same write also rolls the turn into `oxsum.usage_daily` in the same transaction (issue #126): one row per `(tenant, booking day, key, channel, model)` sums turns, tokens and charge. The day key is the settlement entry's `booking_date` — the ledger's day, not the write's instant — so the rollup agrees with the statement period's seam. A replayed usage write inserts no row and rolls nothing; a usage row whose settlement entry is missing aggregates nothing and stays reconciler-visible drift.
   - The same write also records `upstream_cost_minor` (issue #112): `Price::upstream_cost` resolves the same set the turn priced under and prices the usage at upstream's rates, so the row holds what the platform paid beside what it charged. The column is nullable by design — untracked, not zero, for a price without an `upstream` block, a swept turn, or history — and `Db::margin` sums the pair per `(channel, model)` for the admin margin view, counting the NULLs as `untrackedTurns`. Upstream cost never enters a settlement description or an organization-facing endpoint.
3. Each step carries its own `idempotencyKey`, so retries are safe. For a gateway turn the hold's key is derived from the request id the response header carries (`req-<id>:hold`), and the settlement's key is derived from the hold's (`oxsum_core::settlement_key_for`).

### The hold sweeper

1. The gateway notes each hold in `oxsum.open_holds` *before* taking it, and deletes the row when the turn settles — however the turn ends, including the `Drop` path for a client that disconnects mid-stream. A row without a hold (the process died in between) heals itself: the sweeper deletes it when the hold is not there.
2. Every 60 seconds the background job settles the rows older than `OXSUM_HOLD_TIMEOUT` at 0 with kind `swept`: the whole freeze is released and the settlement record marks the anomaly. The sweep writes the same usage row — zero counts and zero charge, with the attribution the watch row kept, so the row says whose turn timed out. A hold younger than the timeout is never touched — "old but still streaming" is excluded by the timeout contract, which must exceed the longest possible single request.
3. The sweeper and a late settlement share the derived settlement key, so both cannot take effect: whichever appends first wins, the other sees `Conflict("hold already settled")`, and the row is cleared either way.

### User bill verification

1. Every booking returns a `contentHash` in the receipt; users keep it.
2. To verify, `GET entries/{id}/proof` returns the entry source, inclusion proof and tree head.
3. The client recomputes the hash from the source, checks it against the saved `contentHash`, then links the hash to the tree head with the proof. A single flipped byte in the source fails verification.
4. Why the user should trust the tree head itself: the operator signs each head (`GET /api/v1/log/head`), `GET /api/v1/log/key` publishes the verifying key, and `GET /api/v1/log/consistency?from=` proves that the current head extends one the user archived. The bills page runs that check for the user automatically: it archives the signed head in the browser's `localStorage` keyed by the log's origin, verifies the signature on every visit, and on growth fetches the consistency proof and verifies it in the browser through `oxsum_verify` (`crates/web/src/heads.rs`, `check_head_archive` in `crates/web/src/app.rs` — issue #92). Third-party witness cosignatures, which would remove the need to trust the operator's own key, are not built (see docs/decisions.md and the "Verifying the log's history" section of docs/user-guide.md).
5. The whole verification chain runs in the browser (WASM after hydration); the server only hands out raw bundles and never takes part in the verdict.

## Core entities

Only relationships; the ledger's table structure is authoritative in `crates/doubleentry/schema/postgres.sql`, oxsum's own in `crates/core/migrations/`.

- Organization 1-to-1 ledger, 1 ledger = 1 schema `ledger_<tenant_id>`, and an organization's tenant id is its own UUID without dashes.
- User many-to-many organizations, via memberships carrying a role (owner, admin, member). Registration creates one of each.
- Organization 1-to-many API keys; a key holds no balance itself, it is a credential resolving to its organization.
- Planned (not built): organization 1-to-many invitations.
- User 1-to-many sessions (web login): built; a session acts as the user's oldest membership until the dashboard adds switching.
- Every ledger has seven fixed accounts:
  - `Liabilities:Wallet`: purchased balance, overdraft forbidden
  - `Equity:Bonus`: granted credit — the signup bonus and admin grants; settlement draws it before the wallet
  - `Liabilities:CreditLine`: the drawn-down half of an organization's credit limit (issue #122) — its balance is undrawn credit; a hold or charge debits it exactly as it debits the wallet
  - `Assets:CreditFacility`: the commitment memo paired with the credit line — its debit balance is the granted limit, so what the organization owes is `limit − line balance`
  - `Assets:Cash`: money received from top-ups
  - `Income:Usage`: revenue recognized on settlement
  - `Equity:Adjustments`: operator-side money that is neither cash nor usage revenue — admin adjustments and the signup bonus draw on it, so grants and deductions stay out of the income line
- The pools a hold can draw are Bonus, Wallet and CreditLine, each carrying the `FundedReservations` limit, in bonus → wallet → credit order. A hold reserves against what each pool carries — a pending debit on each pool it draws, so no pool's reservation exceeds what it holds — and a settlement releases exactly that split and charges in the same order. A deduction draws Bonus first as well. Granting or resizing the credit limit books the delta `debit CreditFacility, credit CreditLine` under a per-tenant advisory lock, and the line's own funding rule refuses a shrink below what is already drawn; a top-up credits the drawn part of the line first and the remainder to the wallet, so paying in repays debt before it adds own funds. What callers see stays one balance: every balance read sums the pools (undrawn credit included in `available`), while `creditLimitMinor`/`creditUsedMinor` report the facility separately. Ledgers written before the split migrate lazily, on their next `Wallet::open`: `reclassify_grants` moves the unspent remainder of historical grants — `clamp(grants - settled wallet outflow, 0, wallet balance)` — with one `debit Wallet, credit Bonus` entry under a fixed idempotency key, which spend reads exclude as a rebalancing rather than a charge.
- One entry has many postings. Each posting sits in the settled or the pending layer, and the entry must balance within each layer.
- The Merkle log's leaves are the entries' content hashes.

## Deployment

- One binary `oxsum` plus PostgreSQL 17. All configuration from environment variables, see `.env.example`.
- The database runs locally via `compose.yaml`. Production deployment is undecided, see TODO.md.
