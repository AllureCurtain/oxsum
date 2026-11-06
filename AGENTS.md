# oxsum

A verifiable AI credit wallet: users top up credits, each streaming LLM call freezes an upper bound before it starts and settles against actual usage afterwards, and users can verify their bills in the browser themselves. The project is a full-stack Rust portfolio piece.

## Tech stack

- Language: Rust stable, full stack; the workspace MSRV is `rust-version = "1.98"` in the root manifest
- Ledger core: doubleentry, vendored in `crates/doubleentry`
- Backend: axum 0.8 + sqlx + PostgreSQL 17
- Frontend: React + Vite dashboard in `web/` (issue #234), embedded into the binary and served at `/app`; the Topcoat server-rendered pages retired to it (issue #256) — `crates/web` now holds only the deliberately server-rendered `/verify` page and the bills-export row types. Legacy `/dashboard/*`, auth and `/admin` paths `301` to `/app`. UI rules in DESIGN.md
- Deployment: a single binary + PostgreSQL, database runs locally via Docker Compose

## Layout

```
crates/doubleentry/   Vendored ledger engine. Change rules below under Hard rules
crates/core/          Domain layer: multi-tenant wallet, hold/settle, bill proofs
crates/verify/        Pure verification (ProofBundle, verify_bundle), wasm32-clean for the browser
crates/integrations/  Third-party service clients: GitHub OAuth, Stripe, Turnstile, SMTP mail
crates/server/        HTTP layer: routing, auth, error mapping, produces the oxsum binary. Conventions in crates/server/AGENTS.md
crates/web/           The server-rendered sliver: `/verify` + its stylesheet + the bills-export types the server reuses. UI rules in DESIGN.md
web/                  React dashboard (issue #234), embedded at /app; README has the parity checklist
examples/             Developer examples: verify-consumer (packed SDK consumer, issue #561), scoped-integration (Python metering client), scoped-management + consumption + webhook-tools (runnable walkthroughs, issue #617)
deploy/               Self-hosted Compose profiles (core + payment overlay) and operator.env.example (issue #619); runbooks in docs/self-hosted-*.md
docs/                 Project documentation
```

## Common commands

Full command list in docs/development.md.

```bash
docker compose up -d                       # start the database
cargo run -p oxsum-server                  # start the server (needs variables from .env)
cargo nextest run -p oxsum-<crate>       # focused tests while iterating (loads .env when present)
cargo nextest run --workspace            # the full suite — cross-crate changes, or just before a risky push
cargo fmt --all && cargo clippy --workspace --all-targets
```

`cargo nextest` is the default test runner: it runs test binaries in parallel
and skips doctest compilation, so a workspace run is markedly faster than
`cargo test --workspace`. `cargo test` remains the fallback and the only
runner for doctests (`cargo test --doc`).

### Testing scope

Default to the narrowest run that still covers the change:

```bash
cargo nextest run -p oxsum-<crate>                     # one crate's suites
cargo nextest run -p oxsum-server --test <suite>       # one test binary
cargo nextest run -p oxsum-core --test <suite> --test <other>   # several, one selection
```

`cargo nextest run --workspace` is **not** the iteration command — it compiles
and runs every test binary (~7 minutes, dozens of `de_*`/`oxsum_test_*`
scratch databases). Reach for it only when the change crosses crates (a shared
trait, a migration ordering rule, a workspace dependency) or right before a
merge whose red CI would cost more than the suite. PR-required CI (`fmt`,
`clippy`, `test`, `wasm`, `frontend`) stays cheap because the `test` job is
`--lib`/`--doc` only; the heavy integration shards run post-merge on `main`,
not on pull requests.

### Test scratch databases

Suites create their own PostgreSQL databases on the shared dev cluster and
drop them at process exit:

- `oxsum_test_<slug>_<unix_ts>_<pid>` — one per `tests/*.rs` file, created by
  `oxsum-testkit`, dropped via `#[ctor::dtor]`
- `de_<32 hex>` — one per `doubleentry` Postgres conformance test, dropped in
  the harness's `Drop` impl (fire-and-forget `WITH (FORCE)`)

A run that is killed (Ctrl-C, `timeout`, a crashed laptop) never reaches its
drop code, so residue accumulates on the dev cluster. The sweep is the next
run's job, not the test's own — CI runs `scripts/drop-test-scratch.sql` before
`nextest`, and you can run the same file by hand:

```bash
docker exec -i oxsum-postgres-1 psql -U postgres -d postgres -f - < scripts/drop-test-scratch.sql
```

The pattern is strict enough that a real database cannot match it. Do **not**
manually `DROP DATABASE` a live suite mid-run — if a `de_*` or
`oxsum_test_*` name is under 24 h old, assume it still belongs to a colleague's
in-flight run on the shared cluster. Anything matching the pattern that has
survived a day is safe to delete.

### Shared build output

Every worktree reads `.worktrees/.cargo/config.toml` (gitignored), which points
the whole family at one shared target directory *inside* the worktree parent:

```toml
[build]
target-dir = "/Users/yao/dev/projects/oxsum/.worktrees/target"
```

Dependency crates compile once per machine, not once per worktree — this is
what keeps five parallel worktrees from producing five multi-GB `target/`
trees. Putting it inside `.worktrees/` (which is gitignored) means the whole
artifact cache disappears with a `rm -rf .worktrees/` and never pollutes
`$HOME`. Serialize heavy `cargo`/`nextest` invocations across worktrees so the
shared lock does not serialize them anyway; everything else (fmt, docs,
`cargo check -p <crate>`) runs fine in parallel.

### Worktree lifecycle

Every task gets a `.worktrees/<slug>` created from the latest `main`. When its
PR merges:

```bash
git worktree remove .worktrees/<slug>
git branch -d <type>/<slug>
```

Merged worktrees accumulate fast — check `git worktree list` against
`gh pr list --state merged` and remove the ones whose branch has merged. Keep
the worktree until its dependents are safe (another open PR branched off it);
after that it is clutter, not history.

## Before pushing

CI runs the full gate — lib/doc tests, the three nextest shards, the process-shared suites and the doubleentry Postgres conformance run — and every job sits on the required-check chain, so a regression cannot reach main through a PR. The local gate is the fast half:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo nextest run -p <crate>   # the crates the change touched
```

Run `cargo nextest run --workspace` locally when the change crosses crates — a trait, shared config, a workspace dependency — or when CI being red would cost more than the seven minutes the suite takes. A `--admin` merge skips every check; never use it on a PR whose CI is not fully green.

## Development process

Issue-driven, one PR per minimal feature; once `main` exists, all task development happens in git worktrees:

- Settle the direction in an issue first, with completion criteria written down, before touching code. TODO.md entries map one-to-one onto issues: the issue carries the discussion, TODO carries only status.
- One PR does one thing: a minimal feature, or the complete closure of one issue. If two unrelated things need changing, split into two PRs.
- A PR may contain multiple commits, split by logical step (migration, implementation, tests, docs are separate commits), so history stays readable after a rebase merge.
- Changes and their documentation go into the same PR, see the doc-update rules below.

### Worktree flow

- During repository bootstrap, a dedicated bootstrap branch may be used for the initial repository and documentation setup. It still follows the branch naming policy below. Once `main` exists, all task work happens in worktrees.
- One worktree per task (issue/PR), created as `.worktrees/<task slug>` under the repo root; `.worktrees/` is in .gitignore.
- The worktree directory uses a slash-free task slug (for example, `.worktrees/wallet-holds`); the branch name keeps its `/`.
- Always create a worktree from the latest `main`: first `git -C <main workspace> pull` (or `git fetch` + `git reset --hard origin/main`, only when local main has no commits of its own), then `git worktree add .worktrees/wallet-holds -b feat/wallet-holds`. **Never commit directly on main**; main only receives PR merges.
- Finish the minimal feature for one PR inside the worktree, push the branch, open the PR. Once the PR merges, the worktree can be removed: `git worktree remove .worktrees/<name>`; the branch can be deleted after the PR closes.
- After merging, sync both mains: `git -C <main workspace> pull` locally, and confirm GitHub's main is current too, so the next worktree is created from the latest code.
- An unrelated change made while browsing another worktree is not a task: without a PR it does not get committed.

### Branch naming

- A branch contains one issue or PR and uses lowercase ASCII in the form `<type>/<kebab-case-description>`.
- Valid names match `^(feat|fix|docs|refactor|test|chore|build|ci|perf)/(?![0-9]+(?:-|$))[a-z0-9]+(-[a-z0-9]+)*$`. The description cannot start with an all-numeric segment, which prevents issue numbers from being disguised as descriptions.
- Allowed types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `build`, `ci`, and `perf`.
- Prefix meanings: `feat` new capability, `fix` bug correction, `docs` documentation, `refactor` behavior-preserving restructuring, `test` tests, `chore` maintenance, `build` build or dependency work, `ci` automation, and `perf` performance work.
- Use `feat/` and `fix/`, never `FEAT_`, `FIX_`, `feature/`, or `hotfix/`.
- The description must state the outcome of this one issue/PR; do not use `work`, `tmp`, `update`, dates, spaces, or underscores.
- Keep issue numbers out of branch names; link the issue from the PR title or body with `Closes #123` or `Refs #123`.
- Examples: `chore/documentation-standard`, `feat/wallet-holds`, `fix/append-lock`, `docs/branch-naming`.

## Language

All project documentation, code comments, and commit messages are written in English: README (optionally plus a README.zh-CN.md), `///` doc comments, internal `//` comments, openapi.yaml descriptions, UI copy, commit messages, and docs/ — everything. The only exception would be a Chinese README variant, if ever added.

## Hard rules

- Interface fields are defined by `crates/server/openapi.yaml`. Change the contract first, then the implementation.
- Ledger table structure is managed by doubleentry's `migrate`; never edit it by hand. oxsum's own tables are only added through migrations.
- Money is always an integer in minor units (1 credit = 1_000_000), never floating point.
- When changing `crates/doubleentry`:
  - Mark the change site with an `oxsum change (not upstream):` comment explaining why
  - Add an entry to docs/decisions.md
  - Get all of its tests passing
- Never commit `.env`. New environment variables are added to `.env.example` in the same change.
- Read DESIGN.md before touching styles.
- Read docs/decisions.md before making a technology choice or overturning an existing one.

## Doc index

| When | Read |
| --- | --- |
| Starting any task | TODO.md |
| Product behavior: roles, billing rules, pages | docs/product.md |
| Module boundaries, data flow, accounting model | docs/architecture.md |
| Interfaces | docs/api.md and `crates/server/openapi.yaml` |
| UI | DESIGN.md |
| Environment, commands, tests, release | docs/development.md |
| Changing the technical approach | docs/decisions.md |
| User-visible feature changes | docs/user-guide.md |
| Verifying a bill as its customer | docs/verify-your-bill.md |
| What each proof covers, and what it does not | docs/proofs.md |
| Supported integrations and developer examples | docs/integrations.md |
| Refused calls and their exact errors | docs/unsupported.md |
| Self-hosted install (operator runbook) | docs/self-hosted-install.md |
| Self-hosted upgrade, rollback and restore | docs/self-hosted-upgrade.md |
| Release candidate gates and promotion | docs/self-hosted-release.md and docs/roadmap/self-hosted-v1/RELEASE-CHECKLIST.md |
| Security reports / operator support | SECURITY.md / SUPPORT.md |

## Doc update rules

Documentation is updated in the same PR as the code:

- Starting or finishing a task: update TODO.md
- Product behavior changed (roles, billing, pages): update docs/product.md
- Module boundaries or data flow changed: update docs/architecture.md
- A technical decision made: append an entry to docs/decisions.md
- New command or environment variable: update docs/development.md and .env.example
- Interface conventions changed: update docs/api.md; interface fields, `crates/server/openapi.yaml`
- User-visible feature change: update docs/user-guide.md
- New design rule or token: update DESIGN.md
