# Development guide

## Requirements

- Rust 1.98: `rust-toolchain.toml` selects it automatically; rustup installs it on demand
- Docker: runs local PostgreSQL 17, also used by doubleentry's Postgres tests
- `wasm32-unknown-unknown` target: needed by the verification page and the Leptos browser build. `rustup target add wasm32-unknown-unknown --toolchain 1.98-x86_64-pc-windows-msvc` (or the platform equivalent). doubleentry is verified to compile for this target; uuid's randomness source is handled by a target dependency in `crates/doubleentry/Cargo.toml`
- `cargo-leptos`: builds the dashboard's server and WASM sides together (`cargo leptos build`, `cargo leptos serve`). Install once with `cargo install cargo-leptos`. Plain `cargo build` / `cargo test` do not need it: the `oxsum` binary serves the pages with server-side rendering either way, and only hydration (the live browser side) needs the WASM build

On Windows with Git Bash, Git's own `/usr/bin/link` shadows MSVC's `link.exe` and linking fails with `link: missing operand`. Either build inside a VS Developer shell, or export the `vcvars64.bat` environment into the current shell first.

## First setup

```bash
cp .env.example .env
docker compose up -d
cargo build --workspace
```

### Test residue in the development database

The server integration tests (`crates/server/tests/gateway.rs` and `crates/server/tests/admin.rs`) write channels sealed with the tests' own key into the shared `oxsum` schema. Afterwards `cargo run -p oxsum-server` refuses to boot: a deployment that cannot open a stored credential fails at startup by design, instead of failing a request mid-relay. Clear the test-sealed rows before starting the server:

```bash
psql "$DATABASE_URL" -c "TRUNCATE oxsum.channel_prices, oxsum.channels;"
```

(`DATABASE_URL` comes from `.env` or the shell, as for the tests.) `TRUNCATE` is what the append-only trigger on `oxsum.channel_prices` leaves open: the trigger refuses row rewrites, not a table reset, so this does not weaken the price-history guarantee. Users, organizations, keys and ledgers are untouched; only channels and their prices are cleared. On a database with real channel configuration, dump it first — this is a development-database reset.

## Branch naming

Each branch contains one issue or PR and uses lowercase ASCII in this form:
`<type>/<kebab-case-description>`.

Allowed types are `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `build`, `ci`, and `perf`.
Use `feat/` for a new capability, `fix/` for a bug correction, `docs/` for documentation, `refactor/` for behavior-preserving restructuring, `test/` for tests, `chore/` for maintenance, `build/` for build or dependency work, `ci/` for automation, and `perf/` for performance work.
Use `chore/documentation-standard` for bootstrap work, or `feat/wallet-holds` and `fix/append-lock` for feature branches. Keep issue numbers in the issue and PR title or body, for example `Closes #123`; do not put them in branch names. Do not use uppercase prefixes, underscores, dates, or generic names such as `work` or `update`.
Valid names match `^(feat|fix|docs|refactor|test|chore|build|ci|perf)/(?![0-9]+(?:-|$))[a-z0-9]+(-[a-z0-9]+)*$`. The description cannot start with an all-numeric segment, which prevents issue numbers from being disguised as descriptions.

Once `main` exists, create the worktree and branch together from the latest `main`:

```bash
git worktree add .worktrees/wallet-holds -b feat/wallet-holds
```

## Environment variables

The full list is in `.env.example`; only the tricky ones are explained here.

Copy `.env.example` to `.env` and the server and the tests pick it up automatically (the loader searches the current directory and its parents, so it works from any crate). Real environment variables always win over the file, so exports and CI overrides are never ignored. `.env` is gitignored and must never be committed.

