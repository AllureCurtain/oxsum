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
| `RUST_LOG` | Optional `tracing-subscriber` filter; defaults to `info` |

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
| doubleentry's Postgres tests | `cargo test -p doubleentry --features postgres --test postgres` (spins up its own container via testcontainers) |
| Verify doubleentry builds for the browser | `cargo build -p doubleentry --features serde --target wasm32-unknown-unknown` |
| Format | `cargo fmt --all` |
| Lint | `cargo clippy --workspace --all-targets` |

## Test strategy

- Unit tests: inside each module's `#[cfg(test)]`, covering pure logic such as tenant id validation and configuration parsing.
- Integration tests:
  - `crates/core/tests/` exercises the wallet's invariants on real PostgreSQL: isolation, concurrent no-overdraw, hold/settle, idempotency, tamper detection, restart recovery, concurrent migrate; `crates/core/tests/identity.rs` covers registration, organizations and API keys, including that the plaintext secret is nowhere in the database.
  - `crates/server/tests/` covers the HTTP layer: key auth, error format, full request chains, and that one organization's key cannot reach another's ledger.
- doubleentry's own tests: changing `crates/doubleentry` requires all of them passing, including its conformance suite.
- Planned:
  - Generative tests: random operation sequences checked against an in-memory model
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
