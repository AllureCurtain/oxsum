# Technical decisions

New decisions go on top. Overturned decisions are never deleted; mark them "Superseded" and name the decision that replaces them.

## 2026-10-01 All tenants share one connection pool: `SET LOCAL search_path` at the start of every transaction

- Status: Adopted. Implemented 2026-10-02; the two points the plan below left open were settled while implementing, see "Implementation notes".
- Background: `Tenants` currently opens one `PgPool` per tenant (`PostgresStore::connect_with` pins `search_path` to the tenant schema via connection options), so connection count grows linearly with tenant count. Each PostgreSQL connection is a backend process; a few hundred tenants would exhaust the database.
- Decision:
  - One `PgPool` for the whole database, fixed size (`max_connections` set per deployment); `Tenants` degrades from "open a pool" to "build a lightweight ledger facade per tenant id" and holds no connection resources.
  - Tenant targeting uses a transaction-scoped session variable: **the first statement of every transaction is `SET LOCAL search_path = 'ledger_<tenant>'`, followed by doubleentry's queries.** `SET LOCAL` only lives inside its transaction and vanishes on COMMIT or ROLLBACK (verified empirically on local PostgreSQL 17), so a connection returning to the pool cannot leak one tenant's search_path to the next user — that is the entire reason it beats plain `SET` (plain `SET` survives COMMIT on the session, confirmed empirically; on a pooled connection that is a cross-tenant landmine).
  - Storage-layer changes in doubleentry (`crates/doubleentry`, marked `oxsum change (not upstream)`):
    - A new constructor path: `PostgresStore::new(pool, ledger).in_schema(schema)` accepts an externally owned shared pool and no longer requires search_path pinned in connection options.
    - All 30 query/transaction sites that currently run directly on the pool (`execute/fetch_*(&self.pool)`, `pool.begin()`) funnel through one internal entry point: transaction paths prepend `SET LOCAL`, read-only paths wrap in a tiny `BEGIN; SET LOCAL ...; <query>; ROLLBACK`. This is where the real work of this change is.
    - `migrate` is the exception: it already runs `CREATE SCHEMA`/`execute_schema` on a dedicated connection (the schema SQL carries its own BEGIN/COMMIT, see the migrate-lock decision); a transaction-scoped `SET LOCAL` at the start of that dedicated connection suffices, mechanism unchanged.
    - `migrate`'s existing `current_schema()` check upgrades in meaning: from "the pool's default search_path must be the tenant schema" to "the check runs inside the transaction, after SET LOCAL". Failure still reports `WrongSearchPath`; the guard stays.
  - oxsum-core side: `Wallet::open` takes the shared pool plus a schema name and never connects itself; `Tenants` only caches `Wallet` facades.
  - Cross-tenant defenses (two layers):
    1. `SET LOCAL`'s transaction-scoped lifetime guarantees a returned connection cannot carry the previous tenant's search_path.
    2. Tenant schema names are always assembled from validated tenant ids (`validate_tenant_id`, existing), with double quotes escaped when spliced into SQL (the same approach `migrate` already uses).
  - Test baseline: the existing isolation tests (`tenants_are_isolated` and the other 7 wallet integration tests) stay untouched, plus two targeted cases — two tenants on the shared pool cannot see each other's reads or writes; one connection reused by tenants A then B then A does not leak.
- Rejected:
  - Keep one pool per tenant with a smaller `max_connections`: only delays the problem; with enough tenants every pool starves and total connections still grow linearly.
  - Prefix every SQL with `ledger_<tenant>.`: doubleentry has 55 `sqlx::query` sites; a full rewrite is error-prone, would make the upstream diff unrecognizable, and `search_path` is exactly the mechanism PostgreSQL provides for this.
  - Plain `SET` plus an `after_connect` reset hook: correctness depends on remembering to reset everywhere; one missed path cross-tenant-leaks. `SET LOCAL` does not depend on discipline.
