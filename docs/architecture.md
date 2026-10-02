# Architecture

## Overview

```
Browser ──→ Leptos pages (admin dashboard, bill page, chat page) ──┐
          WASM verification (same code, hydrated, runs locally)     │
OpenAI client ──→ Gateway /v1/chat/completions ────────────────────┤  one process, one binary
API callers ──→ axum /api/v1 ──────────────────────────────────────┤
                                                                   ▼
                     oxsum-core (Wallet / Tenants)    ──→ upstream LLM (gateway relay)
                                                                   ▼
                     doubleentry (bookkeeping, Merkle log)
                                                                   ▼
                     PostgreSQL: one ledger schema per organization, plus oxsum's own
                     user, organization, key and session tables
```

Only the axum API plus the core and doubleentry line exists today. The gateway, the Leptos pages and the user/organization tables are not built yet; the order is in TODO.md.

## Modules

| Module | Location | Responsibility |
| --- | --- | --- |
| doubleentry | `crates/doubleentry` | Double-entry bookkeeping, balance limits, pending layer, Merkle inclusion and consistency proofs, period closing. Vendored, see docs/decisions.md |
| Wallet | `crates/core/src/wallet.rs` | One tenant's wallet: top-up, hold, settle, balance, proof bundles. Built on the shared pool, never on a pool of its own |
| Tenants | `crates/core/src/tenants.rs` | The process's one pool plus a cached ledger facade per tenant: a tenant costs a facade, not connections. See docs/decisions.md "all tenants share one connection pool" |
| proof | `crates/core/src/proof.rs` | Proof bundle structure and the client-side verify function, later called directly inside a Leptos component |
| HTTP | `crates/server/src/` | Routing, Bearer auth, error mapping |
| Gateway | `crates/gateway` (not yet created) | OpenAI-compatible proxy: relays upstream, freezes before the request, settles on usage. reqwest streaming passthrough, tiktoken-rs for fallback estimation, see docs/decisions.md |
| Pages | `crates/web` (not yet created) | Leptos admin dashboard, bill page, chat page. SSR plus hydration, mounted through the official `leptos_axum`, one binary |

## Directory plan

What exists today (`crates/doubleentry`, `crates/core`, `crates/server`) stays as is. The rest is added phase by phase; names and ownership are settled here so no decision is needed while coding:

```
crates/
  doubleentry/          Vendored ledger engine (only changed per the rules in docs/decisions.md)
  core/                 Domain layer: wallet, tenants, proof, plus A-2's users, orgs, keys, sessions
    src/
      wallet.rs         exists
      tenants.rs        exists, shared-pool facade over many ledger facades
      proof.rs          exists
      users.rs          A-2: registration, login checks, argon2 hashing
      orgs.rs           A-2: organizations, memberships, roles, invitations
      keys.rs           A-2: API key create, revoke, hash verification
      sessions.rs       A-2: session table reads, writes and renewal
    migrations/         A-2: oxsum's own tables (users/orgs/memberships/keys/sessions)
                        Create-table SQL only, applied by oxsum's own migration runner;
                        ledger schemas stay owned by doubleentry's migrate, never mixed
  gateway/              B-4: /v1/chat/completions relay, hold/settle orchestration,
                        channels and price versions (the price table is oxsum's too, migration lives here)
  server/               HTTP assembly: axum Router, error mapping, auth middleware,
                        exposing core and gateway as /api/v1 and /v1
  web/                  B-6: Leptos pages, built with cargo-leptos,
                        server and browser code separated by feature
```

Principle: **domain logic belongs in core; gateway does protocol and orchestration only; server only assembles**. The litmus test — if the logic survives replacing the axum layer, it is in the right place. Migrations follow the crate that owns the table (core's tables in core/migrations, the price table in gateway), applied in dependency order at startup.

## Frontend/backend boundary

- External callers use REST/JSON; the contract is `crates/server/openapi.yaml`.
- Leptos pages live in the same process as the server: SSR calls `oxsum-core` directly, and browser interactions call server code through server functions, with no hand-written page API.
- The verification page runs in the browser on hydrated WASM, calling `oxsum_core::verify_bundle` directly — the same code the server runs. doubleentry is verified to compile for `wasm32-unknown-unknown` (uuid's wasm32 randomness source solved with the `js` feature, see docs/decisions.md).

## Auth and permissions

- API: a single shared Bearer token (`OXSUM_API_TOKEN`) checked by middleware with a constant-time comparison. Planned to be replaced by organization-owned API keys.
- Web login: self-built session table (token hash, user id, expiry), cookie with HttpOnly, SameSite=Lax, Secure; password hashing with password-auth (argon2). Rationale in docs/decisions.md.
- Between tenants: physical isolation, one schema per tenant, no filter columns in queries. The connection layer shares one pool; `SET LOCAL search_path` at the start of each transaction picks the tenant schema (mechanism in docs/decisions.md).
- Planned:
  - A tenant is an organization. Signup comes with a personal organization; team organizations and multi-org membership follow.
  - API keys belong to organizations and replace the shared token.
  - Web login and API keys are two channels resolving to the same "current organization".
  - See docs/decisions.md and TODO.md.

## Core data flows

### Billing for one streaming call

1. Before relaying an LLM request, `POST holds` freezes an upper bound.
   - The wallet account books a debit in the pending layer; available balance drops accordingly.
   - The wallet carries a `NoDebitBalance` limit; the limit check and the write happen in one database transaction, with the pending layer included in the calculation, so concurrent holds cannot overdraw.
2. When the stream ends, `POST settlements` books one entry that does two things:
   - Books a reversal in the pending layer, releasing the hold
   - Charges actual usage in the settled layer, moving wallet → revenue
3. Each step carries its own `idempotencyKey`, so retries are safe.

### User bill verification

1. Every booking returns a `contentHash` in the receipt; users keep it.
2. To verify, `GET entries/{id}/proof` returns the entry source, inclusion proof and tree head.
3. The client recomputes the hash from the source, checks it against the saved `contentHash`, then links the hash to the tree head with the proof. A single flipped byte in the source fails verification.
4. Not yet built: why the user should trust the tree head itself. Plan: witness signatures, or users archive old heads and apply consistency proofs.
5. The whole verification chain runs in the browser (WASM after hydration); the server only hands out raw bundles and never takes part in the verdict.

## Core entities

Only relationships; table structure is authoritative in `crates/doubleentry/schema/postgres.sql`.

- Tenant 1-to-1 ledger, 1 ledger = 1 schema `ledger_<tenant>`.
- Planned (not built): users many-to-many organizations (via memberships, with roles), organization 1-to-1 tenant, organization 1-to-many API keys; user 1-to-many sessions (web login).
- Every ledger has three fixed accounts:
  - `Liabilities:Wallet`: user balance, overdraft forbidden
  - `Assets:Cash`: money received from top-ups
  - `Income:Usage`: revenue recognized on settlement
- One entry has many postings. Each posting sits in the settled or the pending layer, and the entry must balance within each layer.
- The Merkle log's leaves are the entries' content hashes.

## Deployment

- One binary `oxsum` plus PostgreSQL 17. All configuration from environment variables, see `.env.example`.
- The database runs locally via `compose.yaml`. Production deployment is undecided, see TODO.md.
