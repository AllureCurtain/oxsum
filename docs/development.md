# Development guide

## Requirements

- Rust 1.98: `rust-toolchain.toml` selects it automatically; rustup installs it on demand
- Docker: runs local PostgreSQL 17, also used by doubleentry's Postgres tests
- `wasm32-unknown-unknown` target: needed by the verification page and the Leptos browser build. `rustup target add wasm32-unknown-unknown --toolchain 1.98-x86_64-pc-windows-msvc` (or the platform equivalent). doubleentry is verified to compile for this target; uuid's randomness source is handled by a target dependency in `crates/doubleentry/Cargo.toml`
- `cargo-leptos`: install once `crates/web` exists (`cargo install cargo-leptos`); it builds Leptos's server and WASM sides together. Not needed yet

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
| `OXSUM_SIGNUP` | Optional; `invite` (default) or `open`. `invite` refuses `POST /api/v1/auth/register` with 403, so a public instance cannot be signed up by whoever arrives first; `open` is what a demo or a private instance wants |
| `OXSUM_DB_MAX_CONNECTIONS` | Optional; size of the one pool every tenant shares (default 10). Each connection is a PostgreSQL backend process, so this is the process's whole budget rather than a per-tenant allowance. Stay below the database's own `max_connections` (PostgreSQL's default is 100) |
| `OXSUM_ADDR` | Optional listen address; defaults to `127.0.0.1:3000` |
| `OXSUM_UPSTREAM_BASE_URL`, `OXSUM_UPSTREAM_API_KEY`, `OXSUM_MODELS` | The bootstrap channel for a database that has none, set together or not at all. With none of them a fresh deployment serves the wallet only. `OXSUM_MODELS` is JSON: each model maps to `inputPricePerMillion`, `outputPricePerMillion` and `maxOutputTokens`, in minor units per million tokens. `maxOutputTokens` is the ceiling for a request that asks for none and the freeze is computed from it, so it must be the model's real limit rather than a safe guess. A malformed value, or a base URL that is not an http(s) URL, refuses to boot rather than serving a price it had to guess |
| `OXSUM_SECRET_KEY` | 32 bytes, base64, sealing the upstream credentials stored in `oxsum.channels` (AES-256-GCM). Required as soon as a channel exists — including one seeded from the environment, which is why setting `OXSUM_MODELS` means setting this too. The server refuses to start when it cannot open a stored credential, rather than failing a request that has already been relayed. Generate one with `openssl rand -base64 32`. Losing it means re-entering every upstream credential |
| `OXSUM_ADMIN_TOKEN` | Optional; the platform admin's token for `/api/v1/admin`, at least 16 characters. It is not an API key: it changes channels and prices and belongs to whoever deploys oxsum. Unset means no admin surface (the routes answer 401). Compared in constant time, so a long random value is the right one; rotate it by restarting with a new value |
| `OXSUM_UPSTREAM_NAME` | Optional; the bootstrap channel's name, reported as `owned_by` in `GET /v1/models`. Defaults to `upstream` |
| `OXSUM_HOLD_TIMEOUT` | Optional; how long a gateway hold may sit unsettled before the background sweeper releases it at 0 with settlement kind `swept` (recorded as an anomaly). A number of seconds with an optional `s`/`m`/`h` suffix; defaults to `30m`. Validated at startup: it must parse and be at least 60s. It must exceed the longest possible single request — anything older is abandoned by definition, so a hold that is old but whose request is still streaming cannot exist under a correct configuration. The sweeper passes every 60s |
| `OXSUM_SESSION_COOKIE_SECURE` | Optional; whether the session cookie carries the `Secure` attribute: `true` or `false`, default `false`. A deployment behind TLS must set it to `true`, or browsers will not send the cookie back over https; `false` is what makes local http development work. Anything else refuses to start |
| `RUST_LOG` | Optional `tracing-subscriber` filter; defaults to `info` |

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
| All tests | `cargo test --workspace` (loads `DATABASE_URL` from `.env` or the shell) |
| oxsum only | `cargo test -p oxsum-core -p oxsum-server` |
| Generative sequences, quick run | `PROPTEST_CASES=50 cargo test -p oxsum-core --test generative` |
| doubleentry's Postgres tests | `cargo test -p doubleentry --features postgres --test postgres` (spins up its own container via testcontainers) |
| Verify doubleentry builds for the browser | `cargo build -p doubleentry --features serde --target wasm32-unknown-unknown` |
| Format | `cargo fmt --all` |
| Lint | `cargo clippy --workspace --all-targets` |

## Test strategy

- Unit tests: inside each module's `#[cfg(test)]`, covering pure logic such as tenant id validation and configuration parsing.
- Integration tests:
  - `crates/core/tests/` exercises the wallet's invariants on real PostgreSQL: isolation, concurrent no-overdraw, hold/settle, idempotency, tamper detection, restart recovery, concurrent migrate; `crates/core/tests/identity.rs` covers registration, organizations and API keys, including that the plaintext secret is nowhere in the database; `crates/core/tests/sessions.rs` covers login, session authentication, logout and the key role scopes, including that the plaintext token is nowhere in the database and that a session dies with its membership.
  - `crates/core/tests/generative.rs` generates random sequences of top-ups, holds, settlements, replays and key collisions and checks each step against an in-memory model of the wallet account's two layers: the outcome the model predicted, the available balance, that it never went negative, the log size, and a proof for every entry the case wrote (including that a changed amount stops verifying). 1000 cases by default, `PROPTEST_CASES` to narrow it locally; a failure prints its seed and the shortest failing sequence. It settles only holds the ledger actually took, because settling an amount that was never held is accepted today (issue #6). The suite loads `.env` before reading `DATABASE_URL` (issue #18): with the variable only in `.env` it runs for real, and with no database anywhere it skips all cases with a warning rather than failing.
  - `crates/server/tests/` covers the HTTP layer: key auth, error format, full request chains, and that one organization's key cannot reach another's ledger; `crates/server/tests/sessions.rs` covers the login/logout/session endpoints, the cookie attributes, and the member/owner key rules over HTTP.
- doubleentry's own tests: changing `crates/doubleentry` requires all of them passing, including its conformance suite.
- Planned:
  - E2E: the full top-up → call → verify flow
  - See TODO.md

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

Not decided yet, see TODO.md. The goal is a single binary plus a Docker image.