- Why the append lock is unaffected: all three write paths use `pg_advisory_xact_lock` (transaction-scoped, bound to the transaction by definition); the migrate-lock decision already switched `MIGRATE_LOCK` to per-ledger derived keys, so two ledgers on the shared pool contend on different locks and do not block each other.
- Implementation notes (2026-10-02), where the plan met the code:
  - One primitive, `PostgresStore::begin`, opens a transaction and issues `SET LOCAL search_path` as its first statement. Every statement the store makes opens its transaction through it, so read paths, write paths and `migrate` cannot drift apart, and the pin is a property of the code path rather than of remembering to reset. Read-only paths roll back, which is free and leaves nothing behind.
  - The reference DDL (`crates/doubleentry/schema/postgres.sql`) carries its own `BEGIN;`/`COMMIT;`. A `SET LOCAL` issued before it would therefore land outside any transaction block and be **silently ignored** — PostgreSQL only warns (verified on local PostgreSQL 17) — leaving the tables to land wherever the pooled connection resolved. `execute_schema` opens the transaction itself, runs the DDL inside it, and closes it with `COMMIT`/`ROLLBACK` whichever way the DDL ends. Trusting the DDL's own wrapper instead would leave an open transaction on a pooled connection the day upstream drops it, which is the one failure mode a shared pool cannot absorb.
  - `WrongSearchPath` keeps its name and stays in `migrate`, but no longer means "the pool is configured wrong": with a schema name the store quotes itself, it cannot fire. It is now an assertion that the pin took effect before any unqualified name was used. The test that used to provoke it (`a_misconfigured_search_path_is_refused`) was replaced by two tests of the new behaviour — a store on a pool that resolves elsewhere writes into its own schema and nothing into `public`, and two ledgers on one pool of one connection never see each other's rows.
  - `connect_with` is kept and still works for a caller that wants a pool of its own; an externally owned pool goes through `new(pool, ledger).in_schema(schema)`, which no longer requires `search_path` in the connection options.
  - The pool's size is the process's whole connection budget, not a per-tenant allowance: `OXSUM_DB_MAX_CONNECTIONS`, default 10.

## 2026-10-01 Gateway HTTP client and token estimation: reqwest + tiktoken-rs

