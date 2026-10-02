# TODO

Updated: 2026-10-02

## In progress

None.

## Next

Three phases, in order; the rationale is in docs/decisions.md under "wallet service first, then AI gateway, then chat UI".

### A. Wallet service

1. Generative tests (backend): randomly generate sequences of top-ups, holds and settlements, check them against an in-memory model, verifying balance conservation, no negative balances, and that every proof validates. Done when the tests run 1000 cases in CI, all passing.

### B. AI gateway

2. Minimal gateway loop (backend): an OpenAI-compatible `/v1/chat/completions`, streaming and non-streaming, upstream first speaks the OpenAI-compatible format. Relay with reqwest streaming passthrough, local estimation with tiktoken-rs, see docs/decisions.md "gateway HTTP client and token estimation". Freeze an upper bound estimated from `max_tokens` when the request arrives, settle against the usage returned by upstream. How each case settles — client disconnect, upstream failure, missing usage — gets written into docs/user-guide.md. Done when the official OpenAI SDK works by only changing base_url, concurrent requests cannot overdraw, and every failure case has a test.
3. Channel and model pricing configuration (backend): done when a price change only affects new requests and old bills stay untouched.
4. Web login (full stack): sessions table, cookie, login and logout pages, and role checks (owner/admin/member) on organization and key management, per docs/decisions.md "users and login". Done when someone who is not a member of an organization cannot read it or manage its keys, and a member sees only their own.
5. Create `crates/web`, Leptos admin dashboard (full stack): server-side rendering plus browser hydration, mounted through the official `leptos_axum` into one binary with the API; built with `cargo-leptos`. Done when organizations, members, keys, balances, in-flight holds and the transaction log are all visible, and streaming billing progress is pushed over WebSocket in real time.
6. Verification page (frontend): a Leptos component calling `oxsum_core::verify_bundle` directly; users paste a bundle and their contentHash and see the verdict. Done when changing any single number in the bundle flips the page to verification failure. doubleentry compiling to wasm32 is already verified (uuid's wasm32 randomness source solved with the `js` feature, see docs/decisions.md).
7. Trustworthy tree heads (backend): sign heads based on doubleentry's witness module, plus consistency-proof endpoints. Done when a user holding an old head can verify the new head was appended onto it.
8. Per-key sub-limits (backend): settle the implementation approach first, write it into docs/decisions.md. Done when concurrent requests cannot exceed a key's limit.
9. Demo materials (full stack): a demo script connecting the OpenAI SDK to oxsum, plus a demo GIF in the README. Done when a reader who has never seen the project can follow the README alone and watch a billed request go through.
10. Release: Dockerfile and GitHub Actions CI (fmt, clippy, tests with a PostgreSQL service). Done when CI is green on a fresh clone with no local setup, and the image runs against an external PostgreSQL.

### C. Chat UI

11. Leptos chat page (full stack): after login the user can top up, pick a model and chat, with each turn's billing visible in real time. Done when one browser session completes top-up → chat → proof verification without touching the API by hand.

## Blocked

Nothing.

## Recently completed

- 2026-10-02 Users, organizations and API keys: `oxsum.users`, `organizations`, `memberships` and `api_keys` live in one schema migrated by oxsum itself at startup; registration creates the user, personal organization, owner membership and first key in one transaction; a key (`oxs-` + 32 random bytes, stored only as a SHA-256 hash) resolves the request to its organization, so the ledger endpoints lost their tenant path segment and `OXSUM_API_TOKEN` is gone; with it, 403 `FORBIDDEN` and 409 `CONFLICT`, and `OXSUM_SIGNUP=invite|open`
- 2026-10-02 One connection pool shared by all tenants: `Tenants` holds a single `PgPool` and hands each tenant a cached ledger facade, doubleentry pins `SET LOCAL search_path` at the start of every transaction (`PostgresStore::begin`, marked `oxsum change`), the reference DDL is pinned inside a transaction opened for it, and three new tests cover two tenants on one pool, one connection reused by A then B then A, and a tenant costing no connection of its own
- 2026-10-02 Finalized the documentation standard and branch naming convention: branches use `<type>/<kebab-case-description>`, while issue numbers stay in issue and PR metadata
- 2026-10-02 Documentation and standard audit before the first commit: fixed `.env` loading, invalid OpenAPI YAML, incomplete guide paths and README links; added product.md rules and an AGENTS-only template; gave every TODO item a "Done when"; verified fmt, clippy, the wasm32 build, the OpenAPI parse and the full PostgreSQL-backed test suite
- 2026-10-01 Settled the A-1 shared connection pool mechanism (transaction-scoped `SET LOCAL search_path`, lifecycle verified empirically) and the B-4 crate choices (reqwest + tiktoken-rs); phase-A design is closed
- 2026-10-01 product.md finalized: all 4 open questions decided (GitHub login and image input go post-v1; client disconnect cancels the upstream call; mid-stream interruptions settle on local estimation, marked as estimated)
- 2026-10-01 Frontend framework settled on Leptos 0.8, replacing Topcoat; doubleentry compiling to wasm32 verified (uuid gets a wasm32 target dependency, native builds unaffected)
- 2026-10-01 Users & login settled: self-built session table, password-auth hashing, hand-written user/org layer
- 2026-10-01 doubleentry's APPEND_LOCK derived per ledger id, so writes by different tenants in one database no longer block each other; test covered
- 2026-10-01 Project direction and tenant model settled: A then B then C; balances sit on organizations, every user gets a personal organization at signup
- 2026-10-01 Created the oxsum project: workspace layout, vendored doubleentry (commit 58b8739), core and server crates migrated out of the spike, full doc set in place
- 2026-10-01 Fixed doubleentry's concurrent migrate on an empty database colliding on the extension's unique constraint; test covered
