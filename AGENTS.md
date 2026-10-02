# oxsum

A verifiable AI credit wallet: users top up credits, each streaming LLM call freezes an upper bound before it starts and settles against actual usage afterwards, and users can verify their bills in the browser themselves. The project is a full-stack Rust portfolio piece.

## Tech stack

- Language: Rust 1.98, full stack, see `rust-toolchain.toml`
- Ledger core: doubleentry, vendored in `crates/doubleentry`
- Backend: axum 0.8 + sqlx + PostgreSQL 17
- Frontend: Leptos 0.8 (server-side rendering + browser hydration, `crates/web` not yet created); verification runs `oxsum_core::verify_bundle` directly inside a Leptos component
- Deployment: a single binary + PostgreSQL, database runs locally via Docker Compose

## Layout

```
crates/doubleentry/  Vendored ledger engine. Change rules below under Hard rules
crates/core/         Domain layer: multi-tenant wallet, hold/settle, bill proofs
crates/server/       HTTP layer: routing, auth, error mapping, produces the oxsum binary. Conventions in crates/server/AGENTS.md
docs/                Project documentation
```

## Common commands

Full command list in docs/development.md.

```bash
docker compose up -d                       # start the database
cargo run -p oxsum-server                  # start the server (needs variables from .env)
cargo test --workspace                    # all tests (loads .env when present)
cargo fmt --all && cargo clippy --workspace --all-targets
```

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