| Variable | Notes |
| --- | --- |
| `DATABASE_URL` | Each organization gets a `ledger_<tenant_id>` schema in this database, plus the shared `oxsum` schema for users, organizations and keys. Tests read it too; without it, oxsum's database tests skip automatically |
| `OXSUM_SIGNUP` | Optional; `invite` (default) or `open`. `invite` refuses `POST /api/v1/auth/register` with 403 — an invitation link (`POST /api/v1/org/invitations` → `/register?invite=…`) is the way in — so a public instance cannot be signed up by whoever arrives first; `open` is what a demo or a private instance wants |
| `OXSUM_SIGNUP_BONUS_MINOR` | Optional; credits a new organization's wallet starts with, in minor units, default 0. Booked at self-registration as an adjustment carrying "signup bonus" as its reason; an invited account joins an existing organization and is granted nothing |
| `OXSUM_DB_MAX_CONNECTIONS` | Optional; size of the one pool every tenant shares (default 10). Each connection is a PostgreSQL backend process, so this is the process's whole budget rather than a per-tenant allowance. Stay below the database's own `max_connections` (PostgreSQL's default is 100) |
| `OXSUM_ADDR` | Optional listen address; defaults to `127.0.0.1:3000` |
| `OXSUM_UPSTREAM_BASE_URL`, `OXSUM_UPSTREAM_API_KEY`, `OXSUM_MODELS` | The bootstrap channel for a database that has none, set together or not at all. With none of them a fresh deployment serves the wallet only. `OXSUM_MODELS` is JSON: each model maps to `inputPricePerMillion`, `outputPricePerMillion` and `maxOutputTokens`, in minor units per million tokens. `maxOutputTokens` is the ceiling for a request that asks for none and the freeze is computed from it, so it must be the model's real limit rather than a safe guess. A malformed value, or a base URL that is not an http(s) URL, refuses to boot rather than serving a price it had to guess |
| `OXSUM_SECRET_KEY` | 32 bytes, base64, sealing the upstream credentials stored in `oxsum.channels` (AES-256-GCM). Required as soon as a channel exists — including one seeded from the environment, which is why setting `OXSUM_MODELS` means setting this too. The server refuses to start when it cannot open a stored credential, rather than failing a request that has already been relayed. Generate one with `openssl rand -base64 32`. Losing it means re-entering every upstream credential |
| `OXSUM_HEAD_SIGNING_KEY` | Optional; 32 bytes, base64, the seed of the Ed25519 key the operator signs tree heads with (`openssl rand -base64 32` generates one). Unset means the `/api/v1/log` endpoints answer 503; the wallet is served without them. A bad value refuses to boot. Rotating it is restarting with a new seed — heads are signed on demand, so nothing is stored to migrate; old signatures stop verifying, which is what a rotation means |
| `OXSUM_ADMIN_TOKEN` | Optional; the platform admin's token for `/api/v1/admin`, at least 16 characters. It is not an API key: it changes channels and prices and belongs to whoever deploys oxsum. Unset means no admin surface (the routes answer 401). Compared in constant time, so a long random value is the right one; rotate it by restarting with a new value |
| `OXSUM_UPSTREAM_NAME` | Optional; the bootstrap channel's name, reported as `owned_by` in `GET /v1/models`. Defaults to `upstream` |
| `OXSUM_HOLD_TIMEOUT` | Optional; how long a gateway hold may sit unsettled before the background sweeper releases it at 0 with settlement kind `swept` (recorded as an anomaly). A number of seconds with an optional `s`/`m`/`h` suffix; defaults to `30m`. Validated at startup: it must parse and be at least 60s. It must exceed the longest possible single request — anything older is abandoned by definition, so a hold that is old but whose request is still streaming cannot exist under a correct configuration. The sweeper passes every 60s |
| `OXSUM_SESSION_COOKIE_SECURE` | Optional; whether the session cookie carries the `Secure` attribute: `true` or `false`, default `false`. A deployment behind TLS must set it to `true`, or browsers will not send the cookie back over https; `false` is what makes local http development work. Anything else refuses to start |
| `RUST_LOG` | Optional `tracing-subscriber` filter; defaults to `info` |
| `LEPTOS_OUTPUT_NAME` | Build-time, not a server setting, and deliberately not in `.env.example`: it is set in `.cargo/config.toml` for every cargo invocation in the workspace, because leptos reads it with `option_env!` *while it is compiled* and the shell it compiles into the server is what names the wasm module the browser loads. See "Web dashboard" |

A deployment's channels and their prices live in the database, and the environment only seeds a
database that has none. Prices are append-only versions: `POST /api/v1/admin/channels/{name}/prices`
adds one, the version in force when a request starts is the one that prices it, and the settlement
names that version, so an old bill stays checkable however often the price changes. To try the loop
against a real provider, set the variables below and point an OpenAI SDK at
`http://127.0.0.1:3000/v1`:

```bash
OXSUM_SIGNUP=open OXSUM_UPSTREAM_BASE_URL=https://api.deepseek.com/v1 \
  OXSUM_UPSTREAM_API_KEY=sk-... \
  OXSUM_SECRET_KEY="$(openssl rand -base64 32)" \
  OXSUM_ADMIN_TOKEN="$(openssl rand -hex 24)" \
  OXSUM_MODELS='{"deepseek-chat":{"inputPricePerMillion":270000,"outputPricePerMillion":1100000,"maxOutputTokens":8192}}' \
  cargo run -p oxsum-server
```

Then a price change is one call, and it lands on later requests only:

```bash
curl -X POST http://127.0.0.1:3000/api/v1/admin/channels/upstream/prices \
  -H "authorization: Bearer $OXSUM_ADMIN_TOKEN" -H 'content-type: application/json' \
  -d '{"model":"deepseek-chat","inputPricePerMillion":280000,"outputPricePerMillion":1200000,"maxOutputTokens":8192}'
```

The gateway's own tests need no provider and no network: `crates/server/tests/gateway.rs` starts a
scripted upstream on a free port inside the test process and selects its answer by model name, so a
refusal, a 200 that is not JSON and a stream that never finishes are all ordinary cases. They still
need `DATABASE_URL`, because every turn freezes and settles real credit. A model belongs to one
channel, so each test world gives its channel and models a random suffix: the shared database would
otherwise make two worlds price the same model, which is the conflict rule working.

There is no shared API token any more: every request carries an organization's API key, created
by registration or by `POST /api/v1/org/keys`. The plaintext secret is returned exactly once.

oxsum's own tables are migrated at startup, in the same process that serves requests: `Db::migrate`
creates the `oxsum` schema and applies the files under `crates/core/migrations/` that have not run
yet, each in one transaction together with its row in `oxsum._migrations`, under a database-wide
advisory lock so two starting processes cannot race. A failed migration refuses to boot; there is no
separate migrate command to remember. Ledger schemas are untouched by it: doubleentry's `migrate`
creates `ledger_<tenant_id>` on first use.

## Commands

| Purpose | Command |
| --- | --- |
| Start the database | `docker compose up -d` |
| Start the server | `cargo run -p oxsum-server` |
| Build the dashboard (SSR + WASM) | `cargo leptos build` |
| Run the server with the dashboard, rebuilding on change | `cargo leptos serve` |
| Check the built site against the browser contract | `OXSUM_SITE_DIR=target/site cargo test -p oxsum-server --test browser_contract -- --ignored --nocapture` (after `cargo leptos build`, see "Browser contract") |
| All tests | `cargo test --workspace` (loads `DATABASE_URL` from `.env` or the shell) |
| oxsum only | `cargo test -p oxsum-core -p oxsum-server` |
| Run the end-to-end demo | `python3 demo/demo.py` (needs `pip install -r demo/requirements.txt`; starts its own mock upstream and server) |
| Generative sequences, quick run | `PROPTEST_CASES=50 cargo test -p oxsum-core --test generative` |
| doubleentry's Postgres tests | `cargo test -p doubleentry --features postgres --test postgres` (spins up its own container via testcontainers) |
| Verify doubleentry builds for the browser | `cargo build -p doubleentry --features serde --target wasm32-unknown-unknown` |
| Format | `cargo fmt --all` |
| Lint | `cargo clippy --workspace --all-targets` |

## Web dashboard

The dashboard lives in `crates/web` (Leptos 0.8): `/login` and `/logout` call the
session endpoints from the browser, and `/dashboard` shows the organization, the
members, the keys, the balance, the in-flight holds and the transaction log. Pages are
served by the same `oxsum` binary through `leptos_axum`: server-side rendering calls
`oxsum-core` directly, browser interactions go through server functions under `/_pages`
(which are the page API, not part of `crates/server/openapi.yaml`).

- `cargo leptos build` compiles the server and the WASM browser side together (the
  styles in `crates/web/style/main.css` are bundled into `target/site/pkg/`).
- `cargo leptos serve` runs the server and rebuilds on change.
- `cargo run -p oxsum-server` also serves the pages (server-side rendered), and serves the
  bundle that `cargo leptos build` wrote: the site root is the workspace's `target/site`,
  resolved by the server rather than taken from the working directory, so the live browser
  side works under plain `cargo run` too (issue #44). Without a build, the pages render and
  never hydrate — no top-up form, no chat, no bill verification — so run `cargo leptos
  build` first.
- The shell's markup names the files the build writes. leptos decides the wasm file name
  when *leptos is compiled*: `HydrationScripts` appends `_bg` unless `LEPTOS_OUTPUT_NAME`
  is set, which only cargo-leptos did. The module the site holds is `pkg/oxsum.wasm`
  (wasm-bindgen emits `oxsum_bg.wasm`, and cargo-leptos renames it to the name
  `HydrationScripts` computes for `output-name`). With the variable unset, a server built
  by plain `cargo run` asked for `/pkg/oxsum_bg.wasm`, which no build writes: the import
  404'd and every page stayed as inert as the server had rendered it (issue #65).
  `.cargo/config.toml` sets `LEPTOS_OUTPUT_NAME` for every cargo invocation in this
  workspace, so however the server was built it asks for `pkg/oxsum.wasm`; only the
  variable's presence matters to leptos, and `cargo leptos build` sets the same one. This
  is pinned by `the_shell_asks_for_the_wasm_file_the_built_site_holds` in
  `crates/server/tests/dashboard.rs`, which runs in the ordinary
  `cargo test -p oxsum-server` gate: its test binary is a plain-cargo build, so it checks
  what `cargo run` renders, and when `target/site` exists it also fetches the module the
  markup names and requires 200.
- In-flight gateway turns are pushed to the dashboard over `/ws/billing` (WebSocket,
  session-cookie auth): a snapshot of the open holds on connect, then started, progress
  and settled events as turns happen. The events come from a process-wide broadcast the
  gateway publishes to — best-effort and in-memory; the watch table stays the record.
- The public `/verify` page needs no login: it pastes a proof bundle and a content hash
  into `oxsum_verify::verify_bundle` running in the browser (WASM), the same function
  body the server runs. The pure verification logic lives in `crates/verify`
  (`oxsum-verify`), shared through `oxsum-core` re-exports — `oxsum-core` itself cannot
  target wasm32 because sqlx-postgres needs OS sockets (docs/decisions.md).

## Docker image

The release image is built by the `Dockerfile` at the repo root: a multi-stage build of
the whole workspace, including the Leptos dashboard.

```bash
docker build -t oxsum .
docker run --rm -p 3000:3000 -e DATABASE_URL=postgresql://user:pass@dbhost/oxsum oxsum
```

- Builder: `rust:1.98-bookworm` (matches `rust-toolchain.toml`), with `cargo-leptos`
  0.3.11 and the `wasm32-unknown-unknown` target. `cargo leptos build --release`
  compiles the `oxsum` server binary and the WASM browser side together; the
  dashboard's style is plain CSS, so no Node.js or npm is needed.
- Runtime: `debian:bookworm-slim` plus `ca-certificates`. reqwest uses rustls, so no
  OpenSSL is required. It carries the `oxsum` binary and `target/site` (the WASM
  bundle, CSS and `index.html`), which the server finds through `LEPTOS_SITE_ROOT`
  (`/app/site` in the image).
- The image never bakes in `.env` (`.dockerignore` excludes it); `DATABASE_URL` points
  at an external PostgreSQL and is supplied at `docker run`, along with the `OXSUM_*`
  settings above. `OXSUM_ADDR` defaults to `0.0.0.0:3000` inside the image so the port
  mapping works. The schema is migrated at startup, so there is no separate migrate step.

Every pull request builds this image and smoke-tests it over HTTP against a real
PostgreSQL, see "Continuous integration" below: the image is the only release
artifact, and nothing else in the gates runs it.

## Continuous integration

`.github/workflows/ci.yml` runs on every push to `main` and every pull request that
touches more than documentation (`paths-ignore` skips `docs/**` and `*.md`). The
toolchain comes from `dtolnay/rust-toolchain@stable` with `toolchain: "1.98"` (matching
`rust-toolchain.toml`) and the `rustfmt, clippy` components:

- `fmt`: `cargo fmt --all --check`.
- `clippy`: `cargo clippy --workspace --all-targets` with `-D warnings`. No database
  is needed to compile: the tree uses no `sqlx::query!` macros, so compilation never
  touches a live database.
- `test`: `cargo test --workspace` against a `postgres:17-alpine` service container
  (with `DATABASE_URL` set, so the database tests run for real instead of skipping).
  doubleentry's own Postgres conformance suite
  (`cargo test -p doubleentry --features postgres --test postgres`), which starts its
  own container through testcontainers, runs only when the run touches the vendored
  engine, the migration list or `Cargo.lock` — the crate is vendored and otherwise
  unchanged, so running it on every push bought several minutes for no signal.
  Docker makes it the one suite that cannot run on a plain developer machine, which
  is why the conditional lives in CI rather than a local gate.
- `browser`: the browser contract gate. It adds the `wasm32-unknown-unknown` target,
  installs `cargo-leptos` 0.3.11 (the version the Dockerfile pins; the binary is
  cached by `actions/cache`, so only the first run after the pin changes compiles
  it), runs `cargo leptos build`, and then runs the ignored `browser_contract` test
  with `OXSUM_SITE_DIR=target/site`. No PostgreSQL service: the routes it checks are
  answered without a database.

The compile jobs cache the cargo registry and `target/` through
`Swatinem/rust-cache`, so a run that touches one crate does not recompile the
dependency tree. Every job is independent, so `browser` runs in parallel with the
other three. `main` requires all four checks to pass — a pull request cannot merge
while any of them is red.

`wasm32-unknown-unknown` is a second compile target, and `cargo check`/`cargo clippy`
never build it: `oxsum-web` code that only exists under `feature = "hydrate"` (event
handlers, `web_sys` calls) compiles nowhere in the usual local gate. Before pushing
a change to `crates/web`, check the browser side the same way the `browser` job does:

```bash
cargo check -p oxsum-web --target wasm32-unknown-unknown --no-default-features --features hydrate
```

(The `--no-default-features` matters: without it `ssr` stays enabled and pulls `axum`
and `mio` into a target that cannot compile them.)

## Browser contract

`crates/server/tests/browser_contract.rs` is the one suite that checks the built site
rather than a router. It is `#[ignore]`d, so `cargo test --workspace` stays green on a
machine that never ran `cargo leptos build`; CI runs it explicitly, and it fails with a
clear message when `OXSUM_SITE_DIR` is unset rather than skipping.

```bash
cargo leptos build
OXSUM_SITE_DIR=target/site cargo test -p oxsum-server --test browser_contract -- --ignored --nocapture
```

A relative `OXSUM_SITE_DIR` resolves against the workspace root — where `cargo leptos
build` writes `target/site`, and what `site-root` in the workspace `Cargo.toml` is
relative to. It needs no database. The checks are the contract the two defects of
2026-10-03 broke, and both are visible from the built site and the served HTML, without a
browser:

- the site holds the browser bundle: `pkg/oxsum.js` (the wasm-bindgen glue) and
  `pkg/oxsum.wasm` (the module; wasm-bindgen emits `oxsum_bg.wasm`, and cargo-leptos
  0.3.11 renames it to the name `HydrationScripts` computes). A missing, failed or
  half-finished build fails here.
- the server, built with the project's real Leptos options, serves both over HTTP with
  200. A site root resolved against the working directory answers 404 and leaves every
  page as inert as the server rendered it (issue #44).
- `GET /login` carries leptos's hydration script, preloads `/pkg/oxsum.js`, and renders
  the login form with `method="post"`, so a browser that never hydrates does not submit
  the credentials in the query string (issue #46).
- the glue exports `hydrate`, the entry point the generated HTML calls once the wasm has
  loaded: `import("/pkg/oxsum.js").then(mod => mod.default({module_or_path:
  "/pkg/oxsum.wasm"}).then(() => mod.hydrate()))`. Without that export the import throws
  `mod.hydrate is not a function` and nothing on any page works (issue #46).

A second workflow, `.github/workflows/release-image.yml`, gates the release image
itself: it runs on pushes to `main` and on `workflow_dispatch` — not on pull
requests, where the `browser` job already compiles the same site the image
packages, and a Docker build per push duplicated it for no extra signal.
It builds the `Dockerfile` with `docker/build-push-action` (`load: true`, so the image
lands in the runner's own daemon and is never published) and
`cache-from`/`cache-to` of `type=gha`, so a rebuild after a one-line change reuses the
Rust and WASM layers instead of paying for another 20-40 minute release compile. The
built image is then run with `--network host` against the same `postgres:17-alpine`
service container: the runner is Linux, and the service publishes its port on the
runner's loopback, so `DATABASE_URL` names `127.0.0.1:5432` to match that choice. The
workflow waits for `pg_isready`, then polls `http://127.0.0.1:3000/healthz` in a
bounded retry loop and asserts that:

- `/healthz` answers 200. The image migrates at startup and refuses to boot at all
  against a database it cannot reach, so a 200 also means the schema is in place.
- `/pkg/oxsum.js` and `/pkg/oxsum.wasm` answer 200 (`cargo-leptos` renames
  wasm-bindgen's `oxsum_bg.wasm` to `oxsum.wasm`, and it is the name the shell's
  module preload uses). A 404 here is the site root regression of issue #44: the
  pages render and none of them hydrate.
- `/login` carries the Leptos hydration script (`mod.hydrate()`) and a
  `<form ... method="post"` form. Without the script the login form falls back to a
  native GET submit, which puts the password in the query string (issue #46).

When an assertion fails the step prints `docker logs` and exits non-zero, so a red run
is diagnosable from its own log rather than by re-running it.

## Test strategy

- Unit tests: inside each module's `#[cfg(test)]`, covering pure logic such as tenant id validation and configuration parsing.
- Integration tests:
  - `crates/core/tests/` exercises the wallet's invariants on real PostgreSQL: isolation, concurrent no-overdraw, hold/settle, idempotency, tamper detection, restart recovery, concurrent migrate; `crates/core/tests/identity.rs` covers registration, organizations and API keys, including that the plaintext secret is nowhere in the database; `crates/core/tests/sessions.rs` covers login, session authentication, logout and the key role scopes, including that the plaintext token is nowhere in the database and that a session dies with its membership.
  - `crates/core/tests/generative.rs` generates random sequences of top-ups, holds, settlements, replays and key collisions and checks each step against an in-memory model of the wallet account's two layers: the outcome the model predicted, the available balance, that it never went negative, the log size, and a proof for every entry the case wrote (including that a changed amount stops verifying). 1000 cases by default, `PROPTEST_CASES` to narrow it locally; a failure prints its seed and the shortest failing sequence. Since settlement-hold pairing (issue #10) a settlement names the hold it releases, and the oracle predicts `HoldNotFound` for a settlement naming a key that is not an outstanding hold. The suite loads `.env` before reading `DATABASE_URL` (issue #18): with the variable only in `.env` it runs for real, and with no database anywhere it skips all cases with a warning rather than failing.
  - `crates/server/tests/` covers the HTTP layer: key auth, error format, full request chains, and that one organization's key cannot reach another's ledger; `crates/server/tests/sessions.rs` covers the login/logout/session endpoints, the cookie attributes, and the member/owner key rules over HTTP. `crates/server/tests/dashboard.rs` covers the pages' own contract: the pages render server-side, the pkg bundle is served from the real site root, the billing socket refuses an unknown session, and the shell's markup asks for the wasm module the built site holds (`pkg/oxsum.wasm`, the name cargo-leptos writes — issue #65).
- doubleentry's own tests: changing `crates/doubleentry` requires all of them passing, including its conformance suite.
- E2E: the full top-up → call → verify flow is covered. `crates/server/tests/chat_flow.rs` drives login, a top-up, one streaming chat turn through a scripted upstream, the billing events, and then fetches the settlement's proof bundle and verifies it with `oxsum_verify::verify_bundle`; `demo/demo.py` runs the same flow end to end with the official OpenAI SDK and prints both proof bundles for `/verify` to check. TODO.md carries the current status.

## Regression checklist

Before committing, confirm:

- [ ] Branch name matches the branch naming policy above, and the PR links its issue in the title or body where applicable
- [ ] `cargo fmt --all` and `cargo clippy --workspace --all-targets` produce no warnings
- [ ] With `DATABASE_URL` set in `.env` or the shell, `cargo test --workspace` passes
- [ ] Interface changed: `openapi.yaml`, implementation, tests and callers are all in sync
- [ ] `crates/doubleentry` changed: carries an `oxsum change` comment and an entry in docs/decisions.md
- [ ] UI changed: loading, empty and error states all checked
- [ ] Related docs updated (see the doc update rules in AGENTS.md)

## Release

The release is a single binary plus a Docker image (TODO.md B-10, closed 2026-10-03):
`cargo leptos build --release` produces the `oxsum` binary and `target/site`, and the
`Dockerfile` packages them into the image described above. CI
(`.github/workflows/ci.yml`) is the green gate on a fresh clone, and every pull request
also builds the image and runs it against PostgreSQL
(`.github/workflows/release-image.yml`, issue #49), so the artifact a release ships is
checked long before the release is cut.
