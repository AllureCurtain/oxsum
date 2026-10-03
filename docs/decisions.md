# Technical decisions

New decisions go on top. Overturned decisions are never deleted; mark them "Superseded" and name the decision that replaces it.

## 2026-10-03 product.md honesty: the shipped behaviour wins in three places (issue #61)

- Status: Adopted. Implemented 2026-10-03, closing issue #61.
- Background: an audit of docs/product.md against the code found the document describing a larger product than the one that exists — five platform-admin pages, a bills page with exports, a requests page, membership management, team organizations, invitations, the signup bonus and a browser-local tree-head archive, none of which is built. Each of those now has its own issue (#54 the bills page, #55 the requests page, #56 membership management, #57 the platform-admin pages, #58 team organizations and switching, #59 invitations, #60 adjustments and the signup bonus), except the tree-head archive, which has none yet; docs/product.md marks them all as planned. This entry records the three places where the document contradicted behaviour that *is* built; those statements now describe the code.
- Decision:
  - **Self-service top-up on the session is the v1 behaviour.** `POST /api/v1/topups` sits in the `authenticated` router (`crates/server/src/routes.rs`), behind the same credential check as every other organization endpoint: a session cookie or an API key may credit its own organization's balance, and the handler takes an amount and an idempotency key with no description, so no reason is recorded. The chat page's top-up form (`crates/web/src/chat.rs`) is exactly this call, which is why it works with no operator in the loop.
  - **The member list is readable by every member; management is planned.** `get_members` (`crates/web/src/api.rs`) authenticates the session and returns the organization's members with no role check, and the page renders a read-only table (`crates/web/src/app.rs`): reading who is in the organization is not a management action. Invite, remove, change roles and transfer ownership are planned under #56, and those actions will carry their own role checks.
  - **The overview shows money and characters, not tokens.** It shows the available balance, each in-flight request's frozen upper bound and live progress in forwarded characters (`crates/web/src/app.rs`); there is no frozen total and no month-to-date spend on the page, and both stay planned with the overview. Tokens cannot be a live number: upstream reports usage only in the final chunk of a streaming response, so before settlement the page would be presenting an estimate as a measurement. Characters are what has actually arrived.
- Rejected:
  - **Admin-gated top-ups with a required reason** (the operator token plus a description on every credit, product.md's original credit-source table): it would make the chat page's top-up button depend on an operator, and it demands a reason where there is none — the organization is moving its own money in. A reason belongs on an operator's change, which is the shape the planned adjustments (#60) will take.
  - **Gating the member list to owners and admins**: it would add a role rule to a read-only page whose management actions do not exist yet (#56), and the shipped transaction log is already readable by every member.
  - **Printing live token progress on the overview**: upstream sends usage only with the final chunk, so any live token count would be an estimate shown as a measurement; if the overview ever shows tokens, it will be at settlement.
- Implementation notes: documentation only — `docs/product.md` (every page and flow marked shipped or planned, and the three statements corrected), `TODO.md` (the planned items as phase D under Next; this change under Recently completed). No code, no `openapi.yaml`.

## 2026-10-03 Chat page: the browser chats with an API key, bills through a page server function (issue #39)

- Status: Adopted. Implemented 2026-10-03, closing issue #39 (TODO C-11).
- Decision: `/dashboard/chat` drives the existing API only — no new REST endpoints, no `openapi.yaml` change.
  - **Top-up** goes to `POST /api/v1/topups` from the browser with the session cookie (the login page's pattern — the cookie is `HttpOnly`, so only a real endpoint call works). The amount is parsed to minor units without floating point, the same integer-only rule as the rest of the money path.
  - **Chat goes through the gateway with an API key.** The cookie is never accepted on `/v1` (#17), so the page mints a key named `chat` through the existing `create_key` server function and keeps it in the browser's `localStorage` — the user never handles it by hand. Pasting a key from the keys page works too. This revises product.md's "no API key needed": what the user experiences is still "log in and chat", but the credential on the wire is a key, not the session.
  - **Chat is streaming `POST /v1/chat/completions`**, parsed as SSE in the browser; the `x-oxsum-request-id` header names the turn before the first frame.
  - **Billing is the existing `/ws/billing` socket**, now behind a shared client module (`crates/web/src/billing_socket.rs`) that delivers typed events to a caller-supplied handler — the dashboard's holds section and the chat page share it. Freeze on `turnStarted`, streaming progress on `turnProgress`, and on `turnSettled` the new `get_turn_bill` `/_pages` server function returns the settlement entry's proof bundle plus the parsed charge and the content hash.
  - **Each settled turn links to `/verify?bundle=…&contentHash=…`**, which prefills both fields and runs the check on load (a page-level change, not an endpoint).
  - **Conversations persist in `localStorage`**; the server keeps nothing (product.md). Restored turns re-fetch their bills — the entry ids are derivable from the request ids, so a reload loses nothing but the live socket state.
- Why a server function for the bill, not a browser fetch of the proof endpoint: the bundle does not carry the entry's content hash (the entry serializes without it), and the browser cannot recompute it — the hash runs over the canonical entry bytes inside doubleentry (`digest` is crate-internal), and `crates/doubleentry` is untouched by rule. `get_turn_bill` (session-authenticated, page API under `/_pages` like the rest of the dashboard) derives the settlement entry id with `oxsum_core::settlement_key_for`/`entry_id_for`, builds the proof bundle server-side, and returns the parsed charge with the bundle JSON and the content hash for the `/verify` link.
- Rejected:
  - A session-authenticated chat relay endpoint: would punch a hole in #17's gateway auth model ("the cookie is never accepted on `/v1`") for page convenience.
  - Deriving the settlement entry id in the browser and fetching the proof over REST: workable for the bundle, but leaves the page with no content hash for the verify link.
  - Storing conversations server-side: product.md says the server keeps nothing, and there is no duty to store users' conversations.

## 2026-10-03 Per-key spend limits: committed spend, serialized per key (issue #32)

- Status: Adopted. Implemented 2026-10-03, closing issue #32 (TODO B-8).
- Background: API keys have full authority (#17: roles constrain sessions, not keys), so one key could spend an organization's whole balance. product.md deferred per-key spend limits to post-v1, and the gateway's freeze discipline made them the natural next step: a limit that only bound settled spend would be after-the-fact interception — the exact failure this file criticizes in OpenAI's limits.
- Decision:
  - **Storage: `oxsum.api_keys.spend_limit_minor`** — nullable bigint, `CHECK (spend_limit_minor >= 0)`, migration `0005_key_spend_limits`. NULL is unlimited, so every existing key is unaffected; 0 means the key can never hold. A limits table would be a 1:1 join for no reason: exactly one limit per key, with the key row's lifetime.
  - **What it limits: committed spend** — settled charges attributed to the key **plus** outstanding holds attributed to the key, in minor units. A hold of `m` is refused when `committed + m > limit`. A pure hold cap lets cumulative spend blow past the limit; a pure spend cap lets in-flight freezes exceed it, and the freeze-before-spend discipline is the product's whole pitch.
  - **Attribution: `provenance.actor`** — the key id in uuid simple form. Hold entries carry it; `Wallet::settle` copies the hold's actor onto the settlement entry (it already reads the hold entry for the held amount), so the release side and the settled charge side attribute to the key with no extra plumbing: the sweeper, the gateway and the wallet API all attribute for free. Keys with no limit attribute too, so a limit added later counts history.
  - **Enforcement: `Wallet::hold_for_key`**, serialized per key around the engine append, with no engine change. It begins a transaction, takes `pg_advisory_xact_lock` on a stable hash of (tenant, key id), re-reads the limit from the key row `FOR UPDATE` (the value in force now, pinned against a concurrent limit change), reads committed spend from the ledger — postings joined to entries on the actor, wallet account, both layers — and only then appends through the engine's normal path. The lock is held until commit, i.e. until after the engine's append committed, so a racing second hold for the same key can only read usage that already includes the first hold. Two racing holds cannot both pass the check.
  - **Lock order is always key-lock, then the engine's per-tenant append lock**, and the engine never takes the key lock: no deadlock. Per-key serialization adds no tenant-level contention beyond the append lock that already serializes the tenant.
  - **Refusal: `WalletError::KeyLimitExceeded`** — 429 `KEY_LIMIT_EXCEEDED` on `/api/v1`, and on the gateway as `insufficient_quota` in the OpenAI shape. A quota refusal, not a balance one: 402 stays the wallet's.
  - **Management: `spendLimitMinor`** on key creation, and `PATCH /api/v1/org/keys/{keyId}` to set or clear it, under the revoke scope rules (members: keys they created; owners, admins and keys acting as the organization: all; anything else 404). Sessions carry no limit — the wallet hold route enforces only for key principals.
- Rejected:
  - **An engine-side limit** (a `BalanceLimit` variant carrying an amount): it would change the account record's canonical encoding and invalidate the content hash of every stored account, for what oxsum can do outside the engine.
  - **Read-then-write in `Wallet::hold` without the lock**: the exact race the `FundedReservations` decision documents — two requests both read pre-append usage and both commit.
  - **An oxsum-side holds table**: duplicates ledger state that can drift, and would need the same lock to be honest.
  - **A DB CHECK on postings**: the per-key total is an aggregate over attributed postings, not a column.
- Implementation notes:
  - `crates/core/src/keys.rs` (`ActingKey`, `update_key_limit`), `crates/core/src/wallet.rs` (`hold_for_key`, `key_committed`, the settle actor flow, the lock-key hash), `Principal::Key(KeyPrincipal)` carrying the acting key, `crates/server/src/auth.rs` resolving it on both middlewares.
  - The concurrency test races ten 300-minor holds against a 1000 limit: exactly three succeed and 900 is committed — deterministic under the serialization.
  - One in-flight `hold_for_key` transiently needs two pool connections (the outer transaction plus the engine's append inside it); `OXSUM_DB_MAX_CONNECTIONS` should stay comfortably above peak concurrent key-holds.

## 2026-10-03 Trustworthy tree heads: operator-signed heads via doubleentry's witness module (issue #30)

- Status: Adopted. Implemented 2026-10-03, closing issue #30 (TODO B-7).
- Background: inclusion proofs answer everything about the history a user was *shown*; they cannot answer whether it is the history everybody else was shown. Each guarantee in the ledger is relative to a head, and a user who archived an old head had no way to check that today's head extends it — the server could serve two heads to two users, each with perfectly verifying proofs.
- Decision:
  - **The operator signs each tenant's ledger head** as a C2SP `signed-note` (a `tlog-checkpoint` body) with an Ed25519 operator note signature (algorithm `0x01`, `SigningKey::sign`) under the fixed key name `oxsum/tree-heads`, using doubleentry's witness module as a dependency. Each tenant's ledger is its own Merkle log, so the origin names the tenant: `oxsum/ledgers/<tenant_id>`.
  - **Signing is on demand and stateless.** The note attests the head at signing time; nothing is stored, so rotating the key rotates every head with no migration. A write landing mid-request leaves the answer one entry behind the absolute latest — still a true statement about the head it attests.
  - **No server-side `Witness`.** The state machine is for independent parties, and the operator witnessing its own log would prove nothing. The C2SP wire format is the interop point: third-party witnesses can cosign these notes later, with software that is not this crate's.
  - **Key management: `OXSUM_HEAD_SIGNING_KEY`**, 32 bytes base64 like `OXSUM_SECRET_KEY` (generate with `openssl rand -base64 32`). Unset means the `/api/v1/log` endpoints answer 503; the wallet serves without it. The verifying key is published at the public `GET /api/v1/log/key` — key name, base64 public key, and the 4-byte signature selector. Fetching it from the server is convenience: a verifier must have chosen the key through a channel the operator does not control, or the signature proves only that the server agrees with itself.
  - **Endpoints:** `GET /api/v1/log/head` (org credential: the current head, signed), `GET /api/v1/log/consistency?from=<size>` (org credential: the signed new head, the old head recomputed from the log, and the proof between them), `GET /api/v1/log/key` (public). `from=0` is 400 — every log extends the empty tree, so such a proof would verify against any history; `from` beyond the log is 400; `from == size` answers the trivial empty proof.
- Rejected:
  - **A `tree_heads` table of stored signed heads.** The ledger is the record; signing on demand is one Ed25519 sign per request and cannot drift from the log it attests.
  - **Cosignatures (algorithm `0x04`) from the operator.** A cosignature says "an independent party had seen this history by time T"; the operator cannot be its own witness, and a self-cosignature would be theater.
  - **Refusing to boot without the key.** The wallet is useful without signed heads; the admin-token precedent (unset means the surface stays closed) fits — 503 names the missing variable instead.
- Implementation notes:
  - `crates/core/src/heads.rs` (origin, signing, key publication), `Wallet::{signed_head, consistency}`, thin routes in `crates/server/src/routes.rs`, `ApiError::ServiceUnavailable` → 503 `SERVICE_UNAVAILABLE`.
  - `crates/doubleentry` is untouched; the `witness` cargo feature is enabled on the workspace dependency.
  - Contract first per the hard rules: openapi.yaml is at 0.6.0 with the three endpoints and the new schemas.

## 2026-10-03 Bill verification in the browser via a shared `oxsum-verify` crate (issue #28)

- Status: Adopted. Implemented 2026-10-03, closing issue #28 (TODO B-6).
- Background: the verification page must run `verify_bundle` in the browser (WASM) — the component calls the same code the server runs, directly, with no round-trip. But `oxsum-core` cannot target `wasm32-unknown-unknown`: it depends on sqlx-postgres with the tokio `net` runtime, which needs OS sockets.
- Decision:
  - **New crate `oxsum-verify` for the pure verification half.** `ProofBundle`, `verify_bundle`, and the money `SCALE` the entry encoding depends on move into `crates/verify`, whose only dependencies are doubleentry (a path dep with the `serde` feature only — the workspace entry's `postgres` feature would pull sqlx and tokio-net back in), serde and serde_json. All three are verified wasm32-clean.
  - **`oxsum-core` re-exports, nothing else changes.** `proof.rs` becomes a re-export shim and `wallet::SCALE` re-exports `oxsum_verify::SCALE`, so `oxsum_core::{verify_bundle, ProofBundle, SCALE}` keep resolving for the server and every existing caller. One definition of the scale: the writer and every verifier cannot drift.
  - **A public `/verify` route, no login.** Verification is the trust surface for anyone holding a bill; a session gate would defeat the point. SSR renders the inert form; the check runs on submit in the browser. The dashboard sidenav links it next to the transaction log.
  - **The verdict is icon plus words, never color alone** (DESIGN.md); a bundle that does not parse and a hash that is not 64 hex characters get their own error states, distinct from verification failure. The page states product.md's honesty sentence verbatim.
- Rejected:
  - **Feature-gating `oxsum-core` for wasm.** Making sqlx, tokio and the rest optional behind a feature would thread `#[cfg]` through the whole domain layer for one pure function; the extraction is smaller, and the boundary (I/O-free) is honest.
  - **Duplicating the check in the web crate.** Two implementations of the verification would drift; the issue requires the same code the server runs.
  - **Verifying through a server function.** The check would then depend on trusting the server — the party being checked.
- Implementation notes:
  - `crates/verify/src/lib.rs` carries a tamper unit test over a golden bundle fixture captured from a real ledger: the fixture verifies, the same fixture with one amount digit changed does not.
  - `docs/development.md` ("Web dashboard") and `docs/user-guide.md` ("Verifying a bill") describe the page; TODO.md B-6 is closed.

## 2026-10-03 The dashboard is a Leptos 0.8 app in `crates/web`, served by the same binary

- Status: Adopted. Implemented 2026-10-03, closing issue #26 (TODO B-5).
- Background: the pages were decided for Leptos 0.8 earlier ("pages move to Leptos, replacing Topcoat"), and the session endpoints were built for pages that did not exist yet ("the Leptos login/logout pages stay with TODO 5"). This item builds them: `/login`, `/logout`, and `/dashboard` with the organization surface — members, keys, balances, in-flight holds and the transaction log — plus live billing progress over WebSocket.
- Decision:
  - **Versions: Leptos 0.8.21, `leptos_axum` 0.8.10, `leptos_router` 0.8.16** — the latest non-yanked 0.8.x line, staying on the major the project chose. `cargo-leptos` 0.3.11 builds both halves; it is installed on the build machine and touches nothing in the repo. Rust stays 1.98 (Leptos 0.8's MSRV is below it).
  - **One binary.** `crates/web` (package `oxsum-web`) is a standard cargo-leptos crate — `ssr` feature for the server side, `hydrate` for the browser side, code separated by feature — and the `oxsum` binary depends on it with the `ssr` feature, mounting it through `leptos_axum::LeptosRoutes` into the same axum router that serves `/api/v1` and `/v1`. The `[[workspace.metadata.leptos]]` section lives in the workspace root (a virtual manifest cannot carry `[package.metadata]`, and cargo-leptos 0.3 only honours `bin-package`/`lib-package` on the workspace section); plain `cargo build` and `cargo test` are unaffected because `ssr` is the web crate's default feature.
  - **SSR calls `oxsum-core` directly; the browser calls server functions.** There is no hand-written page API: the pages' data functions are `#[server]` functions under the `/_pages` prefix, and the database reaches them through Leptos context provided at mount time (`Db` and `Tenants`; the request's own parts, for the session cookie, are provided by `leptos_axum` itself). The `/_pages` prefix keeps the page API off the REST contract's `/api/v1` namespace, so `crates/server/openapi.yaml` stays the REST contract and gains nothing here.
  - **The login/logout pages call the session endpoints from the browser.** `POST /api/v1/auth/login`, `POST /api/v1/auth/logout` and `GET /api/v1/session` are called with `fetch` from the login/logout components: the session cookie is `HttpOnly`, so only a real call to those endpoints sets or clears it on the browser. The dashboard's session guard uses `GET /api/v1/session` the same way; the server functions independently refuse a missing or dead session.
  - **Live billing progress is a process-wide broadcast.** The gateway publishes `BillingEvent`s — turn started, streaming progress (every 1 KiB of forwarded answer text), turn settled with the charge and the kind — onto a `tokio::sync::broadcast` channel on `AppState`, keyed by tenant id. `/ws/billing` (session-cookie auth, like the pages) forwards the caller's organization events as JSON, starting with a snapshot of the holds currently in flight. No database table, no REST endpoint: the watch table stays the durable record, and a missed event is a missed live update, not lost state. The sweeper's own settlements do not publish — the row disappearing is what the dashboard shows.
  - **Three small reads in core, nothing in the engine.** `Db::open_holds_for_tenant` (one organization's in-flight holds), `Db::members` (memberships with emails and roles), `Wallet::recent_entries` (newest log entries with index, id, description and content hash). `crates/doubleentry` is untouched.
- Why:
  - **The `/_pages` prefix rather than the default `/api`.** Server functions at the default prefix would live beside `/api/v1` without colliding today, but the contract file claims the `/api` namespace and a future endpoint could take a name a page function already uses. A separate prefix makes the page API visibly not-the-contract.
  - **A broadcast channel rather than polling or a table.** Polling the watch table from the browser is a second read path with its own staleness; a table of events is durable state for information nobody replays. The events the gateway already knows — it takes the hold, watches the chunks, writes the settlement — are published where they happen, which is the smallest hook that satisfies "pushed in real time".
  - **Session-only WebSocket and server functions.** The dashboard is the session's surface: an API key names no session, and the key's authority (acting as the whole organization) does not map onto pages built around a user and a role. Keys keep using the REST API directly.
  - **The snapshot on connect.** A page opened mid-turn would otherwise show an empty holds list until the next turn starts; the snapshot is one indexed query on the watch table.
- Rejected:
  - **Server functions under `/api`.** The namespace argument above; the REST contract stays exactly the API's.
  - **Progress events per chunk.** A push per upstream packet is a push per few dozen bytes; every 1 KiB of forwarded text is live enough for a dashboard and bounded in rate.
  - **Publishing sweeper settlements.** The sweeper lives in core and has no access to the server's broadcast; threading it through would couple the background job to a page concern for an event the dashboard already shows as the row disappearing.
  - **Leptos 0.9.** A beta line mid-project, against the recorded 0.8 decision; 0.8.21 is the maintained line the decision named.
- Implementation notes:
  - `crates/web/src/app.rs` holds the pages, `crates/web/src/api.rs` the server functions and their view types; `crates/server/src/billing.rs` the event enum, `crates/server/src/ws.rs` the socket route, `crates/server/src/web.rs` the mount. The gateway hook is three publishes: `TurnStarted` in `gateway::run`, progress in `Turn::observe`, `TurnSettled` in `Plan::write` (which covers the stream, the whole-body and the `Drop` paths).
  - The dashboard is an admin/organization surface only: organizations, members, keys, balances, in-flight holds, the transaction log. Chat is TODO C-11 and is not built here.
  - `docs/development.md` gains the frontend commands (`cargo leptos build`, `cargo leptos serve`); `docs/user-guide.md` gains the dashboard section; DESIGN.md's token location is corrected to `crates/web/style/main.css`.

## 2026-10-03 Web login: a session is a second credential, and roles constrain sessions, not keys

- Status: Adopted. Implemented 2026-10-03, closing issue #17 (TODO B-4).
- Background: an API key names an organization and nothing else — a machine credential with no person behind it, which is why `api_keys.created_by` was always null and product.md's role table had a schema but no enforcement. This item makes a *user* a possible principal: a session table, a login, a logout, and role checks on the organization and key endpoints.
- Decision:
  - **Two credentials, one principal.** A request may carry `Authorization: Bearer oxs-…` or the session cookie; both resolve to a `Principal` — `Key` (the organization the key spends for) or `Session` (the user, the organization they act as, and their role in it). The middleware resolves once and hands the principal to the handler; nothing else in the request can name an organization.
  - **Roles constrain sessions, not keys.** A key keeps acting as the whole organization — full authority, pre-session behaviour deliberately unchanged — while a user acts with the authority their role grants them: members list and revoke only the keys they created, owners and admins see and revoke all. The reason is what the credential *is*: a key is a machine credential the organization issued to itself, so there is no person to hold to a role; a session is a person, and a person can be a member. Constraining keys by role would require inventing a person behind a credential that has none.
  - **The session table holds no usable credential.** The cookie value is `oxsess-` plus 32 random bytes; the database keeps only its SHA-256 hash (unique, so the lookup is one index probe), for the same reason `api_keys` keeps only key hashes: a key is 256 bits of machine randomness, so there is nothing to slow down with argon2, and every authenticated request pays for the lookup.
  - **Resolution joins `memberships` on `(user_id, organization_id)`.** A session stops authenticating the moment its membership is gone — the row cannot outlive the authorization it names. No second invalidation mechanism, no session list to sweep.
  - **Login is email + password, one failure for both.** An unknown email and a wrong password answer the same 401 with the same message ("invalid email or password"), and the unknown-email path still pays for one argon2 verification against a process-wide dummy hash, so neither timing nor wording reveals whether an account exists. The hash runs before any session row is written and outside any transaction, so a slow hash never holds a connection — the same rule registration follows.
  - **The session acts as the user's oldest membership.** Invitation flows that create a second membership are a later item, so today this is always the personal organization from signup; switching between several arrives with the dashboard rather than as a login parameter nobody can yet use.
  - **Thirty days, absolute.** `expires_at` is login plus thirty days, no sliding renewal in v1: a session that never expires is a credential that never dies, and sliding renewal is a second expiry policy to get right. `last_used_at` is touched on authentication at most once per five minutes, so the column stays meaningful without a write on every request.
  - **The cookie is `HttpOnly`, `SameSite=Lax`, `Path=/`, and `Secure` when the deployment says so.** Lax, because the dashboard is same-site and Lax is what keeps the cookie off cross-site requests without breaking top-level navigation to it; `OXSUM_SESSION_COOKIE_SECURE` (default `false` for local http development) is what a deployment behind TLS must set. The endpoints stay JSON — the cookie is only the credential, not a page contract — so the same API serves the dashboard and any script holding a session.
  - **An explicit bearer token wins over the cookie.** With no bearer credential at all the cookie is tried; a presented bearer is resolved as a key and never falls through. A wrong credential fails rather than getting a second chance, and the ambient cookie cannot override what the caller explicitly sent.
  - **The cookie is never accepted on `/v1`.** The gateway's credential stays the API key alone: a logged-in browser must not be able to spend through the gateway by ambient authority, and the gateway's error shape stays OpenAI's.
  - **Logout is an open route and always 200.** It revokes the session the cookie names (idempotent — twice is not an error) and clears the cookie with `Max-Age=0`. It sits outside the auth middleware precisely so that logging out twice answers 200 with no session at all.
  - **A key that exists but is not the caller's answers 404, never 403.** The member naming a key they did not create gets the same answer as a key that does not exist — and the key stays live — because the scope is part of the lookup, not a check after it. The organization boundary already works this way, so ids cannot be probed.
  - **`created_by` starts being set, and is exposed.** Keys minted through a session record the acting user; keys minted with an API key keep it null. The field is on the API response (nullable) so the rule is observable, not just a database column.
- Why:
  - **A hand-written session table rather than a token library.** The project already decided this for users and keys (docs/decisions.md, "users and login"): the table is four queries, the semantics (membership-bound, revocable, expiring) are the product's own, and a JWT would need a revocation list to support logout — at which point it is a session table with extra steps.
  - **SHA-256 for session tokens, argon2 for passwords.** The same split as API keys versus passwords, for the same reason: what is being guessed decides the hash, not which hash is "better".
  - **No session choice at login.** A `organizationId` parameter would be honest for multi-org users, but no flow creates a second membership yet, so it would be a parameter nothing can meaningfully vary today. The oldest-membership rule is recorded here so the dashboard item knows what it replaces.
- Rejected:
  - **Sliding expiry:** a second policy (idle timeout) beside the absolute one, with its own edge cases; the absolute expiry is the whole v1 story.
  - **Roles on API keys:** there is no person behind a key to assign a role to; the pre-session "any active key may manage keys" stays exactly as it was.
  - **A 403 for cross-member key access:** 404 keeps ids unprobeable, matching the organization boundary.
  - **Session auth on `/v1`:** non-goal per the issue; the chat page (TODO C-11) is what will need it, and it gets its own contract then.
  - **The Leptos login/logout pages:** `crates/web` does not exist until TODO B-5; a page written in the server crate now would be deleted there. This item delivers the endpoints those pages call.
- Implementation notes:
  - `crates/core/src/sessions.rs` holds the store, the token scheme and the `Principal`/`KeyScope` types; `crates/server/src/auth.rs` the two middlewares; `crates/server/src/routes.rs` the three endpoints and the cookie attributes; migration `0004_sessions` (the issue's `0003` was taken by #13's `0003_open_holds`).
  - `crates/core/tests/sessions.rs` and `crates/server/tests/sessions.rs` cover the completion criteria: the cookie attributes, the indistinguishable 401s, session/membership/expiry/revocation invalidation, the member/owner key rules, `createdBy` on both mint paths, the gateway refusing the cookie, and the table dump authenticating nothing.
  - `GET /api/v1/session` for a key principal is 404: a key names no session.

## 2026-10-02 Test-sealed channels are cleared with TRUNCATE, documented in the dev loop

- Status: Adopted. Implemented 2026-10-02, closing issue #19.
- Background: the gateway and admin integration tests write channels sealed with the tests' own key (`[7; 32]`) into the shared `oxsum` schema. A `cargo run -p oxsum-server` afterwards refuses to boot: `prepare` opens every stored credential at startup and names `OXSUM_SECRET_KEY` when one does not open, by design. The tests therefore leave the development database in a state the server rejects.
- Decision:
  - The dev loop documents the reset: `psql "$DATABASE_URL" -c "TRUNCATE oxsum.channel_prices, oxsum.channels;"` before `cargo run`, in docs/development.md.
  - `TRUNCATE` is what the append-only trigger leaves open: the trigger fires on row-level `UPDATE`/`DELETE`, not on `TRUNCATE`, so the reset does not weaken the "a price version cannot be rewritten or deleted" guarantee — it clears the tables, it does not rewrite history.
  - Users, organizations, keys and ledgers are untouched; only channels and their prices are cleared.
- Why:
  - Test-side cleanup is the infeasible alternative, not the smaller one: `DELETE` is blocked by the trigger plus `ON DELETE RESTRICT` (both deliberate product invariants), `TRUNCATE` in per-test teardown races the parallel tests sharing the schema, libtest has no after-all hook, and a separate test database is a bigger architectural change than the problem warrants.
  - Keeping the startup refusal (rather than teaching the server to skip unopenable channels) is the point of the check: a deployment that cannot open its credentials must fail before serving, not per request.
- Rejected:
  - Deleting test channels in teardown: blocked by the append-only trigger by design; working around it would punch a hole in a product invariant for test tidiness.
  - Sealing test channels with the dev `.env` key: makes tests depend on ambient configuration, and breaks the missing-key startup test, which needs channels the config cannot open.
  - A server flag to clear channels: a new interface for what is a documented one-liner, and a footgun on a real deployment.
- Implementation notes:
  - Verified end-to-end: a gateway test leaves a `mock-*` channel, `cargo run` refuses with "a stored upstream credential does not open with OXSUM_SECRET_KEY", the documented `TRUNCATE` clears both tables, and the server boots with `/healthz` answering ok.

## 2026-10-02 The generative suite loads `.env` before reading `DATABASE_URL`

- Status: Adopted. Implemented 2026-10-02, closing issue #18.
- Background: `tenants()` in `crates/core/tests/generative.rs` read `DATABASE_URL` from the process environment before `runtime()` loaded `.env` via dotenvy. Under the documented setup (`cp .env.example .env`, nothing exported) every one of the 1000 proptest cases hit `let Some(tenants) = tenants() else { return Ok(()); }` and the suite passed in ~0.2s, having tested nothing. The other suites (`wallet.rs`, `channels.rs`, `identity.rs`, `admin.rs`, `api.rs`, `gateway.rs`) all load `.env` first; the generative suite was the only one with the inverted order.
- Decision:
  - The suite reads the variable through a `database_url()` helper that calls `dotenvy::dotenv()` first, mirroring the `url()` helper in the other suites.
  - The no-database skip stays: docs/development.md documents that database tests skip without `DATABASE_URL`. But it is loud now — a warning naming the skipped case count, printed once, so a vacuous run cannot look like a real one.
  - A subprocess probe pins the ordering: it runs one proptest case in a child whose environment has no `DATABASE_URL` and whose working directory holds a `.env` with an unparseable URL, and asserts the child fails (it tried the database). Hermetic — no real database, no repo `.env` — and fast, since the URL fails at parse time before any I/O.
- Why:
  - Keeping the skip (rather than failing hard without a database) is what the docs promise, and a bare `cargo test` on a machine without PostgreSQL must stay green.
  - Loading `.env` in the helper rather than requiring an exported variable is what makes the documented setup honest: `cp .env.example .env` followed by `cargo test` now really tests.
  - A subprocess probe rather than a unit test: the ordering is a process-global property (environment plus working directory), so only a subprocess can pin it without racing the suite's own parallel cases.
- Rejected:
  - Making the skip a hard failure: contradicts the documented contract and would break `cargo test` on machines without a database.
  - Requiring `DATABASE_URL` to be exported: contradicts the documented setup and the other suites' behaviour.
- Implementation notes:
  - Verified both directions: the probe passes with the fix and fails with the exact assertion message when the lookup is reverted to the environment-before-`.env` order.
  - `runtime()` keeps its own `dotenvy::dotenv()` call; loading twice is harmless (dotenvy never overrides an existing variable).

## 2026-10-02 Channels and versioned prices: the database is the configuration, and a request is priced by the version it starts on

- Status: Adopted. Implemented 2026-10-02, closing TODO 3 / issue #15. Replaces one line of "A gateway turn freezes first": prices still come from a `PriceBook` behind a seam, and what fills the seam is now rows rather than the environment.
- Background: item 2 left the deployment described by `OXSUM_MODELS` and `OXSUM_UPSTREAM_*` — one channel, one price per model, read once at startup. product.md asks for more than that from the start: a price change appends a version instead of overwriting one, and the version in force when a request starts is the version that settles it. None of that fits an environment variable, which has no history. The request path already had the right shape — the price is resolved before the hold and carried by the turn — so what was missing was a source with a history, and recording which version was used.
- Decision:
  - **Channels and prices are oxsum's own tables** (`oxsum.channels`, `oxsum.channel_prices`, migration `0002_channels`), in the same schema as identity. A channel is a name, a base URL, a sealed credential, and the credential's last four characters; a price is one row per `(channel, model, version)`.
  - **Prices are append-only, and a trigger enforces it.** A change inserts `version = max + 1` under the channel's own row lock; an `UPDATE` or a `DELETE` on `oxsum.channel_prices` raises, for any client, psql included. "Changing a price never overwrites the old one" is a property of the database rather than a convention the code observes.
  - **A request resolves its channel and its version once, before the freeze**, carries them through the turn, and writes them into the settlement's description (`channel`, `priceVersion`) beside the prices and the token counts. A bill therefore says which version priced it, and that statement stays checkable after any number of later price changes.
  - **Upstream credentials are sealed with AES-256-GCM under `OXSUM_SECRET_KEY`** (32 bytes, base64), with a fresh nonce per value and only the last four characters kept in the clear. A dump of `oxsum.channels` cannot call upstream, and the key stays deployment configuration rather than data.
  - **The environment is the bootstrap, not the configuration.** At startup a database with no channels is seeded with one channel and one price version per `OXSUM_MODELS` entry; a database that has channels is left alone, whatever the environment says.
  - **The platform admin is an operator token** (`OXSUM_ADMIN_TOKEN`, at least 16 characters, compared in constant time) on `/api/v1/admin`: create or repoint a channel, append a price version, list the channels with their current versions, and read one channel's whole price history. It is not an API key and belongs to no organization — pointing the gateway at another upstream is not something an organization does to itself.
  - **A model belongs to exactly one channel, enforced where it can be**: pricing a model that another channel already serves is `CONFLICT`. That is what keeps the gateway's model lookup unambiguous — one model, one upstream — which is product.md's v1 rule rather than an implementation detail.
  - **A deployment that cannot open its channels refuses to start.** `oxsum_server::prepare` opens every stored credential once and names `OXSUM_SECRET_KEY` when it cannot, so a lost or wrong key is a startup failure instead of a 500 on a request that has already been relayed.
  - **A settlement's arithmetic does not change.** The record still carries the token counts, both prices, the charge and the freeze, plus the channel and version; nothing re-prices anything already written.
- Why:
  - **History in the table, not in a column that moves.** A price version is what a bill references, and a row that can be overwritten cannot be referenced. The trigger is eight lines of SQL and removes the whole class of "someone updated a price and an old bill no longer adds up".
  - **The version is recorded, not only the prices.** The prices alone let a bill be recomputed; the version lets it be *checked against the configuration* — a reader can ask the API for version 3 of that model and compare. That is the difference between a record and evidence.
  - **An operator token rather than a session.** Organization roles and sessions arrive with TODO 4, and the dashboard with TODO 5. What this item needed was a way for whoever deploys oxsum to change a price today, and an environment token is one a deployment can rotate without a login system. The endpoints are the ones the dashboard will drive.
  - **Encryption at rest with the key outside the database.** product.md requires stored credentials to be unreadable, and a key stored beside its ciphertext would satisfy the letter and none of the point. AES-GCM authenticates as well as it encrypts, so a record that does not open under the configured key is found at startup rather than becoming a plausible wrong credential.
  - **The environment seeds an empty database.** A first deployment still needs no HTTP call before it can serve, the tests keep describing a deployment the way an operator would, and the way off the environment is "start once, then change prices over the API".
  - **One model, one channel, checked when the price is written.** The alternative — letting two channels price one model and picking at request time — makes the answer depend on row order, which is a routing policy nobody asked for and which v1's "no load balancing, no failover" forbids.
- Rejected:
  - **A channel id in the ledger descriptions instead of a name and a version**: the description is capped at 512 characters and is meant to be read by a person verifying a bill; a uuid plus a lookup is neither readable nor stable.
  - **Keeping prices in the environment and versioning them in a file**: a file has no history either, and two deployments of one file can disagree about what is in force.
  - **Deleting or deactivating a channel or a price**: with no delete there is no way to make an old bill unverifiable; a channel that should stop serving can be repointed, or left without a current price.
  - **A platform-admin flag in `oxsum.users`**: the platform admin is whoever deploys oxsum and belongs to no organization. Modelling them as a user would put a second identity system in the same PR as the first one.
  - **Encrypting with `pgcrypto`**: the key would live in the same database as the ciphertext, and every relayed request would hand the credential to Postgres to decrypt.
- Implementation notes:
  - `crates/core/src/channels.rs` holds the store and the sealing, `crates/server/src/admin.rs` the surface and its middleware, and `crates/core/migrations/0002_channels.sql` the schema and the trigger.
  - The bootstrap is `oxsum_server::prepare`, which `main` and the integration tests both call. `app` stays a pure router builder, so a test can still build one over a pool that never connects for the requests that are answered without the database.
  - `crates/core/tests/channels.rs` covers version allocation, the trigger refusing an update and a delete, the one-channel-per-model conflict, credential sealing (including a record tampered with in the database), and the bootstrap's "seed an empty database, leave a populated one alone".
  - `crates/server/tests/admin.rs` covers the surface: create, repoint, append, list, history, both token refusals, and the input refusals in oxsum's error shape. `crates/server/tests/gateway.rs` covers the promise itself: a price change while a streamed turn is in flight, the in-flight turn settling at version 1 and the next one at version 2.
  - The gateway tests share one database and a model belongs to one channel, so every test world names its channel and its models with a random suffix. Without that, two worlds pricing `ok` would conflict, which is the rule working rather than a test problem.
  - `OXSUM_SECRET_KEY` and `OXSUM_ADMIN_TOKEN` are documented in `.env.example` and `docs/development.md`. The first is required as soon as a channel exists, including one seeded from the environment — which is why a deployment that sets `OXSUM_MODELS` must set it too.

## 2026-10-02 A gateway turn freezes first, relays through a generator, and settles however it ends

- Status: Adopted. Implemented 2026-10-02, closing TODO 2 / issue #12. Carries out the shape "Gateway HTTP client and token estimation" chose, and corrects two of its lines: what the disconnect signal is, and how the stream is relayed.
- Background: the gateway is the component that owns both halves of a hold/settle pair (issue #6 named it as such), so it is where product.md's billing table becomes real: freeze an upper bound before the call, settle against upstream's usage afterwards, and account for every way a turn can end. Three of those endings are not "upstream answered normally": upstream refuses, upstream is unreachable, and the client leaves mid-stream. The last one is the interesting one, because the code that would settle it is the code the client's departure destroys.
- Decision:
  - **The order is the promise.** The hold is taken before upstream is contacted, upstream's answer decides the charge, and the settlement is the last thing that happens to a turn. A refusal at the hold is answered 402 before any network call, and the message states the freeze, the balance, and that `max_tokens` lowers the price — a payment error that does not name its price is a support ticket.
  - **Prices come from a `PriceBook` behind a seam.** `OXSUM_MODELS` describes the channel in the environment for now; TODO 3 replaces the source with versioned rows managed from the admin dashboard, which is a change of what fills the book rather than a change of the request path.
  - **All money arithmetic lives in `crates/core/src/billing.rs`**: the input upper bound (UTF-8 bytes plus a fixed per-message overhead), the freeze, the priced usage, the local estimate, and the settlement record. The server decides *when* to price; core decides *what it costs*. The freeze rounds up, so a fraction of a minor unit is never charged at zero.
  - **The settlement kinds are product.md's table, and which one applies is decided by where the turn ended, not by configuration**: `usage` (upstream reported usage), `estimated` (it reported none), `client_cancelled` (the client left mid-stream), `upstream_error` and `upstream_unreachable` (nothing was received, so nothing is charged), `capped` (upstream's usage priced above the freeze — the freeze is charged and the excess is an anomaly for the admin page rather than a silent platform loss).
  - **The relay is an `async-stream` generator over `reqwest::Response::chunk()`**, and it awaits the settlement *after* the last forwarded chunk and before the stream ends, so a client that reads a stream to its end reads a settled bill. This corrects the earlier entry's `bytes_stream()`: `chunk()` is reqwest's own accessor, and the generator is what lets the settlement be awaited at the end of a stream at all — a hand-written `Stream` has nowhere to await.
  - **The turn owns its own settlement, and a begun settlement cannot be cancelled.** hyper drops the response body when the client goes away, which drops the generator, which drops the upstream response — reqwest's only cancel. The write itself runs in a task of its own and is awaited, so the client's departure cannot cancel an append that has started. What a departure can do is end the turn before the settlement starts, and then `Drop` settles from what had been forwarded, spawned onto the runtime because a destructor cannot await. A client that leaves after upstream has already ended has not cut the turn short: that turn is billed as the finished turn it is, from upstream's own counts, as `usage`. This corrects the earlier entry's "axum's connection notification and a `tokio::select!`": the body is the notification, and there is no second signal to keep in sync with it.
  - **The scanner reads the SSE stream for two things only**: the `usage` object upstream's final chunk carries (the stream is asked for it with `stream_options.include_usage`), and the answer text of the deltas, capped at 256 KiB, for the estimate when usage never arrives. Frames split across chunks are reassembled by holding the trailing partial line.
  - **`/v1/*` answers in OpenAI's error shape**, with oxsum's own code in `error.code`, and every response carries `x-oxsum-request-id`. The two ledger entries are `req-<id>:hold` and `req-<id>:settle`, derived by the public `oxsum_core::entry_id_for`, so a caller holding a request id can name the entries it caused without asking the server which ones they were.
  - **`/v1/models` lists exactly the models with a price**, and a request for any other model is refused 400. A model the gateway cannot price cannot be frozen, so it is not served at all.
- Why:
  - **A generator, because the settlement is the last step of a stream.** The three alternatives all move the ledger write off the request: settling in a spawned task (the client's stream closes before its bill exists, and a test has to poll), settling in a `Drop` (a destructor cannot await, so the write races the process), or settling before the last chunk is forwarded (it would charge for output it had not sent). Awaiting inside the generator is the only shape where the stream's end *is* the settlement, and the `Drop` path remains for the case the generator never reaches its end.
  - **The body as the disconnect signal.** Cancelling the upstream call is the whole point (product.md: a disconnect cancels the call and settles on estimation), and dropping the response is how reqwest cancels. Anything else — a watchdog, a `select!` on a connection notification — is a second thing that has to be right about the same event.
  - **A spawn inside the turn, because a destructor cannot retry what a cancelled write leaves behind.** The first version settled inline and gave the plan up as the write began. An OpenAI SDK client closes the connection the moment it reads the terminator, which over a real socket arrives while the append is in flight, so the append was cancelled with the plan already spent: no entry, and a freeze reserved until the sweeper. Settling through a task that is still awaited keeps the property the relay was built for — the stream's end is the settlement — and makes a begun write uncancellable. The `Drop` path cannot retry it instead: the cancelled append still holds its transaction's locks, so the retry waits behind it.
  - **A disconnect after upstream ended is not a cancelled turn.** Billing it as `client_cancelled` would put the mainstream SDK's every streamed call in the fallback column and hide a real cancellation among them. The turn records that upstream has ended, so the two are distinguishable without a second signal, and the estimate is only what a turn cut short is charged from.
  - **Freeze before, never after.** A reservation taken after the call would be a claim on a balance that may already be gone, and the whole product promise is that credit is committed before it is spent.
  - **Capped as its own kind, not a bigger estimate.** A usage report above the freeze means the output bound did not hold upstream, which is a bug somewhere, and an anomaly the operator needs to see rather than a charge nobody can explain. An *estimate* above the freeze is not that: it is the platform under-measuring, and it keeps its own kind.
  - **No retries, no second upstream attempt.** product.md forbids retrying a streamed call that has already billed; a retry would need the idempotency of a turn, which is TODO 5's `requests` table, not this loop.
- Rejected:
  - **A `requests` table in this change**: it is TODO 5, and the ledger already records both halves of a turn. Adding it here would put two sources of truth for "is this turn in flight" in one PR.
  - **A hold sweeper in this change**: issue #13. Without the `requests` table there is no way to tell a turn that is still running from one whose process died, so a sweeper would have to guess.
  - **Price channels and versioning**: TODO 3. One channel and one price per model is what a deployment needs to bill honestly today, and the `PriceBook` seam is where a second channel attaches.
  - **Passing upstream's error status through unchanged**: a refusal from the provider is not the caller's fault and not evidence that oxsum is broken, so it answers 502 with upstream's own error object, which the SDK shows verbatim.
  - **Tokenizing the input to compute the freeze**: already rejected in product.md; a tokenizer that undercounts breaks the freeze, and byte length cannot.
  - **Holding the whole answer in memory to price it exactly on the cancel path**: the estimate window bounds what a stalled stream can cost the server, and an undercount is the platform's cost rather than the user's.
- Implementation notes:
  - `crates/server/tests/gateway.rs` runs a second axum server as a scripted upstream on a free port, selected by model name, so refusals, a non-JSON 200, a stream without its terminator, and a stream that stalls forever are all ordinary test cases. Seventeen tests cover the loop: usage charging, the estimate fallback, the cap, both upstream failure kinds, the 402 that never touches the network, the image refusal, the unknown model, the disconnect that settles off the request path, the client that hangs up at the terminator and is still billed from upstream's counts, and two concurrent turns that cannot overdraw one wallet. The disconnect tests run the gateway on a real socket, because the failure they cover only happens when a client hangs up for real.
  - The loop was also driven by the official OpenAI Python SDK (3.23) against a mock provider: `models.list()`, one plain completion and one streamed completion, both billed from the ledger with upstream's token counts. The streamed call is what exposed the cancelled append; the integration test above is that case, kept.
  - A settlement that fails after the response has already been sent is logged and left: the hold stays outstanding for the sweeper (issue #13) instead of turning into a 500 after upstream has answered. Its two reachable refusals — a release beyond the reservation and a reused key — cannot happen for a turn with a freshly minted request id, which is why the gateway mints one per request.
  - Graceful shutdown drops in-flight bodies, so a turn that was mid-stream at shutdown settles as `client_cancelled` even though the client was still there. It is the same path, and the alternative is a shutdown that waits for every stream.
  - The three environment variables that describe the channel are documented in `.env.example` and `docs/development.md`; a deployment that sets none of them serves the wallet and answers `/v1/models` with an empty list.

## 2026-10-02 A reservation may not be released beyond what is reserved: `BalanceLimit::FundedReservations`

- Status: Adopted. Implemented 2026-10-02, closing issue #6. Found by the generative harness, which is where the hole came from.
- Background: `Wallet::settle` releases the `held` the caller hands it and never looks the hold up, which is fine as long as the ledger refuses a release nothing covers. It did not. The wallet carried `NoDebitBalance`, which folds the two layers asymmetrically — a reservation consumes room, a pending credit grants none — but a pending credit also *costs* none, so releasing a reservation that was never made raised the available balance by exactly that amount: free, spendable credit from a credit entry. The generator's first 1000-case run reported it in minutes (a settlement of 1 credit against a wallet that had never held anything).
- Decision:
  - The invariant is stated in the engine as a fourth `BalanceLimit`: **`FundedReservations`** is `NoDebitBalance` **and** "the pending layer may not carry a credit of its own", so a release can only give back room a reservation first took. oxsum's wallet account carries it.
  - It is enforced where every other limit is: inside the append transaction, against the balance the entry would leave behind, with the constrained account's row locked — so a limit checked before the write cannot be raced.
  - The rule is **aggregate**: it bounds the total released by the total reserved, not one settlement by one hold. The gap that leaves is issue #10.
  - The refusal is `INSUFFICIENT_FUNDS`, 402, like an unfunded hold. The contract changed first (`crates/server/openapi.yaml`), then `docs/api.md`, `docs/user-guide.md` and `docs/architecture.md`.
  - `Wallet::open` applies the limit on **every** open and upserts it, so a ledger written under the older rule is tightened rather than keeping the weaker rule for the rest of its life.
  - The generative harness dropped the restriction it was written with: it now settles any earlier op's amount, and its model predicts settlement refusals from the reserved total, read through the new `Wallet::settled()` / `Wallet::reserved()` accessors.
- Why:
  - **The engine, because that is where the check can be atomic.** Both alternatives in the issue leave it outside the append. A `holds` table in oxsum's schema duplicates state the ledger already keeps — the pending layer — so it can drift from the books, and it is only race-free if it is written in the same transaction as the append, which means the same lock, in a second place. Reading the pending layer in `Wallet::settle` is a read-then-write: two concurrent settlements both see the reservation and both release it.
  - **One variant, not a second limit column.** An account carries one limit, and this is the rule a funded reservation account needs; two of the three existing variants would have to be set on the wallet to get both halves. A per-layer column would also change the account record's canonical encoding, which would invalidate the content hash of every stored account, for a rule only this account uses.
  - **The headroom is the tighter of the two rules.** They constrain opposite directions — a reservation is bounded by the funds on hand, a release by the reservations outstanding — so the *sign* is what the check needs, and the magnitude is the binding one. `headroom_minor` documents this.
  - **The change is marked where it lands** (`oxsum change (not upstream)` at the variant, the headroom arm, both store code mappings, the DDL and the README), and the engine's own conformance suite gained a check, so the in-memory journal, SQLite and PostgreSQL all prove the rule rather than only the one oxsum runs.
- Rejected:
  - An oxsum-side `holds` table: the same fact kept twice, with a lock and a transaction to keep it honest, and a second thing that can be wrong.
  - Reading the pending layer in `Wallet::settle` before appending: a read-then-write races two settlements into both releasing one reservation. This was the mechanism the issue called "reading the pending layer inside the append transaction" — it is the right shape, but doing it from the domain layer means adding a hook to the engine's append, and a hook is a bigger engine change than a limit the engine already knows how to enforce.
  - Redefining `NoDebitBalance` to include it: it would change behaviour for accounts that use the variant today, and its asymmetric fold is documented and tested as it is.
  - Making a settlement name the hold's idempotency key: it pairs a settlement with exactly one hold, but it changes the request contract and needs per-hold release state the ledger does not keep. That is issue #10, and the gateway (TODO 2) is the component that owns both halves of a hold/settle pair.
  - A database `CHECK` on the pending layer: the layer's total is the sum of postings, not a column, and the rule has to hold against the balance the entry would leave behind.
- Implementation notes:
  - **What the rule does not do** is pair a settlement with the hold it names: releasing more than the named hold is accepted while other holds cover the total. No value can be fabricated that way — the total released still cannot exceed the total reserved — but the pairing is loose, which issue #10 tracks and which `docs/api.md` and the `settle` doc comment state plainly rather than hiding.
  - The generative model was rewritten to the aggregate: it tracks `settled` and `reserved` absolutely, read from the ledger at the start of a case, instead of per-hold amounts and deltas. That the new path is actually exercised is not assumed: with the refusal prediction removed, 60 cases fail within two seconds on a settlement of an amount that was never held.
  - Deploying over an existing database needs the constraint widened, and `CREATE TABLE IF NOT EXISTS` does not rewrite one, so the PostgreSQL DDL drops and re-adds `accounts_balance_limit` (idempotent, and it runs inside the migration's transaction with the schema pinned). Verified against a ledger in the dev database that predates the variant: the widened constraint accepts `funded_reservations` and still refuses an unknown code. SQLite has no `ALTER TABLE ... DROP CONSTRAINT`, so an existing SQLite file keeps the old constraint — noted in its schema, and oxsum uses PostgreSQL only.
  - `Wallet::open` sets the limit on every open, so an existing ledger is upgraded on next use; a test weakens a ledger's stored limit to `no_debit` and asserts that opening it restores `funded_reservations` and refuses an unheld settlement again.

## 2026-10-02 A reused idempotency key is a conflict, mapped by the domain layer from the engine's refusal

- Status: Adopted. Implemented 2026-10-02, closing issue #7. Corrects one line of the generative-test entry below, which left the mapping to this issue.
- Background: the ledger refuses a key that an entry with different content already holds (`PostgresError::IdempotencyConflict`). The domain layer's `From<PostgresError>` mapped only `LimitBreached`, so this one fell into `Storage`, which the API deliberately answers as 500 `INTERNAL_ERROR` with the details kept in the logs. A caller that reused a key by mistake therefore got a server error, for a request it could fix itself — and every such mistake was logged as an incident.
- Decision:
  - `PostgresError::IdempotencyConflict` maps to `WalletError::Conflict`, which the HTTP layer answers 409 `CONFLICT`.
  - The refusal classes each pick their own code: a breach of a balance limit is `INSUFFICIENT_FUNDS` (402), a reused key is `CONFLICT` (409), a bad argument is `VALIDATION_ERROR` (400). Everything else stays `Storage` and 500, which is the honest answer for a ledger that cannot be read or written.
  - The message names the cause and not the ledger's internals: "idempotency key already used for a different request". The existing entry's id is not echoed back, and the caller does not need it — the same key with the original content still replays successfully.
- Why:
  - The distinction being made is *whose mistake it is*, and that is what decides the code: a caller who reused a key gets a 4xx it can act on, while a failing ledger gets a 500 that pages someone. Collapsing them makes both worse — the caller retries a request that will never succeed, and the logs fill with incidents that are not incidents.
  - Mapping it in the domain layer rather than in the route layer keeps the HTTP layer's job what it is: `WalletError` in, code out. The engine's error type does not appear above `oxsum_core`.
- Rejected:
  - Answering 400 `VALIDATION_ERROR`: the request is well formed and the key is valid; it is the *combination* of key and content that conflicts, which is what 409 means.
  - Answering 422 or a new code: no code in docs/api.md fits better than `CONFLICT`, and a code used once is a code every client has to special-case.
  - Checking the key against stored entries before appending: a read-then-write races, two concurrent requests with the same key could both pass, and the engine already makes the check atomic inside the append.
- Implementation notes:
  - `crates/core/tests/wallet.rs` asserts the refusal is `Conflict`, that the refused attempts leave the balance, the log and the key's original entry untouched, and that a top-up and a hold under one key collide with each other. `crates/server/tests/api.rs` asserts 409, `CONFLICT`, a message that does not mention storage, and that the original request still replays with `isNew: false`.
  - The generative model (`crates/core/tests/generative.rs`) reads a reused key as a refusal class rather than a `WalletError` variant, so it needed only its stale comment removed: the arm that accepted a storage-shaped refusal was deleted, which makes the oracle fail if the mapping ever regresses.

## 2026-10-02 Generative wallet sequences: proptest against an in-memory model, on eight shared ledgers

- Status: Adopted. Implemented 2026-10-02 (`crates/core/tests/generative.rs`), closing TODO 1 / issue #5.
- Background: the unit and integration tests cover the cases somebody thought of, while the wallet's promises are about *sequences*: a hold settled twice, a top-up replayed after a failure, a key reused with different content, a hold that runs into the balance after an earlier settlement released part of it. The project already has this style of test in the engine (`crates/doubleentry/tests/simulation.rs` generates seeded sequences and checks the engine's invariants after every step), so the tool and the shape were not up for debate: proptest, an explicit operation enum, a model, assertions after each step.
- Decision:
  - `Vec<Raw>` of `TopUp`, `Hold`, `Settle { back, mode }`, `Replay { back }`, `Collide { back, up }` is generated, then resolved **positionally** into ops, so a back-reference can only ever name an earlier op and every generated sequence is well formed. The first op has no earlier op, so a back-reference there becomes the write it would otherwise have referred to.
  - The oracle is the wallet account's two layers (`settled`, `pending`) plus the map of keys that have written an entry. After every step it asserts the outcome class the model predicted (writes / replays / conflict / insufficient funds / invalid input), `available() == settled + pending`, `available() >= 0`, and the log size. At the end of a case it asserts a proof for every entry the case wrote, and that the proof stops verifying when the amount it records changes.
  - The 1000 cases **share eight ledgers** (`generative_0` … `generative_7`), dropped and recreated once per run, and each case's keys carry its own number. A case reads its tenant's balance and log size at the start and works in deltas from there.
  - `PROPTEST_CASES` sets the case count; the default is 1000, which is what TODO asks CI for.
- Why:
  - Sharing ledgers is what makes 1000 cases affordable and repeatable. Creating a ledger is a DDL migration (an extension plus eleven tables); one per case would spend the whole run on schema creation and leave a thousand schemas behind. The delta is not a weaker assertion: the state at the start of a case cancels out of both sides of it, and the invariants (never negative, log size, proofs, idempotency) do not depend on where the case began.
  - The oracle comes from the *intended* contract, not from what the implementation happens to do, which is the only way such a test can find anything. Where the implementation deviates the deviation is named in the module docs and left to its issue, rather than written into the oracle as if it were correct: at the time it was written the generator settled only holds the ledger actually took, a restriction issue #6 lifted once the rule it needs existed.
  - Keys are derived from the op index rather than generated as random strings, because the interesting operations reference earlier ones by name; a per-case prefix keeps two cases on one ledger from colliding in the ledger's idempotency space.
- Rejected:
  - One ledger per case: the DDL cost above, and a test database that grows by a thousand schemas per run.
  - Asserting on `WalletError` variants for the reused-key case: when this harness was written the engine's refusal reached the domain layer wrapped as a storage failure, and pinning that shape would have frozen a bug into the oracle. The model asserts the class instead — refused, nothing changed — so mapping the refusal to `CONFLICT` (issue #7) did not rewrite it.
  - A tamper check that flips a byte: it can land in a field name, and the verifier deliberately ignores fields it does not know so that a newer server can add one without breaking browsers that already shipped. The tamper check moves an amount instead, which the content hash covers.
- Implementation notes:
  - The generator found two things while it was being written, both filed rather than papered over: a settlement of an amount that was never held was accepted and fabricated available balance (issue #6, fixed by the `FundedReservations` entry above), and a key reused with different content reached the API as a 500 rather than a 409 (issue #7, since fixed: a reused key is mapped to `CONFLICT` and answers 409).
  - The first version shared ledgers without a per-case key prefix and failed immediately: case two's `op-0` collided with case one's entry. The seed, the shrunken input and the message came out of proptest and made it a two-minute fix.
  - A settlement cannot fail for lack of funds, which the oracle relied on until issue #6 changed the rule: a settlement charges `actual` out of settled money and releases `held >= actual` of reservation, so the available balance it leaves is at least the one it found. That is still true, and it is now the reason the only two refusals a settlement has are an argument outside `0..=held` and a release beyond the reserved total — see the `FundedReservations` entry above for the second one.

## 2026-10-02 oxsum's own tables live in one `oxsum` schema, and an organization's ledger is created on first use

- Status: Adopted. Implemented 2026-10-02. Refines "a tenant is an organization" (which already put these tables outside the ledger schemas) and corrects one line of "users and login": registration no longer creates the ledger inside its transaction.
- Background: the user, organization, membership and API key tables needed a home. The ledger tables already have a mechanism — one schema per organization, targeted by `SET LOCAL search_path` at the start of every transaction (see "all tenants share one connection pool") — which is exactly what oxsum's own tables must *not* share: `users` is global, an email is unique across the deployment, and a key resolves the organization rather than being scoped by it.
- Decision:
  - `users`, `organizations`, `memberships` and `api_keys` live in the single `oxsum` schema, created and versioned by oxsum's own runner: `Db::migrate` creates the schema, then applies the files under `crates/core/migrations/` that have no row in `oxsum._migrations`, each migration in one transaction together with that row, under a database-wide advisory lock.
  - Every statement names the schema (`INSERT INTO oxsum.users …`). There is no second `search_path` pinning mechanism, and identity statements are not wrapped in the ledger's transaction-scoped pin.
  - Registration writes user, personal organization, owner membership and first API key in one transaction. It does **not** create the ledger: `Tenants::get` creates `ledger_<tenant_id>` on first use, idempotently, inside the ledger's own advisory lock.
  - `tenant_id` is the organization's UUID without dashes — 32 lowercase hex characters, which the ledger's existing tenant-id rule accepts unchanged, so nothing has to be chosen, probed for uniqueness or sanitized.
  - API keys: `oxs-` plus 32 random bytes, hex-encoded. The database stores a SHA-256 hash (unique, so a lookup is one index probe) plus the first 12 characters as a display prefix, and never the plaintext, which is returned once at creation.
- Why:
  - One schema for identity, schema-qualified, is the smallest thing that works: the tables are few and always the same, and `oxsum.` in the SQL is explicit and greppable, unlike a pin that has to be issued correctly on every path. It also keeps one place to migrate, and no per-organization identity DDL.
  - Cross-organization isolation is unaffected: balances, entries and proofs stay in per-organization ledger schemas, and identity rows are reached by the key lookup that produced the organization in the first place.
  - Lazy ledger creation keeps registration inside tables that exist before any organization does. A ledger migration is a large DDL (an extension plus eleven tables); running it inside the registration transaction holds that transaction open and makes a retry depend on the ledger migration being idempotent. Created on first use, it is idempotent by construction, and a user who never tops up never costs a schema.
  - SHA-256 for API keys, argon2 for passwords: a key is 256 bits of machine randomness, so there is nothing to brute-force, while every authenticated request pays for the lookup; a password is chosen by a human and short, so it gets the slow hash. Both choices are about what is being guessed, not about which hash is "better".
- Rejected:
  - Identity tables inside each ledger schema: N copies of `users`, and cross-organization email uniqueness becomes impossible.
  - A separate identity database: a second pool and a two-phase commit to join what is one foreign key today.
  - Pinning `search_path` per identity transaction as well: a second mechanism to get right, for tables that never vary by organization.
  - Plaintext or argon2-hashed API keys: plaintext leaks on any dump; argon2 costs ~100 ms per API request for no gain over SHA-256 on a 256-bit random secret.
  - Letting the caller pass a tenant id or organization slug in the path: the credential already names the organization, and a path segment the caller controls is one more thing to authorize. This is why the ledger endpoints lost `/tenants/{tenant}`.
- Implementation notes:
  - The migration runner clears `search_path` for the transaction it applies a migration in. The first version of `0001_identity.sql` used unqualified names and created its tables in `public` while recording the migration as applied — found by the suite, not by reading. With `search_path` empty, that mistake fails loudly instead, and a test asserts nothing named `users`/`organizations`/`memberships`/`api_keys` exists in `public`.
  - A duplicate email surfaces as 409 `CONFLICT` by mapping the unique-constraint violation of `users_email_normalized_key`, not by checking first: two simultaneous registrations cannot both pass a check, and the loser would otherwise be a 500.
  - Every way a key can fail — missing header, malformed, unknown, revoked, expired — answers the same 401 `UNAUTHORIZED` with the same message, so key state cannot be probed. A key id belonging to another organization answers 404, not 403, for the same reason.
  - `Db::migrate` unlocks the advisory lock before returning the migration's own result, so a failed migration hands its connection back to the pool unlocked.
  - Signup mode is deployment configuration, not domain state: `Config::from_env` reads `OXSUM_SIGNUP` (default `invite`) and the route refuses registration with 403 `FORBIDDEN` when it is not `open`.

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
  - The user/org layer is tightly coupled to the ledger: registration creates user, personal organization and owner membership in one transaction (the ledger itself is created on first use, see "oxsum's own tables live in one `oxsum` schema" above; the tenant model is "a tenant is an organization"); removing a member has to deal with their keys. Off-the-shelf user libraries know nothing about "a ledger hanging under an organization".
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

## 2026-10-03 Settlement-hold pairing: contract change, not a holds table (issue #10)

- Status: Adopted
- Decision: a settlement names the hold it releases (`holdKey`); the server reads the hold's amount from the hold entry in the ledger, so the request carries no amount to assert. One hold settles at most once: the settlement entry's idempotency key is derived from the hold's key (`oxsum_core::settlement_key_for`), and the ledger's idempotency gate refuses a second, different settlement of the same hold with `Conflict` ("hold already settled"), inside the append. Retrying the identical settlement replays it. Naming a hold that is not outstanding is `WalletError::HoldNotFound`, answered 404; the gateway maps it to 500 because it settles the hold it just took, so a missing one is an internal inconsistency.
- Why: the gateway already owns both halves of a turn — it takes the hold under `req-<id>:hold` and settles under a key it chose — so asking the client to re-supply the hold key costs nothing and removes the aggregate hole: a settlement can no longer release more than its own hold reserved while other holds cover the total. The ledger stays the source of truth (no `oxsum.holds` table, no second store to keep consistent), and the pairing rides the existing idempotency machinery instead of a new lock.
- Rejected:
  - An `oxsum.holds` table: a second source of truth for what the ledger already records; every write would need the table and the ledger to agree.
  - A pre-check for "already settled" before the append: racy by construction, and unnecessary — the derived-key collision in the append is the check, and it is atomic.
- Kept as backstop: the `funded_reservations` limit still refuses a pending credit the outstanding reservations cannot cover, behind the pairing.

## 2026-10-03 A small watch table for the hold sweeper (issue #13)

- Status: Adopted
- Decision: the sweeper finds stale holds in `oxsum.open_holds`, one row per unsettled gateway hold (hold key, tenant, request, model, channel, price version, prices, freeze, opened at). The gateway writes the row *before* taking the hold and deletes it when the turn settles; the background job settles rows older than `OXSUM_HOLD_TIMEOUT` at 0 with kind `swept` and deletes the row. The timeout defaults to 30 minutes, is validated at startup (parseable, at least 60 seconds), and must exceed the longest possible single request.
- Why: product.md's `requests` table (item 5) does not exist yet, so it cannot be the source. The ledger's pending layer says how much is held but not by which request or since when, and paging every tenant's log on every pass would cost O(the log) per tenant with no index to aim at — the issue already judged that scan too expensive, and the store's only enumeration is a full-log `page()`. The table is a finding aid only, which is what keeps #10's decision intact: the ledger stays the source of truth for the hold's amount (read from the hold entry, re-read inside the settle) and for whether it is settled, and the derived settlement key (`oxsum_core::settlement_key_for`) is the atomic guard — the sweeper and a late settlement name the same entry, so exactly one of them takes effect and the other sees `Conflict("hold already settled")`. A row whose hold is gone (the process died between noting the row and taking the hold) is deleted without a ledger write, so a disagreement always resolves toward the ledger. "A hold that is old but whose request is still streaming is not swept" is handled by the timeout contract rather than liveness tracking: anything older than the timeout is abandoned by definition, and the gateway already caps the output side of every turn by writing the output upper bound into the relayed request.
- Rejected:
  - Waiting for the `requests` table: would drag this issue into item 5's (Leptos dashboard) scope; the watch list is superseded by that table when it arrives.
  - A ledger scan per tenant: O(log) per tenant per sweep pass, opening every tenant's ledger, no index.
  - A per-chunk heartbeat for liveness: a write per chunk for a case the timeout contract already excludes.

## 2026-10-03 Demo materials: a self-driving Python script, an honestly-manual GIF (issue #35)

- Status: Adopted
- Decision: `demo/demo.py` (stdlib plus the official `openai` SDK) starts everything it needs: a scripted mock upstream in-process — mirroring `crates/server/tests/gateway.rs`' upstream (one scripted model, SSE answer frames, the token usage in the final chunk) — and the oxsum server on a free port with bootstrap env (`OXSUM_SIGNUP=open`, the mock as `OXSUM_UPSTREAM_BASE_URL`, one demo model with fixed prices, a fresh `OXSUM_SECRET_KEY`). It registers a fresh demo user (a fresh org per run, so a run never touches real data), tops up a fixed amount, runs one streaming chat turn through the SDK with `base_url` pointed at oxsum, and prints the balance delta, the turn's settlement entry — named from the `x-oxsum-request-id` header via the UUIDv5 derivations `docs/api.md` documents as caller-computable (`req-<id>:hold`, `oxsum_core::settlement_key_for`, `entry_id_for`) — with its billing record, and both proof bundles. No real provider, no API key; no new Rust code and no new HTTP endpoints: the demo uses only what the contract already promises.
- Why Python: the demo's point is the OpenAI SDK path a real user takes, and the SDK's reference implementation is Python — a demo in any other language would demo a different client. The transcript is deterministic (fixed prices, fixed scripted usage, fixed top-up); only the demo email and the API key vary per run.
- The GIF is deliberately not faked: this environment cannot record one, so the script prints a deterministic terminal-friendly transcript and the recording (the terminal running `python3 demo/demo.py`) is a manual follow-up for the repo owner, noted in the issue and the PR. No placeholder GIF is committed.
- Rejected:
  - Driving the demo against a real provider: needs a real key, is non-deterministic, and costs money — the scripted upstream answers the same wire format.
  - A new `/api/v1/entries` listing endpoint for the demo: the request-id-to-entry derivation is already public interface; adding a listing endpoint would widen the contract for a demo's convenience.

## 2026-10-03 Release: Dockerfile and GitHub Actions CI (issue #37)

- Status: Adopted
- Decision: the release is a multi-stage `Dockerfile` plus a `.github/workflows/ci.yml` workflow. The builder is `rust:1.98-bookworm` (the tag verified to exist; matches `rust-toolchain.toml`), installing `cargo-leptos` 0.3.11 (the version already used for the dashboard; verified not yanked) and the `wasm32-unknown-unknown` target, then `cargo leptos build --release` — the server binary lands in `target/release/oxsum`, the site (WASM bundle, CSS, `index.html`) in `target/site`. The runtime is `debian:bookworm-slim` plus `ca-certificates` only: reqwest uses rustls, so no OpenSSL is needed. It carries the binary and the site, sets `OXSUM_ADDR=0.0.0.0:3000` and `LEPTOS_SITE_ROOT=/app/site`, and runs against an external PostgreSQL through `DATABASE_URL` supplied at `docker run`; the schema migrates at startup, so there is no migrate step. CI runs on every PR and push to `main`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets` with `-D warnings`, and `cargo test --workspace` against a `postgres:17-alpine` service container with `DATABASE_URL` set — followed by doubleentry's testcontainers Postgres conformance suite (`cargo test -p doubleentry --features postgres --test postgres`). The toolchain comes from `dtolnay/rust-toolchain@stable` with `toolchain: "1.98"` and the `rustfmt, clippy` components, matching the pinned `rust-toolchain.toml`.
- Why: the dashboard's style is plain CSS (no tailwind, no npm), so the Docker build needs only cargo-leptos and the wasm target — no Node.js. The doubleentry conformance suite is included in CI because GitHub-hosted runners have Docker while developer machines may not; CI is the one place the full gate is green. The tree uses no `sqlx::query!` macros, so compilation needs no live database and no `SQLX_OFFLINE`. `.dockerignore` excludes `.env` so secrets can never be baked into the image.
- Rejected:
  - Bundling PostgreSQL into the image: the deployment contract is one binary plus an external database (AGENTS.md), and compose already covers local development.
  - Skipping the testcontainers suite in CI: it is doubleentry's own Postgres conformance gate; leaving it out would make CI weaker than the documented release bar, and the runners provide Docker.
  - A non-root user or distroless base: kept out to keep the image minimal; the surface is one binary behind the operator's own TLS.
- Found while verifying the image: the server never served `/pkg/*` — the shell referenced assets that answered 404, so the pages rendered but never hydrated, in the Docker image and in local development alike. `web::mount` now serves the site pkg dir through `leptos_axum`'s `site_pkg_dir_service` (a targeted route, not a fallback, so API 404s are unchanged), covered by `the_pkg_bundle_is_served_from_the_site_root` in `crates/server/tests/dashboard.rs`.

## 2026-10-03 Browser hydration: the wasm entry point (issue #46)

- Status: Adopted
- Decision: `crates/web` exports exactly one browser entry point, `hydrate()`, under `#[cfg(feature = "hydrate")]`, calling `leptos::mount::hydrate_body(App)`. The name and the shape are leptos's own — `leptos_meta`'s generated module script awaits the module's default initialiser and then calls `mod.hydrate()` — so the export is the contract between the shell and the bundle, not a name this project is free to choose. The login form also carries `method="post"`, so a browser that never hydrates posts instead of putting the credentials in the URL.
- Why: the export was missing, and wasm-bindgen only exports `#[wasm_bindgen]` items, so the bundle had no `hydrate` at all. Every page loaded the wasm, threw `TypeError: mod.hydrate is not a function` and stayed as inert as the server had rendered it: no login handler, no top-up, no chat, no billing socket, no proof check. The 2026-10-03 release entry above saw the symptom through the image and fixed the pkg *route*; the site root (issue #44) and this entry point were the two remaining reasons no browser had ever run the app — and, like the site root, no router-level test can see either, because they pin what the server answers rather than what the browser does with it. The gate that does run the wasm is C-11's manual walkthrough.
- Rejected:
  - A hand-written JS bootstrap in the shell (import the wasm ourselves and call an entry point of our own naming): it replaces leptos's generated script, which is one more thing to keep in step with the framework, and buys nothing.
  - `console_error_panic_hook`: not a dependency today and not needed by this fix; a panic still surfaces as a failed wasm call.