- Status: Adopted. Executed when phase B starts; settled now so no choice is made mid-implementation.
- Decision:
  - The gateway relays with `reqwest` (0.12, default rustls): stream upstream via `bytes_stream()`, forwarding to the client while accumulating forwarded bytes for the estimation-on-interrupt path; disconnect detection uses axum's connection notification and a `tokio::select!` branch that drops the upstream future (reqwest has no bare cancel API; dropping the future is the disconnect). Cancelling the upstream means cancelling the charge — that is product.md's settled "client disconnect cancels the upstream call".
  - Freeze-time byte estimation needs no library: `str::len()` is the UTF-8 byte count, plus a fixed per-message overhead.
  - Local token estimation on stream interruption/disconnect uses `tiktoken-rs` (0.6, `o200k_base` vocabulary built in, pure Rust): this is where product.md's "estimate with tiktoken's o200k_base" lands. Fallback path only — normal settlement always uses upstream usage, so the two pricing paths never mix.
  - Upstream timeouts: connect timeout 10 seconds; no read timeout (stream inter-arrival is the upstream's guarantee), the 30-minute hold timeout is the backstop, see product.md.
- Rejected:
  - Hand-rolling on `hyper`: reqwest is the default answer in the axum ecosystem; the saved dependency is not worth the showcase.
  - Maintaining our own BPE vocabulary: `tiktoken-rs` already embeds o200k_base; a self-maintained vocabulary is pure burden.
  - Estimating the freeze with a tokenizer: already rejected in product.md — tokenizers differ per model, an underestimate breaks the freeze promise; byte estimation overshoots but has a guaranteed upper bound.

## 2026-10-01 Pages move to Leptos, replacing Topcoat

- Status: Adopted. Replaces the Topcoat part of "full-stack Rust: axum + Topcoat" below; the axum, doubleentry and WASM-verification conclusions stand.
- Decision: pages use Leptos 0.8; the admin dashboard, bill page, chat page and verification page all live in one Leptos app. Server-side rendering plus browser-side WASM hydration, mounted through the official `leptos_axum`, one binary with the API. `verify_bundle` is called directly inside a Leptos component; no separate wasm-bindgen crate.
- Verified premise: `cargo build -p doubleentry --features serde --target wasm32-unknown-unknown` passes on this machine. The only obstacle was uuid 1.26's `v7` feature force-requiring a randomness source on wasm32; solved with an `oxsum change (not upstream)` target dependency in `crates/doubleentry/Cargo.toml`: `[target.'cfg(target_arch = "wasm32")'.dependencies] uuid = { version = "1", features = ["js"] }`. Browser-side randomness goes through wasm-bindgen; native builds are unaffected.
- Why:
  - Verification is oxsum's core selling point. Leptos's browser side is already WASM, so `verify_bundle` is called straight from a component and the frontend is one technology. Topcoat's design translates Rust expressions to JS, so the verification page would need a separate wasm-bindgen crate with hand-written JS glue — two technologies side by side, and Topcoat's "no frontend build step" advantage disappears anyway.
  - Maturity: Leptos has been maintained since 2022 with about 1.32M downloads in 90 days; Topcoat 0.1 shipped in 2026-04 and its README says "early-stage and experimental, expect breaking changes". Mid-project migration risk is far smaller with Leptos.
  - Server functions let pages call server code directly, no hand-written page API layer.
- Rejected:
  - Staying on Topcoat: accepting a separate verification crate and framework instability for the "newer and shinier" optics.
  - Dioxus: equally capable, but its energy is mostly on desktop and mobile.
  - askama + htmx: the templating is very mature, but it hardly showcases Rust on the frontend and the verification page still needs a separate WASM crate.
- Costs and knock-on constraints:
  - The build goes from "just cargo" to also needing `cargo-leptos`; server and WASM code are separated by feature.
  - Toolchain: Leptos 0.8's MSRV is below 1.98; rust-toolchain.toml keeps 1.98, with its comment rewritten to no longer attribute the pin to Topcoat.
  - Sessions change with it: Topcoat's built-in `topcoat::session` is gone, see the "users and login" decision below.

## 2026-10-01 Users and login: self-built sessions, password-auth, hand-written user/org layer

- Status: Adopted
- Decision:
  - Login sessions are self-built: one `sessions` table (token hash, user id, expiry); token generation, new-token-on-login, logout and renewal are implemented by oxsum. Cookie carries the usual safety attributes (HttpOnly, SameSite=Lax, Secure).
  - Password hashing uses `password-auth` (RustCrypto, 1.0, argon2 inside): two functions, hash and verify, with no knobs to get wrong.
  - Users, organizations, memberships, roles and API keys are all hand-written. Four roles (platform admin, owner, admin, member); permission checks are one `match`.
  - CSRF: mutating operations never use GET, plus an Origin-check middleware.
  - Email later via `lettre` (SMTP); GitHub login later via `oauth2` or `openidconnect`. Neither in v1.
- Why:
  - The user/org layer is tightly coupled to the ledger: registration creates user, personal organization and ledger in one transaction (see "a tenant is an organization"); removing a member has to deal with their keys. Off-the-shelf user libraries know nothing about "a ledger hanging under an organization".
  - Authorization engines like casbin or cedar are built for many, constantly-changing rules; here the configuration would outgrow the business code. This layer is exactly the business code worth showing in a portfolio piece.
  - The demo path is "docker compose up, one binary, and you can play"; adding a standalone identity service (Rauthy, Kanidm, Keycloak, Zitadel, etc.) lengthens the path and adds a deployment unit. Enterprise SSO later plugs in via `openidconnect` without redesign.
- Rejected:
  - axum-login + tower-sessions: the common axum-ecosystem pairing, but its last release was 2025-07 and it buys nothing over a self-built session table. Note: the tower-sessions crate itself remains evaluable where a default implementation is wanted; this decision means "we write the session table and renewal logic", not "that crate is banned".
  - Topcoat's built-in `topcoat::session`: retired together with the page layer moving to Leptos.
- Known cost: argon2 verification is slow (~100ms), so the login endpoint responds noticeably; acceptable for a portfolio's login frequency — slow hashing is exactly the protection wanted if the credential database leaks.

## 2026-10-01 doubleentry's APPEND_LOCK derived per ledger id

- Status: Adopted
- Decision: in `crates/doubleentry/src/storage/postgres.rs`, the constant `APPEND_LOCK` is replaced by `append_lock_key(ledger_id)`: blake3 over a domain-separated string plus the ledger id, first 8 bytes as the advisory lock key. The key is computed when the `PostgresStore` is constructed and exposed via `append_lock()` for testing. Marked `oxsum change (not upstream)`.
- Why: an advisory lock's scope is the whole database. Upstream assumes one ledger per database, so a constant is fine there. oxsum puts many tenant schemas in one database; with a constant, every tenant's writes serialize against every other's.
- Why derived from the ledger id, not the schema name: the same ledger opened through different pools must still contend on the same lock, or log positions would collide.
- Hash collision: if two ledgers' keys collide on the 64-bit key, the consequence is only that those two ledgers serialize with each other — slower, not wrong. This lock only orders writes; it carries no data.
- Test: `ledgers_in_one_database_do_not_share_an_append_lock` in `crates/doubleentry/tests/postgres.rs`. While one ledger's lock is held, another ledger writes through; the same ledger keeps waiting.

## 2026-10-01 A tenant is an organization; every user gets a personal organization at signup

- Status: Adopted. Refines "one tenant, one schema": the "tenant" in that entry means the organization.
- Decision:
  - Balances and ledgers hang on organizations; one organization, one ledger (schema `ledger_<org>`).
  - Signup creates the user, the personal organization (`personal`) and the owner membership in one transaction. A personal user is just "an organization with one member" — the UI never surfaces the organization layer.
  - Users can create team organizations and join several. Roles: owner, admin, member.
  - API keys belong to organizations and hold no balance themselves; they are credentials. Web login and API keys are two auth channels resolving to the same "current organization".
  - Users, organizations, memberships and API keys are oxsum's own tables, outside the ledger schemas, added via migrations.
- Why: every comparable product does this; adding people later means adding members, not migrating ledgers.
  - sandbase: personal organization at signup, balance on the organization, API keys as organization-scoped credentials.
  - OpenAI: prepaid balance on the organization; projects only divide spend within it.
  - OpenRouter: personal accounts and organizations are separate; organizations share a credit pool with admin and member roles.
- Rejected:
  - One ledger per user plus a separate organization-ledger scheme: two models coexisting; moving from personal to team means migrating the ledger.
  - Balance on the user (new-api's approach): team-shared credit becomes impossible later.
- Deferred: per-key sub-limits (sandbase and LiteLLM have them). The approach waits until the gateway works, see TODO.md.

## 2026-10-01 Wallet service first, then AI gateway, then chat UI

- Status: Adopted
- Decision: three phases, each independently demoable.
  - A. Wallet service: make the existing multi-tenant wallet API solid.
  - B. AI gateway: an OpenAI-compatible proxy; point base_url at oxsum and any OpenAI client works. Freeze an upper bound on entry, settle on upstream usage. The admin dashboard, WASM verification and witness all live in this phase.
  - C. Chat UI: on top of B, a chat page (framework per "pages move to Leptos") — top up and chat in the browser.
- Why:
  - With only the wallet API, someone has to write a client before they can try it. B gives oxsum real callers, and streaming relays, mid-stream interruptions and client retries all actually happen.
  - A is B's core; make A solid and B stands. C is a bonus, saved for last.
  - Comparable products all leave billing gaps, which is exactly B's selling point:
    - sandbase is postpaid with no freezing; its own comments admit balances can go negative under concurrency.
    - OpenAI's spend limits are after-the-fact interception; the docs admit actual spend may slightly exceed the cap.
    - LiteLLM added budget reservation in 2026-04, but the freeze lives in a Redis counter kept alive by TTL, not in one transaction with the ledger, and with no proofs.
  - oxsum's freeze and balance check complete inside one database transaction, and every bill carries a Merkle proof.
- Rejected:
  - A only: nobody can try it without writing their own client; not persuasive as a portfolio.
  - C directly: the largest workload, and a chat page is meaningless before the gateway works.
- Known cost: B overlaps with new-api and LiteLLM. The goal is not to replace them but to build a small gateway whose billing cannot go wrong and whose bills prove themselves.

## 2026-10-01 Fix doubleentry's concurrent migrate colliding on the extension's unique constraint

- Status: Adopted
- Decision: in `crates/doubleentry/src/storage/postgres.rs`'s `execute_schema`, a database-level session advisory lock serializes migrations. Marked `oxsum change (not upstream)`.
- Why: multiple tenants migrating an empty database concurrently both pass the `CREATE EXTENSION IF NOT EXISTS btree_gist` existence check and the loser hits `pg_extension`'s unique constraint; reproduced in the spike. A session lock rather than a transaction lock, because the schema SQL carries its own BEGIN/COMMIT which would end the enclosing transaction early and drop a transaction-level lock halfway through.
- Rejected: wrapping a lock in oxsum-core (what the spike did): it only routes around the problem; any other caller hits it again.

## 2026-10-01 One tenant, one schema

- Status: Adopted
- Decision: each tenant uses its own PostgreSQL schema `ledger_<tenant>`, holding one doubleentry ledger.
- Why: doubleentry is designed assuming "one database holds one ledger". Ledgers are physically isolated; their Merkle trees do not leak entry counts to each other, and there is no "forgot the filter column" data-leak risk.
- Rejected:
  - One ledger plus a tenant column: all tenants share one Merkle tree; closing a month would commit other tenants into the seal, and proofs would leak log sizes.
  - One tenant, one database: connection count and operational cost are too high.
- Known costs:
  - doubleentry's `APPEND_LOCK` was a global constant, so all tenants' writes in one database serialized with each other. Solved, see "doubleentry's APPEND_LOCK derived per ledger id".
  - Each tenant opened its own connection pool. To be changed in a later task, see TODO.md.

## 2026-10-01 Vendor doubleentry's source; no crates.io dependency

- Status: Adopted
- Decision: copy hupe1980/doubleentry at commit `58b8739` (0.7.0) into `crates/doubleentry`, keeping LICENSE-MIT and LICENSE-APACHE. Every change is marked `oxsum change (not upstream)`.
- Why: we need to change the engine for our needs (multi-tenant locks, connection pooling, migrate) without being bound to upstream's release cadence. The markers keep future diffs and merges of upstream fixes possible.
- Rejected:
  - crates.io dependency plus a patch: once the changes pile up, unmaintainable.
  - PRs upstream only: uncontrollable cycle, and some changes only make sense in the multi-tenant setting; upstream may not take them.

## 2026-10-01 Full-stack Rust: axum + Topcoat

- Status: Superseded. The page part is replaced by "pages move to Leptos, replacing Topcoat"; axum as the API layer stands.
- Decision:
  - API in axum 0.8
  - Pages in Topcoat 0.9: server-side rendering, interactions translated from Rust expressions to JS
  - Topcoat mounted onto the axum Router via `TowerRoute`, one binary
  - Verification page in WASM
- Why: the project is a Rust portfolio piece meant to show full-stack Rust. Verification logic sharing one code base with the server shows Rust's edge directly.
- Rejected at the time:
  - React frontend: not full-stack Rust.
  - Leptos/Dioxus: equally viable, but Topcoat was newer and tokio-team maintained.
- Underestimated then: the verification WASM needs its own crate under Topcoat; the framework is very new (0.1 in 2026-04, self-described "early-stage and experimental"), so a breaking release mid-project was likely. Both became reasons to move to Leptos.

## 2026-10-01 Positioning: portfolio demo, not a commercial product

- Status: Adopted
- Decision: build a complete, verifiable AI credit wallet demo as a GitHub portfolio piece.
- Why: every commercial direction investigated fails to hold up. Ledger services have Formance, Midaz and Blnk; hold/settle has Blnk and TigerBeetle; AI-usage reconciliation demand is weak. The piece's value is being complete, demonstrable and showing Rust craft — none of that depends on a market gap.
- Rejected:
  - Selling verifiable bills to relay operators: a Merkle proof only shows records were not altered after the fact; it cannot prove the record was honest at write time.
  - A reconciliation service for AI spend against upstream bills: the recoverable amounts on the enterprise side are small, and relay operators' willingness to pay is low.
  - The research lives in three documents under `D:\Study\project`.
