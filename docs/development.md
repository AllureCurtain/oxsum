# Development guide

## Requirements

- Candidate bundles: `python3 scripts/test-candidate-bundle.py` checks negative
  byte/provenance fixtures; `python3 scripts/candidate_bundle.py validate
  <bundle-directory> --sha <sha> --online` checks downloaded artifacts against
  their successful GitHub candidate and full-CI runs. `--online` needs `gh`;
  omitting it checks local consistency only. `VERIFY_CONSUMER_OUTPUT=<new-dir>
  scripts/check-verify-consumer.sh` retains the exact installed SDK tarball.

- Image promotion: `python3 scripts/test-candidate-promotion.py` tests refusal,
  immutable identity and credential boundaries without registry access.
  `python3 scripts/test-candidate-promotion.py --local-registry <new-output-dir>`
  exercises a task-owned loopback registry using the script's explicitly pinned
  image (must already be local). `scripts/promote-candidate.py` consumes saved
  candidate bytes; production requires `gh` plus secret-store exports
  `OXSUM_REGISTRY_USERNAME` and `OXSUM_REGISTRY_TOKEN`, never a root `.env` load.
  See [the promotion runbook](self-hosted-release.md) for selectors and receipts.

- Digest deployment: `python3 scripts/test-candidate-deploy.py` exercises
  runtime evidence, immutable plans, Compose rendering, refusal and managed
  state transitions. Its loopback HTTP fixture uses a mocked Docker state model;
  it is not an application-image deployment. `scripts/deployment_plan.py`
  validates candidate provenance and published digest with read-only registry
  authority; `scripts/deploy-candidate.py` applies only trusted plans on Linux.
  See [the deployment/rollback runbook](self-hosted-upgrade.md). No helper builds
  or publishes an image. `OXSUM_IMAGE_REF` is the shared Compose image input;
  `SMOKE_VERSION_OUTPUT` retains observed candidate runtime JSON.

- Candidate validation: `python3 scripts/test-candidate-ci.py` exercises the
  full-CI evidence gate without credentials. `scripts/check-candidate-ci.py
  --sha <40-character-sha> --repository AllureCurtain/oxsum --output ci.json`
  requires authenticated `gh` and refuses absent, failed or skipped heavy
  checks. The release workflow runs the image smoke automatically; see
  [self-hosted-release.md](self-hosted-release.md).

- Rust stable: no toolchain pin — rustup uses whatever `stable` resolves to. The workspace MSRV is `rust-version = "1.98"` in the root manifest; cargo will refuse to build on anything older.
- Rustfmt: the pinned nightly in `rustfmt-nightly` (currently `nightly-2026-11-01`) with the `rustfmt` component. `scripts/fmt.sh` prints the `rustup toolchain install` command when it is missing. The nightly is needed for `imports_granularity` and `format_macro_matchers`, which stable ignores.
- Docker: runs local PostgreSQL 17, also used by doubleentry's Postgres tests

On Windows with Git Bash, Git's own `/usr/bin/link` shadows MSVC's `link.exe` and linking fails with `link: missing operand`. Either build inside a VS Developer shell, or export the `vcvars64.bat` environment into the current shell first.

## First setup

```bash
cp .env.example .env
docker compose up -d
cargo build --workspace
```

Initialize the Business owner through the [installation runbook](self-hosted-install.md)
and use `/app/business` with that owner's session. Fresh business policy is
invite-only; `OXSUM_SIGNUP` is a legacy parsed value, not authorization to create
customers. Use isolated fixtures for tests and retain the root `.env` separately.

### Formatting

Format with `scripts/fmt.sh`, never bare `cargo fmt`. The script reads
`rustfmt-nightly`, formats only the workspace's own crates and is the same
entry point the CI `fmt` job and `.githooks/pre-commit` use, so a local run and
a CI run cannot disagree about the formatter version. `scripts/fmt-check.sh`
is the read-only form.

- **Why nightly.** `imports_granularity = "Module"` and
  `format_macro_matchers = true` in `rustfmt.toml` are unstable options; stable
  rustfmt silently ignores them, so a stable run would pass CI while leaving
  the import rules unenforced.
- **Why not `cargo fmt --all`.** `crates/doubleentry` is vendored upstream
  code and must keep its layout. The script passes an explicit `-p` list and
  never formats it.
- **Upgrading.** Change the one line in `rustfmt-nightly`, run
  `scripts/fmt.sh`, and commit the version bump with the resulting diff as its
  own PR. CI reads the branch's version file, so the upgrade PR is formatted by
  the new version while every other PR stays on the old one.

### Lints

The CI `clippy` job runs `cargo clippy --workspace --all-targets -- -D
warnings`. Workspace lints are declared once in `[workspace.lints]` of the root
manifest; every member opts in with `[lints] workspace = true`, so a new crate
inherits them by adding the same table. The current set is:

- `unsafe_code = "forbid"` — no member may add an `unsafe` block.
- `missing_docs = "warn"` — every public item and module carries `///`/`//!`
  documentation (exception: `crates/doubleentry` is vendored and has its own
  layout and lints).
- `unwrap_used = "warn"` and `expect_used = "warn"` — a panic must be a
  deliberate, documented decision; a poisoned lock uses a targeted
  `#[expect(clippy::expect_used, reason = "...")]` instead of an `#[allow]`.

Warnings are errors under CI's `-D warnings`, so a new warning fails the PR even
when the local `cargo clippy` run only printed it. Run the same command with
`-D warnings` before pushing.

### Pre-commit hook (optional)

`.githooks/pre-commit` runs `scripts/fmt-check.sh` (the pinned nightly) plus a *scoped* `cargo clippy`
— only the crates the staged files touched, never the whole workspace — so a
docs-only commit costs seconds, not minutes. Manifest changes (`Cargo.toml`,
`Cargo.lock`, `.cargo/config.toml`) still lint everything; `web/`, `docs/`,
`.github/` and other non-Rust paths skip clippy entirely (the frontend gate is
the Dockerfile's `frontend` stage and the CI `frontend` job). Install once per
clone:

```bash
git config core.hooksPath .githooks
```

`git commit --no-verify` bypasses it when needed. `scripts/test-pre-commit.sh`
verifies the path-to-crate mapping with stubbed git output."

### Demo data

`OXSUM_DEMO_SEED=true cargo run -p oxsum-server -- seed` fills a fresh deployment with a believable world and exits
without serving: the `admin@oxsum.local` owner and a second member `dev@oxsum.local` (both under
the password `admin-password`), the member under a committed-spend budget, three API keys —
the owner's full key, a `staging` key capped and allowlisted to the first catalog model, and the
member's `experiments` key — three manual-rail top-ups, a month of settled gateway turns in every
settlement kind (`usage`, `capped`, `unpriced`, all with real `Settlement` descriptions the
verifier recomputes), one hold left in flight for the sweeper, an organization-scoped discount,
and a finalized statement for the month that just ended. The run prints the login credentials and
the owner key's secret — the only place that secret is ever shown.

Seeded request ids are deterministic (`seed-<day>-<n>`) so the demo reads the same everywhere,
and the seeded posting dates spread from today back through the whole previous month, which is
what fills the usage page's rollup and last month's statement. The command is idempotent: a second run finds the
demo account and prints "already seeded" without writing anything. It needs `DATABASE_URL` and
`OXSUM_DEMO_SEED=true` — the explicit opt-in (issue #619) — and it refuses a database that
already holds organizations: demo data is for a fresh install, never a deployment with real
books. With `OXSUM_SECRET_KEY` set and no catalog at all, the seed creates the demo channel
first — `demo`, two priced models (`demo-chat`, `demo-reasoner`) and a deliberately unreachable
upstream URL, because the demo serves seeded history rather than live traffic — so the turns and
the statement are written (issue #330). Without the key it seeds against whatever catalog the
deployment already serves, or writes the world with no turns when the catalog is empty: a channel
row cannot be created without a key to seal its credential.

### The statement lifecycle, end to end

A delinquent organization walks four states, and one test asserts all four
(`the_statement_lifecycle_suspends_and_reinstates` in
`crates/server/tests/statements.rs`). Against a running deployment the same walk
is four operator steps:

1. **Finalize** — `POST /api/v1/admin/statements` with `{"period": "2026-09"}`
   builds the draft for every organization with usage that month; `POST
   /api/v1/admin/statements/{id}/finalize` (`{"idempotencyKey": …}`) freezes the
   terms snapshot, the due date and the proof window. `paymentStatus` is
   `pending`.
2. **Overdue** — the `statements` job flips a pending finalized statement whose
   due date passed to `overdue` (`overdue_at` first-set). Until the job runs, a
   read does the same flip lazily.
3. **Suspended** — the same job's dunning sweep suspends an organization whose
   statement stands past `due_date + dunningGraceDays` (`dunningGraceDays` must
   be set on the organization; the admin PATCH turns it on). `suspendedAt` shows
   on `GET /api/v1/admin/organizations`, the statement moves to `suspended`, and
   `organization.suspended` goes out on the outbound webhooks. A manual
   suspension is never the sweep's to lift.
4. **Reinstated** — a top-up repays the drawn credit line; the reconcile that
   follows every money-in walks the statement to `paid`, and the next sweep
   lifts the suspension it placed.

The test uses zero-day terms and zero-day grace so the four steps fit one run; a
deployment normally leaves days between them. `cargo nextest run -p
oxsum-server --test statements` is the check.

### Test database isolation

Server integration tests that launch `oxsum` use
`crates/server/tests/support/binary.rs`: nextest's runtime executable path
takes precedence over Cargo's build-time path. CI extracts test archives into
a different directory, so `env!("CARGO_BIN_EXE_oxsum")` alone is not portable.
Use `cargo nextest archive` followed by `cargo nextest run --archive-file`
when changing these subprocess tests (issue #684).

Each integration-test suite (`crates/server/tests/*.rs`, `crates/core/tests/*.rs`)
gets its own scratch PostgreSQL database, so suites never share mutable state
and repeated runs cannot accumulate residue (issue #226). The helper lives in
`crates/testkit`: a test file's `url()` calls
`oxsum_testkit::suite_db_url("<file_stem>")`, which on first use creates
`oxsum_test_<slug>_<unix_timestamp>` through an admin connection derived from
`DATABASE_URL` (same server and credentials, `postgres` maintenance database)
and returns a URL pointing at it. When the suite's process exits, the scratch
database is dropped (best-effort; a crashed run may leave a timestamped
`oxsum_test_*` database behind, identifiable by name).

Consequences:

- The shared development database stays clean: test organizations, ledger
  schemas, and test-sealed channels now live in scratch databases that are
  dropped after the run. `cargo run -p oxsum-server` is no longer blocked by
  test fixtures.
- Suites that scan all tenant ledgers (the admin suite is the heavy case) only
  ever see their own organizations, so suite time no longer degrades across runs.
- No new environment variables: `DATABASE_URL` remains the single knob. Without
  it, the database tests skip automatically, as before.

If a run is interrupted and leaves `oxsum_test_*` databases behind, drop them
manually; the reset script below is still available for the development
database itself.

Cleanup is self-healing since #271: before creating its own scratch database a
suite drops every `oxsum_test_*` older than 24 hours off the same admin
connection, so a crashed run's residue is collected by the next run rather than
left to accumulate. The doubleentry Postgres suite does the same for `de_*`
databases on a `DATABASE_URL`/`DOUBLEENTRY_DATABASE_URL` cluster, reading each
v7-uuid name's embedded timestamp so a live neighbour is never matched. For an
immediate cleanup — the cluster already heavy, or a one-off check —
`scripts/drop-test-scratch.sql` drops both families by name pattern:

```bash
psql -d postgres -f scripts/drop-test-scratch.sql
# or against the Compose database directly:
docker exec -i oxsum-postgres-1 psql -U postgres -d postgres -f - < scripts/drop-test-scratch.sql
```

### Test residue in the development database

The section above describes the current per-suite isolation. What follows is
kept for the development database itself: if you ever point the tests at a
database by hand, or write fixtures outside the suites, the residue rules
still apply. Every test organization leaves a user, an organization and a
`ledger_<tenant_id>` schema behind, and channels sealed with the tests' own
key block `cargo run -p oxsum-server` at startup by design (a deployment that
cannot open a stored credential fails fast instead of failing a request
mid-relay).

Run the reset script any time the development database has accumulated
fixtures outside the test suites:

```bash
psql "$DATABASE_URL" -f scripts/reset-dev-database.sql
# or against the Compose database directly:
docker exec -i oxsum-postgres-1 psql -U postgres -d oxsum -f - < scripts/reset-dev-database.sql
```

(`DATABASE_URL` comes from `.env` or the shell, as for the tests.) The script drops every `ledger_*` schema and truncates every mutable `oxsum` table, leaving `oxsum._migrations` — and therefore the schema version — untouched. It is a development-database reset: on a database holding anything worth keeping, dump it first. `TRUNCATE` is what the append-only triggers leave open: they refuse row rewrites, not a table reset, so this does not weaken the audit guarantees.

`crates/doubleentry` Postgres tests run against throwaway containers by default, started by testcontainers — but only when neither `DOUBLEENTRY_DATABASE_URL` nor `DATABASE_URL` is set (in the environment or `.env`; the suite loads `.env` itself since #277). With the local `.env` carrying `DATABASE_URL` this path is already skipped in day-to-day work. When the container path does run, an interrupted run or a Docker restart can leave the container behind — Ryuk's removal is best-effort — so `scripts/clean-testcontainers.sh` sweeps `org.testcontainers.managed-by` containers older than an hour (`MIN_AGE_MINUTES` to tighten, `--drop` to act; issue #277). The suite's deferred-sequencing tests still prefer a dedicated container even when `DATABASE_URL` points at a shared cluster — a neighbour's open transaction stalls the sequencing watermark and the conformance suite can read wrong, not just slow — so the shared-cluster path is the Docker-less fallback, never the default when Docker answers. External-mode residue is collected by the same external sweep the workspace suites use (issue #271): `scripts/drop-test-scratch.sql` drops `de_*` databases by name pattern, and the CI step runs it before every suite — no in-process sweep serializes teardown.

### Debug info in dev builds

`.cargo/config.toml` sets `[profile.dev] debug = 1` — line tables, not full debuginfo. This is not a speed preference: at full `debug = 2` the `liboxsum_server` rlib grows past 4 GB, and the COFF archive index cannot address members beyond 4 GiB, so `cargo build -p oxsum-server` fails at link time with a hundred-odd unresolved oxsum_server symbols (issue #176). Line tables keep file:line in backtraces while staying well under the limit. Anyone who needs full debuginfo for a session can override per invocation — `CARGO_PROFILE_DEV_DEBUG=2 cargo build ...` — knowing the resulting rlib may not link on MSVC.

### x402 facilitator sidecar

The real x402 rail (issue #282) delegates verify/settle to an [x402-rs facilitator](https://github.com/x402-rs/x402-rs) — a standalone service, deliberately not an in-process crate, so chain RPC connections and signer keys stay out of the billing binary. The commented `facilitator` service in `compose.yaml` runs `ghcr.io/x402-rs/x402-facilitator`; it wants a `facilitator.config.json` at the repo root naming chains, signer keys and RPC endpoints:

```json
{
  "chains": {
    "eip155:84532": {
      "eip1559": true,
      "signers": ["$FACILITATOR_PRIVATE_KEY"],
      "rpc": [{ "http": "https://sepolia.base.org" }]
    }
  },
  "schemes": [{ "scheme": "exact", "chains": ["eip155:84532"] }]
}
```

`OXSUM_X402_NETWORK` uses the V1 network name (`base-sepolia` for the CAIP-2 `eip155:84532` chain above); `OXSUM_X402_ASSET` is the USDC contract on that chain, `OXSUM_X402_PAY_TO` the deployment's receiving address. Base Sepolia USDC is free from the Circle faucet, so a full real-rail exercise costs nothing. Without any of this the stand-in verifier (`OXSUM_X402_VERIFYING_KEY`) still covers local development and every test suite.

A settle the facilitator refuses — `success: false` on `/settle`, or an unreachable sidecar — leaves the deposit `confirmed` and records why on `deposits.meta.settleRefusal`: the facilitator's `error_reason` when it answered, a local transport note when it did not. The row's `payment_ref` (the signed `nonce`) keeps the claim, so a retry under the same authorization re-attempts the settle rather than double-crediting; the caller still sees a plain `FORBIDDEN`.

#### Real x402 rail exercise

A full end-to-end run — a real signed authorization verified and settled on-chain — is a cheap recipe. `scripts/x402_sign.py` produces the wire `payment` string for `POST /api/v1/topups/x402`; the hosted facilitator `https://facilitator.x402.rs` already serves `base-sepolia` under the V1 `exact` scheme, so no local sidecar is needed:

```bash
# .env carries the rail's four vars:
#   OXSUM_X402_FACILITATOR_URL=https://facilitator.x402.rs
#   OXSUM_X402_NETWORK=base-sepolia
#   OXSUM_X402_ASSET=0x036CbD53842c5426634e7929541eC2318f3dCF7e   # USDC on Base Sepolia
#   OXSUM_X402_PAY_TO=<the deployment's receiving address>

cargo run -p oxsum-server &

WIRE=$(PAYER_KEY=<the payer's secp256k1 key> \
       PAY_TO=$OXSUM_X402_PAY_TO \
       AMOUNT_MINOR=5000000 \
       python3 scripts/x402_sign.py)

curl -X POST http://127.0.0.1:3000/api/v1/topups/x402 \
  -H "Authorization: Bearer <org api key>" \
  -H "content-type: application/json" \
  -d "{\"payment\":\"$WIRE\",\"amountMinor\":5000000}"
# → {"data":{"entryId":…,"logIndex":…,"isNew":true}} and the deposit row
# carries the on-chain tx hash on `facilitator_tx_hash`.
```

A successful run was exercised against Base Sepolia on 2026-10-25 (issue #372) — transaction `0xe25f9c399dbb2f620d236246ac382b8d7cf9760bc6ec4d184442a7bff0cc4038` on `https://sepolia.basescan.org` — which is historical **testnet** evidence in [the rail matrix](rails.md), not a live-money certification. The wallet needs only faucet USDC; the facilitator fronts the gas.

Two wire details the script exists to absorb, both of which cost a debugging round-trip during bring-up:

- The USDC Base Sepolia contract's EIP-712 `name` is the string `"USDC"`, **not** `"USD Coin"` — Circle renamed between FiatToken v1 and v2, and the domain separator commits to whichever string the contract signed under. A signer using the wrong name produces a valid-looking signature that `ecrecover` rejects at settle time with `FiatTokenV2: invalid signature`.
- `coincurve`'s recoverable signature carries the recovery id `v ∈ {0, 1}`; Ethereum's `ecrecover` and the x402 wire want `v ∈ {27, 28}`. The script shifts it — leaving it unshifted surfaces as `ECRecover: invalid signature 'v' value` from the facilitator.

### Real Airwallex rail exercise

This historical exercise combined an API intent with a locally synthesized
signed success event. It did not establish a real payer checkout or automatic
provider delivery. The later [T31 follow-up](roadmap/self-hosted-v1/evidence/T31-followup.md)
records a 403 credential rejection. Use the current merchant/version contract
and a usable processor sandbox account for new certification; the following
recipe is retained only to explain the earlier fixture, not as a current setup
command. Its credential and callback authority predates current personal-session
and versioned-merchant requirements.

The historical fixture used these variables:

```
OXSUM_AIRWALLEX_CLIENT_ID=…
OXSUM_AIRWALLEX_API_KEY=…
OXSUM_AIRWALLEX_API_URL=https://api-demo.airwallex.com       # demo, not prod
OXSUM_AIRWALLEX_CHECKOUT_URL=https://checkout-demo.airwallex.com
OXSUM_AIRWALLEX_WEBHOOK_SECRET=whsec_…
```

The historical fixture transcript (do not copy into an ordinary deployment):

```bash
cargo run -p oxsum-server &

# 1) Open a real intent — the server logs in, calls
#    /pa/payment_intents/create and answers a real clientSecret.
curl -X POST http://127.0.0.1:3000/api/v1/topups/airwallex \
  -H "Authorization: Bearer <org api key>" \
  -H "content-type: application/json" \
  -d '{"idempotencyKey":"awx-sandbox-1","amountMinor":500}'
# → {"data":{"checkoutUrl":"…","clientSecret":"…","depositId":"…","intentId":"int_…"}}

# 2) Simulate the webhook the hosted page would have triggered, signed
#    under the deployment's webhook secret. scripts/airwallex_webhook.py
#    carries the recipe; the deposit flips pending → credited.
WEBHOOK_SECRET=$OXSUM_AIRWALLEX_WEBHOOK_SECRET \
INTENT_ID=<the intent_id from step 1> \
MERCHANT_ORDER_ID=<the depositId> \
AMOUNT_MAJOR=5.00 \
python3 scripts/airwallex_webhook.py
# → 200, and the deposit row flips to credited.
```

Two wire details this exercise surfaced during bring-up (issue #374, fixed by #375/#376):

- `api-demo.airwallex.com` produces TLS 1.3 records rustls cannot decrypt — the rail's HTTP client is capped at `max_tls_version(TLS_1_2)` for this endpoint only; the rest of the workspace stays on 1.3.
- The demo login endpoint requires `Content-Length` even on a bodyless credential POST — without it the answer is `411 Length Required` with an HTML body that decodes as a "response unreadable" failure rather than the real status.

### Hyperswitch sidecar

Use the [isolated pinned test stack](../infra/hyperswitch/README.md), issue #757.
It owns a random Compose project, private generated keys and named PostgreSQL
17/Redis/mail volumes; no database/admin ports are published beyond loopback.
Source, image digests and the migration tool are locked in
`infra/hyperswitch/versions.json`. Do not run the raw Compose file without its
private generated env/project identity.

```bash
./infra/hyperswitch/setup.sh
python3 infra/hyperswitch/stack.py check
python3 infra/hyperswitch/stack.py up
python3 infra/hyperswitch/merchant.py provision
python3 infra/hyperswitch/merchant.py inspect
python3 -m unittest discover -s infra/hyperswitch -p 'test_*.py'
# Pass the exact binary already built by Cargo:
python3 infra/hyperswitch/smoke.py --binary /absolute/path/to/oxsum
python3 infra/hyperswitch/stack.py stop
```

Local addresses: router `127.0.0.1:18080`, SDK `127.0.0.1:19050`, private
Control Center `127.0.0.1:19000`, Mailpit UI `127.0.0.1:8025`, SMTP
`127.0.0.1:1025`. No shared demo login is created. `merchant.json` under ignored
`.runtime/` retains the fresh API/publishable/signing credentials; never print
or commit it. `up` recreates services and preserves their volumes; do not
remove encryption state while retaining the database.

The `paypal_test` connector is a local simulation, not PayPal or Stripe sandbox
certification. The smoke uses an isolated oxsum database and locally signed
callback fixtures, never a real PSP or model. HTTPS browser checkout and
switch-delivered callbacks require the documented reachable public SDK/API/
callback topology. Set both SDK `ENV_BACKEND_URL` and `ENV_SDK_URL`; do not
serve public payers a localhost API or bypass destination/TLS policy. Use
`public_proxy.py` to restrict temporary tunnel paths, and keep administrative
interfaces private. Genuine Stripe sandbox acceptance requires a separately
supplied Stripe test-account credential. See the stack README for exact
configuration, identity persistence and task-only cleanup.

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
| `OXSUM_SIGNUP` | Legacy parsed `invite`/`open` value retained for configuration compatibility. It never overrides business policy. Fresh/upgraded installations are invite-only until the initialized owner sets `open` through `/api/v1/business/registration`. |
| `OXSUM_SIGNUP_BONUS_MINOR` | Optional; credits a new organization's wallet starts with, in minor units, default 0. Booked at self-registration as an adjustment carrying "signup bonus" as its reason; an invited account joins an existing organization and is granted nothing |
| `OXSUM_DB_MAX_CONNECTIONS` | Optional; size of the one pool every tenant shares (default 20). Each connection is a PostgreSQL backend process, so this is the process's whole budget rather than a per-tenant allowance. Stay below the database's own `max_connections` (PostgreSQL's default is 100); size it at roughly twice the expected number of concurrent gateway requests, floor 10 — saturation answers 429 `POOL_BUSY` with `Retry-After`, never a 500 |
| `OXSUM_ADDR` | Optional listen address; defaults to `127.0.0.1:3000` |
| `OXSUM_UPSTREAM_BASE_URL`, `OXSUM_UPSTREAM_API_KEY`, `OXSUM_MODELS` | The bootstrap channel for a database that has none, set together or not at all. With none of them a fresh deployment serves the wallet only. `OXSUM_MODELS` is JSON: each model maps to `inputPricePerMillion`, `outputPricePerMillion` and `maxOutputTokens`, in minor units per million tokens. `maxOutputTokens` is the ceiling for a request that asks for none and the freeze is computed from it, so it must be the model's real limit rather than a safe guess. A malformed value, or a base URL that is not an http(s) URL, refuses to boot rather than serving a price it had to guess |
| `OXSUM_SECRET_KEY` | 32 bytes, base64, sealing the upstream credentials stored in `oxsum.channel_credentials` (a channel's key pool, issue #444) and the webhook signing secrets in `oxsum.webhook_endpoints` (AES-256-GCM). Required as soon as a channel exists — including one seeded from the environment, which is why setting `OXSUM_MODELS` means setting this too — and for the webhook sender to run at all: without it `POST /api/v1/webhooks` is refused and no worker is spawned. The server refuses to start when it cannot open a stored credential, rather than failing a request that has already been relayed. Generate one with `openssl rand -base64 32`. Losing it means re-entering every upstream credential and rotating every webhook secret |
| `OXSUM_HEAD_SIGNING_KEY` | Optional; 32 bytes, base64, the seed of the Ed25519 key the operator signs tree heads and price-catalog checkpoints with (`openssl rand -base64 32` generates one). Unset means the `/api/v1/log` endpoints answer 503 and proof bundles carry no `price` link; the wallet is served without them. A bad value refuses to boot. Rotating it is restarting with a new seed — heads and price checkpoints are signed on demand, so nothing is stored to migrate; old signatures stop verifying, which is what a rotation means |
| `OXSUM_WITNESS_URLS` | Optional; comma-separated base URLs of external C2SP `tlog-witness` endpoints the hourly `witness-checkpoints` job publishes each organization's signed head to (`/add-checkpoint`; issue #231). Requires `OXSUM_HEAD_SIGNING_KEY` — the pass only schedules when both exist. The cosigned notes land in `oxsum.witness_cosignatures` and are served at `GET /api/v1/log/checkpoint`; unset means no anchoring and the endpoint answers `data: null` |
| `OXSUM_WITNESS_KEYS` | Optional; the deployment's pinned witness keyset, a JSON array of `{"url","keyName","publicKey","keyHash"}` — one entry per `OXSUM_WITNESS_URLS` witness, where `keyHash` is the cosignature/v1 selector `SHA-256(keyName ‖ '\n' ‖ 0x04 ‖ publicKey)`'s first four bytes (issue #259). Served on `GET /api/v1/log/checkpoint` as `witnessKeys` so a reader can verify each cosignature line itself. A `url` that is not in `OXSUM_WITNESS_URLS`, a bad key name, a non-Ed25519 `publicKey`, or a `keyHash` that does not match the recomputed selector refuses to boot. Unset serves `witnessKeys: []`; a verifier holding its own pinset can still check the notes |
| `OXSUM_WITNESS_SIGNING_KEY` | Optional; 32 bytes, base64, the seed of the Ed25519 key this deployment's own tlog-witness cosigns foreign checkpoints under at `POST /add-checkpoint` (issue #260). Set together with `OXSUM_WITNESS_TRUST`, a subset is a startup error; unset means `/add-checkpoint` is not mounted and answers 404 like every unknown path |
| `OXSUM_WITNESS_KEY_NAME` | Optional; the name this witness's `cosignature/v1` lines carry, default `oxsum/witness` — the name a submitter's verify step looks the key up by. Only meaningful with `OXSUM_WITNESS_SIGNING_KEY` |
| `OXSUM_WITNESS_TRUST` | Required once `OXSUM_WITNESS_SIGNING_KEY` is set; comma-separated `origin=size:root_b64` pins — every foreign log this witness vouches for and the head it starts from, learned out of band from that log's operator. There is no trust-on-first-use: a submission for an unpinned origin is a 404, and a witness that accepted whatever arrived first could already be signing a fork. Pinning `size=0` onboards a brand-new log deliberately — the first non-empty head it is offered is taken on trust (every log extends the empty tree, so nothing earlier is provable) and from then on it is a real witness. A restart re-pinning a root the stored head contradicts at the same size refuses to boot rather than roll back onto a fork |
| `OXSUM_ADMIN_TOKEN` | Optional; the platform admin's token for `/api/v1/admin`, at least 16 characters. It is not an API key: it changes channels and prices and belongs to whoever deploys oxsum. Unset means no admin surface (the routes answer 401). Compared in constant time, so a long random value is the right one; rotate it by restarting with a new value |
| `OXSUM_UPSTREAM_NAME` | Optional; the bootstrap channel's name, reported as `owned_by` in `GET /v1/models`. Defaults to `upstream` |
| `OXSUM_HOLD_TIMEOUT` | Optional; how long a gateway hold may sit unsettled before the background sweeper releases it at 0 with settlement kind `swept` (recorded as an anomaly). A number of seconds with an optional `s`/`m`/`h` suffix; defaults to `30m`. Validated at startup: it must parse and be at least 60s. It must exceed the longest possible single request — anything older is abandoned by definition, so a hold that is old but whose request is still streaming cannot exist under a correct configuration. The sweeper passes every 60s |
| `OXSUM_SESSION_COOKIE_SECURE` | Optional; whether the session cookie carries the `Secure` attribute: `true` or `false`, default `false`. A deployment behind TLS must set it to `true`, or browsers will not send the cookie back over https; `false` is what makes local http development work. Anything else refuses to start |
| `OXSUM_RATE_LIMIT_BACKEND` | Optional; where a key's `requestsPerMinute` window is counted (issue #498): `memory` (the default) keeps it in the process — all a single binary needs; `postgres` counts it in `oxsum.rate_windows` so several instances behind a load balancer share one window instead of each admitting the full cap. Anything else refuses to start |
| `OXSUM_SMTP_URL` | Optional; the lettre connection string for outbound mail — `smtps://user:pass@host` for implicit TLS, `smtp://user:pass@host` for STARTTLS-when-offered. The credentials live in the URL, so keep it out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_MAIL_FROM` | Optional; the `From:` mailbox of the verification and reset mails, e.g. `oxsum <noreply@example.com>` |
| `OXSUM_PUBLIC_URL` | Optional on its own, required by the mailer and OAuth client; the deployment's public base URL the mailed links, the OAuth callback and the device grant's `verificationUri` are built on, e.g. `https://wallet.example.com` — no trailing path |
| `OXSUM_GITHUB_CLIENT_ID` | Optional; the GitHub OAuth app's client id — set together with `OXSUM_GITHUB_CLIENT_SECRET` and `OXSUM_PUBLIC_URL`. The callback the GitHub app registers is `<public-url>/api/v1/auth/oauth/github/callback` |
| `OXSUM_GITHUB_CLIENT_SECRET` | Optional; the OAuth app's client secret — keep it out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_GITHUB_WEB_URL` | Tests only; the provider's web origin the authorize redirect and token exchange go to, default `https://github.com` — a stub stands there |
| `OXSUM_GITHUB_API_URL` | Tests only; the provider's API origin, default `https://api.github.com` |
| `OXSUM_OIDC_PROVIDERS` | Optional; the enterprise IdPs the login federates to (issue #504) — a JSON array `[{"name","issuer","clientId","clientSecret"}]`, one object per provider (`name` is the route segment: the login runs at `/api/v1/auth/oauth/oidc:{name}` and its callback registers as `<public-url>/api/v1/auth/oauth/oidc:{name}/callback`). Non-empty requires `OXSUM_PUBLIC_URL`; malformed JSON or a missing field refuses to boot. Unset or `[]`, no IdP buttons draw and the routes 404 |
| `OXSUM_TURNSTILE_SITE_KEY` | Optional; the Cloudflare Turnstile site key the register page hands the widget — set together with `OXSUM_TURNSTILE_SECRET_KEY`, a subset is a startup error. Public by design: `auth/methods` publishes it |
| `OXSUM_TURNSTILE_SECRET_KEY` | Optional; the secret `siteverify` authenticates with — keep it out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_TURNSTILE_VERIFY_URL` | Tests only; the siteverify endpoint, default `https://challenges.cloudflare.com/turnstile/v0/siteverify` — a stub stands there |
| `OXSUM_BUSINESS_NAME` | Optional unsaved seller-name default. An initialized business uses its accepted setup name; an explicitly saved Business profile overrides all branding variables. At most 80 characters; unset defaults to `oxsum` |
| `OXSUM_BRAND_LOGO_URL` | Optional unsaved logo default; absolute HTTP(S), no user info or controls, at most 2048 bytes. A saved profile (including null) takes precedence. No ledger/proof effect |
| `OXSUM_BRAND_ACCENT_COLOR` | Optional unsaved `#rrggbb` accent default; a saved profile takes precedence. The portal maps it onto the `primary` design token — the action hue the whole component layer follows. A malformed value refuses to boot rather than breaking a CSS custom property on every page; unset keeps the palette's stock accent |
| `OXSUM_STRIPE_SECRET_KEY` | Optional; the Stripe API key the checkout-session POST authenticates with (`sk_live_…`/`sk_test_…`) — set together with `OXSUM_STRIPE_WEBHOOK_SECRET` and `OXSUM_PUBLIC_URL`, a subset is a startup error. Unset, the rail is absent: `POST /api/v1/topups/stripe` answers 503 and `/api/v1/webhooks/stripe` 404. Keep it out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_STRIPE_WEBHOOK_SECRET` | Optional; the `whsec_…` secret that verifies the `Stripe-Signature` header on `POST /api/v1/webhooks/stripe` — the endpoint Stripe's dashboard registers is `<public-url>/api/v1/webhooks/stripe` |
| `OXSUM_STRIPE_API_URL` | Tests only; the Stripe API origin the checkout-session POST goes to, default `https://api.stripe.com` — `crates/server/tests/stripe.rs` stands a stub there |
| `OXSUM_AIRWALLEX_CLIENT_ID` + `OXSUM_AIRWALLEX_API_KEY` + `OXSUM_AIRWALLEX_WEBHOOK_SECRET` | Optional as a set with `OXSUM_PUBLIC_URL`; all four or none — a subset is a startup error (issue #285). The web-app Developer key pair is exchanged for a short-lived bearer token at `/api/v1/authentication/login` when `POST /api/v1/topups/airwallex` creates a PaymentIntent; the webhook secret verifies the `x-signature` header (hex HMAC-SHA256 of `x-timestamp + body`) on `POST /api/v1/webhooks/airwallex` — the endpoint Airwallex's web app registers is `<public-url>/api/v1/webhooks/airwallex`. Unset, the rail is absent: the intent answers 503, the webhook 404. Keep the secrets out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_AIRWALLEX_API_URL`, `OXSUM_AIRWALLEX_CHECKOUT_URL` | Tests only; the API and hosted-payment-page origins the intent POST and browser redirect go to, defaults `https://api.airwallex.com` and `https://checkout.airwallex.com` — `crates/server/tests/airwallex.rs` stands a stub there |
| `OXSUM_HYPERSWITCH_API_KEY` + `OXSUM_HYPERSWITCH_WEBHOOK_SECRET` + `OXSUM_HYPERSWITCH_PUBLISHABLE_KEY` | Optional as a set with `OXSUM_PUBLIC_URL`; all four or none — a subset is a startup error (issue #319). The API key authenticates `POST /payments` when `POST /api/v1/topups/hyperswitch` opens a deposit; the webhook secret is the business profile's `payment_response_hash_key`, verifying `X-Webhook-Signature-512` (hex HMAC-SHA512 over the raw body) on `/api/v1/webhooks/hyperswitch`; the publishable key is what the browser SDK initializes with. Unset, the rail is absent: the intent answers 503, the webhook 404. Keep the first two out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_HYPERSWITCH_BASE_URL` | The switch origin, default `https://sandbox.hyperswitch.io`; a self-hosted deployment points it at its own app server and `crates/server/tests/hyperswitch.rs` stands a stub there |
| `OXSUM_HYPERSWITCH_SDK_URL` | The `HyperLoader.js` origin the top-up page mounts Unified Checkout from (issue #320), default `https://beta.hyperswitch.io/v1` — the hosted sandbox's SDK bundle. A self-hosted deployment points it at its hyperswitch-web service (`http://localhost:9050` for the local sidecar). It travels to the browser in the intent's `sdkUrl` |
| `OXSUM_<RAIL>_MERCHANT_ACCOUNT_ID` + `OXSUM_<RAIL>_MERCHANT_MODE` | Optional; both together or neither, per rail (`STRIPE`, `AIRWALLEX`, `HYPERSWITCH`) — and each requires that rail's own credential set to be present (issue #649). Setting them bootstraps one explicit merchant under the business on first start, attributes existing single-rail deposits to it, and routes the rail's checkout plus its `/api/v1/webhooks/<rail>` callback through the scoped intake; it runs once per rail and is never overwritten by later environment drift. `ACCOUNT_ID` is the provider account (Stripe `acct_…`, Airwallex/Hyperswitch merchant id); `MODE` is `test` or `live`. Without it the rail keeps its unscoped legacy behavior but cannot open merchant-bound payments |
| `OXSUM_<RAIL>_MERCHANT_DESTINATION_POLICY` | Optional; a `ProviderDestinationPolicy` JSON object (e.g. `{"network":"internal","allowedIps":[…]}`) scoping where the bootstrapped merchant's provider calls may go — the public default applies when unset |
| `OXSUM_X402_FACILITATOR_URL` + `OXSUM_X402_NETWORK` + `OXSUM_X402_ASSET` + `OXSUM_X402_PAY_TO` | Optional as a set; all four or none (a subset is a startup error). The real x402 rail (issue #282): `POST /api/v1/topups/x402` delegates verify/settle to an x402-rs facilitator sidecar (`OXSUM_X402_FACILITATOR_URL`, e.g. `http://localhost:8080` or the hosted `https://facilitator.x402.rs`) over its V1 HTTP API. `NETWORK` is the V1 chain name (`base-sepolia` for tests, `base` for production), `ASSET` the accepted token contract (USDC on base-sepolia: `0x036CbD53842c5426634e7929541eC2318f3dCF7e`), `PAY_TO` the deployment's receiving address. The settle answer's chain hash lands on `deposits.facilitator_tx_hash` |
| `OXSUM_X402_VERIFYING_KEY` | Optional; the hex Ed25519 public key the x402 stand-in rail pins as its trusted payer (issue #264) — the test-scheme fallback read only when the facilitator set is entirely absent. Unset both, the rail is absent and the endpoint answers 503; a malformed value is a startup error. The stand-in is documented in docs/decisions.md |
| `OXSUM_E2E_UPSTREAM_BASE_URL`, `OXSUM_E2E_UPSTREAM_API_KEY`, `OXSUM_E2E_MODEL` | Tests only; all three together opt `crates/server/tests/real_upstream.rs` into reaching a real provider — the base URL, the provider's key, and the model name it serves. Unset, the test skips before touching the network; `cargo test` never sets them and neither does CI. Keep the key in the shell environment, never in `.env` or any tracked file — the test seals it into `oxsum.channels` like a real deployment |
| `OXSUM_UPSTREAM_PRICE_SYNC` | Optional; `true` schedules the daily `sync-upstream-prices` job that probes each channel's `GET {baseUrl}/api/v1/pricing` and opens upstream-price-drift rows where the stored `upstream` rates disagree (issue #222). The job also needs `OXSUM_SECRET_KEY` to unseal channel credentials — it is only enqueued when both exist. Default unset: the job never runs, and channels pointing at non-oxsum upstreams are better left unprobed anyway — only an oxsum-compatible catalog answers that path |
| `OXSUM_UPSTREAM_BILL_SYNC` | Optional; `true` schedules the daily `upstream-bill-sync` job that pulls each channel's provider-billed cost for the day just closed and records it beside the day's summed `upstream_cost_minor` in `oxsum.upstream_bills`, flagging drift over the alert threshold as `anomaly.detected` (issue #283). The job also needs `OXSUM_SECRET_KEY` to unseal each channel's `bill_key_sealed` — the admin billing credential an admin channel write attaches with `billKey` — and is only enqueued when both exist. Enabling it without the sealing key is a startup error, the same shape `OXSUM_UPSTREAM_PRICE_SYNC` takes. Default unset: the job never runs and `providerBilledMinor` on the margin view stays null |
| `OXSUM_RETENTION_PROVIDER_RAW_DAYS` | Optional; days a usage row keeps `usage_details.provider_raw` before the daily `retention` job nulls it — the privacy window, since the raw payload may carry prompt fragments. Default 7; the normalized columns are kept forever |
| `OXSUM_RETENTION_DELIVERIES_DAYS` | Optional; days a delivered or failed `webhook_deliveries` row is kept. Default 30 |
| `OXSUM_RETENTION_JOBS_DAYS` | Optional; days a finished `oxsum.jobs` row is kept — `dead` rows included, so the window is also how long `jobs_dead` drift evidence survives. Default 90 |
| `OXSUM_RETENTION_REQUEST_LOG_DAYS` | Optional; days a `oxsum.request_log` row is kept before the daily `retention` job deletes it (issue #505). Default 30. The ledger entries the rows point at are untouched — only the operational search surface ages out |
| `OXSUM_EVIDENCE_UPSTREAM_COST` | Optional; `true` or `false`, default `false` (issue #616). Whether minted evidence bundles carry the platform's upstream-cost figures — a usage artifact's `upstreamCostMinor`/`real_attempt` is the platform's own supplier spend, not the customer's bill, so the default export redacts it. The customer-charged `chargedMinor` and each row's `costBasis` spelling always export; `scope.upstreamCostIncluded` in the bundle records which case the mint ran under |
| `RUST_LOG` | Optional `tracing-subscriber` filter; defaults to `info` — applies to exported traces too when `OXSUM_OTLP_ENDPOINT` is set |
| `OXSUM_OTLP_ENDPOINT` | Optional; the OTLP collector's HTTP endpoint, e.g. `http://localhost:4318` — the exporter appends `/v1/traces` (issue #224). Unset, tracing stays strictly local. Spans carry only the `gen_ai.*` semantic-convention vocabulary — operation, system, model, token counts — never a request id, organization, or key |
| `OXSUM_DISPLAY_CURRENCY` | Optional; the deployment-wide display unit an organization without its own setting renders credits in — ISO 4217, e.g. `USD`. Set together with `OXSUM_DISPLAY_RATE_PER_CREDIT`, a subset is a startup error. An organization's own `PATCH /api/v1/org` setting wins over this default (issue #257) |
| `OXSUM_DISPLAY_RATE_PER_CREDIT` | Optional; the decimal literal one credit displays as in `OXSUM_DISPLAY_CURRENCY`, e.g. `0.0014`. Stored and wired as text — money never goes through a float |
| `OXSUM_DISPLAY_DECIMALS` | Optional; how many decimals the fiat rendering rounds half-up to (0–6). Default 2 |
| `OXSUM_RAILS` | Optional operator declaration (issue #619); comma-separated deposit rails the deployment intends — `stripe`, `airwallex`, `hyperswitch`, `x402`. Set and the boot refuses either half of a mismatch: a listed rail whose `OXSUM_<RAIL>_*` set is incomplete, or a configured rail the list omits. Unset declares nothing |
| `OXSUM_CUSTOMER_SELF_REGISTER` | Optional operator declaration (issue #619); `true`/`1` requires the mailer set — a deployment that could never send verification mail refuses to boot. Unset or `false` declares nothing; the business's registration policy is what opens signup |
| `OXSUM_DEMO_SEED` | Optional; `true`/`1` is the explicit opt-in `oxsum seed` requires (issue #619). Unset, the subcommand refuses — a fresh deployment never carries demo data. The seed still refuses a database that already holds organizations |

#### A witness sidecar beside the deployment

A witness process uses its own database and `OXSUM_WITNESS_SIGNING_KEY`,
listens on its own address and accepts `POST /add-checkpoint`.
`OXSUM_WITNESS_TRUST` pins expected ledger origins and initial heads; `size=0`
explicitly trusts the first received head. The deployment uses
`OXSUM_WITNESS_URLS` and `OXSUM_WITNESS_KEYS` to publish and check cosignatures.

Running a second process on the same operator's host exercises the protocol and
continuity checks. It does not establish organizational independence. Customers
must choose independently trusted witness keys if they need that corroboration;
no current public-demo witness deployment is certified by this guide.

The mailer trio is all three or none: a subset refuses to boot, because a half-configured mailer would only fail at send time inside a request. Unset, the deployment sends no email — registration answers `verificationSent: false`, forgot-password still answers its indistinguishable 200, and `POST /api/v1/auth/verify/request` alone reports the absence with 503. The GitHub OAuth pair follows the same rule — a client id or secret without `OXSUM_PUBLIC_URL` refuses to boot, and unset the login page draws no GitHub button and the OAuth endpoints answer 404. So does the Stripe trio (`OXSUM_STRIPE_SECRET_KEY`, `OXSUM_STRIPE_WEBHOOK_SECRET`, `OXSUM_PUBLIC_URL`): a subset refuses to boot, and unset, the intent endpoint 503s and the inbound webhook 404s.

A deployment's channels and their prices live in the database, and the environment only seeds a
database that has none. Prices are append-only versions: `POST /api/v1/admin/channels/{name}/prices`
adds one, the version in force when a request starts is the one that prices it, and the settlement
names that version, so an old bill stays checkable however often the price changes. To try the loop
against a real provider, set the variables below and point an OpenAI SDK at
`http://127.0.0.1:3000/v1`:

```bash
OXSUM_UPSTREAM_BASE_URL=https://api.deepseek.com/v1 \
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
refusal, a 200 that is not JSON and a stream that never finishes are all ordinary cases; the
failover tests stand a second upstream beside the first and script the attempts each took. They still
need `DATABASE_URL`, because every turn freezes and settles real credit. Each test world gives its
channel and models a random suffix: a shared model name would otherwise make two worlds' routes
candidates for the same request, which is failover working.

There is no shared API token any more: every request carries an organization's API key, created
by registration or by `POST /api/v1/org/keys`. The plaintext secret is returned exactly once.

The process's own telemetry is scraped at `GET /metrics` (Prometheus exposition format), gated
by the same `OXSUM_ADMIN_TOKEN` as the admin surface — a scrape config carries it as
`bearer_token`. HTTP request counts and durations by route, the gateway's holds, settlements,
charges and tokens, upstream response latency, rate-limit and idempotency outcomes, the
sweeper's releases and dead-letter transitions, and scrape-time gauges for open and
dead-lettered holds and the connection pool are all under the `oxsum_` prefix:

```bash
curl -H "authorization: Bearer $OXSUM_ADMIN_TOKEN" http://127.0.0.1:3000/metrics
```

Traces can leave the process too: with `OXSUM_OTLP_ENDPOINT` set, every gateway turn is
exported to that OTLP/HTTP collector as a `gen_ai.turn` span carrying the GenAI semantic
conventions — `gen_ai.operation.name` (`chat`/`embeddings`/`rerank`), `gen_ai.system`
(`openai`/`anthropic`), `gen_ai.request.model`, and `gen_ai.usage.input_tokens` /
`gen_ai.usage.output_tokens` filled in once settlement knows them. Two span events mark
the phase boundaries — `hold taken` (`oxsum.hold.freeze_minor`) when the ledger hold
lands, `settle written` (`oxsum.charged_minor`, `settlement.kind`) when the write
returns — so the span's own start and end bracket admission, the relay, and the settle
without a child span. The span is batched and shipped on the application's own tokio
runtime, and `RUST_LOG`'s env filter applies to it the same way it applies to logs.
Point a collector at it:

```bash
OXSUM_OTLP_ENDPOINT=http://localhost:4318 cargo run -p oxsum-server
```

oxsum's own tables are migrated at startup, in the same process that serves requests: `Db::migrate`
creates the `oxsum` schema and applies the files under `crates/core/migrations/` that have not run
yet, each in one transaction together with its row in `oxsum._migrations`, under a database-wide
advisory lock so two starting processes cannot race. A failed migration refuses to boot.
`oxsum --migrate-only` runs the same pass and exits — the deploy `oxsum-migrate` step and the
upgrade runbook's deliberate migrate. Ledger schemas are untouched by either: doubleentry's
`migrate` creates `ledger_<tenant_id>` on first use.

## Commands

| Purpose | Command |
| --- | --- |
| Start the database | `docker compose up -d` |
| Start the server | `cargo run -p oxsum-server` |
| Migrate only | `cargo run -p oxsum-server -- --migrate-only` (applies pending migrations, prints the schema level, exits — the deploy `oxsum-migrate` step) |
| Validate the environment | `cargo run -p oxsum-server -- --check-config` (runs the startup config parse and prints a JSON verdict without touching the database — issue #619) |
| Collect sanitized deployment diagnostics | `python3 scripts/operator-diagnostics.py --url http://127.0.0.1:3000 --token-file /private/operator-token --env-file deploy/.env --output /private/diagnostics.json` — private inputs, new output; optional token/env flags; fixed counts/presence only ([guide](operator-diagnostics.md), #819) |
| Seed the demo world | `OXSUM_DEMO_SEED=true cargo run -p oxsum-server -- seed` (populates, prints the credentials, exits — see "Demo data") |
| All tests | `cargo nextest run --workspace` — runs the full suite locally (loads `DATABASE_URL` from `.env` or the shell; `cargo test --workspace` is the fallback and covers doctests). Worth it for cross-crate changes; otherwise CI runs the same coverage on every PR — see "CI" below |
| Focused while iterating | `cargo nextest run -p oxsum-<crate>` — only the crates the change touched; the local pre-push gate is fmt + clippy + these focused runs, with CI holding the full suite |
| Run the end-to-end demo | `python3 demo/demo.py` (Python dependencies in `demo/requirements.txt`, Node 20.16+, WASM toolchain and PostgreSQL CREATEDB role; isolated database, stub SDK call, clean offline consumer; see README) |
| All tests | `cargo nextest run --workspace` — runs the full suite locally (loads `DATABASE_URL` from `.env` or the shell; `cargo test --workspace` is the fallback and covers doctests). Worth it for cross-crate changes; PR CI runs the fast tier; heavy integration/engine evidence requires the full workflow — see "CI" below |
| Focused while iterating | `cargo nextest run -p oxsum-<crate>` — only the crates the change touched; the local pre-push gate is fmt + clippy + these focused runs, with full workspace coverage additionally required for cross-crate changes |
| Demo replacement | Follow #728 / PR #751 after integration; the baseline script has stale authority/isolation assumptions and must not run against an ordinary database |
| Generative sequences, quick run | `PROPTEST_CASES=50 cargo test -p oxsum-core --test generative` |
| Real-provider end-to-end check | `cargo test -p oxsum-server --test real_upstream` (needs env vars — see "Real-provider smoke check") |
| doubleentry's Postgres tests | `cargo test -p doubleentry --features postgres --test postgres` (spins up its own container via testcontainers; `DOUBLEENTRY_DATABASE_URL`/`DATABASE_URL` set → that server instead, no Docker) |
| Format | `scripts/fmt.sh` (pinned nightly; write) |
| Check formatting | `scripts/fmt-check.sh` |
| Lint | `cargo clippy --workspace --all-targets` |

## Web dashboard

The dashboard is the React app under `web/` (Vite + React 19 + TypeScript +
Tailwind v4 + TanStack Query, issue #234), built once into `web/dist` and
embedded into the binary — `crates/server/src/spa.rs` serves it at `/app`, with
client-side routes falling back to `index.html`. It replaced the server-rendered
Topcoat dashboard, which retired in issue #256: `crates/web` is now only the
deliberately server-rendered `/verify` and `/bill/{token}` pages plus their
stylesheet and the bills-export row types the server's download routes reuse, and
the server answers every legacy `/dashboard/*`, auth and `/admin` path with a
`301` to its `/app` twin (`crates/server/src/web.rs`).

- `cargo run -p oxsum-server` serves everything; `cargo build -p oxsum-server`
  embeds whatever `web/dist` holds. To rebuild the frontend, `npm ci`,
  `npm run codegen` (after an `openapi.yaml` change) and `npm run build` in
  `web/` — see `web/README.md`; `npm run dev` proxies `/api` to a server on
  `:3000` for iteration.
- In-flight gateway turns are pushed to the dashboard over `/ws/billing` (WebSocket,
  session-cookie auth): a snapshot of the open holds on connect, then started, progress
  and settled events as turns happen. The events come from a process-wide broadcast the
  gateway publishes to — best-effort and in-memory; the watch table stays the record.
- The public `/verify` page needs no login: it posts a proof bundle and a content hash,
  and the server runs `oxsum_verify::verify_bundle` plus the charge and price-binding
  checks, then renders the verdict. When the deployment shipped the wasm bundle, the
  page's `verify.js` loader runs the *same* `oxsum_verify` functions in the browser
  instead (issue #284) — the page is the no-JavaScript floor, the wasm is the
  don't-trust-the-server path. The pure verification logic lives in `crates/verify`
  (`oxsum-verify`); its `wasm` feature exports the `verify_*` functions over
  wasm-bindgen, and `scripts/build-verify-wasm.sh` emits the module into
  `crates/web/verify-wasm/` over the committed placeholders — a cargo build without
  the toolchain still works and the page keeps its server-side form.
- The public bill page (`/bill/{token}`, issue #302) is the customer's side of a
  usage report: the token is the whole credential, `Db::resolve_report_link` turns
  it into the frozen report, and the page renders the figures beside the operator
  key and the ledger head. It is server-rendered and needs no JavaScript; the
  response layer in `crates/server/src/web.rs` sets `Referrer-Policy: no-referrer`,
  `X-Robots-Tag: noindex` and, for the bill only, `Cache-Control: private, no-store`.
  No third-party script runs on it. Pressing **Verify this bill** runs the same
  verifier in the browser over the bundle the page embeds (`bill.js`, issue #305):
  the script fetches the wasm artifact, hashes it, calls `verifyUsageReport` with
  the bundle's own key, and reports what it checked. Without JavaScript the bill
  still renders and the button simply does nothing.

## Docker image

The release image is built by the `Dockerfile` at the repo root. It is a multi-stage build:
the React dashboard is built by the `frontend` stage (Node 22, `npm ci` then `npm run build`),
the `/verify` verifier is compiled to wasm in the `wasm` stage (`scripts/build-verify-wasm.sh`
with a wasm-bindgen CLI pinned to the crate's version), the `builder` stage compiles the
`oxsum` binary in release mode and copies both artifacts over the committed placeholders, and
the runtime stage is `debian:bookworm-slim` plus `ca-certificates`.

```bash
docker build -t oxsum .
docker run --rm -p 3000:3000 -e DATABASE_URL=postgresql://user:pass@dbhost/oxsum oxsum
```

- Build-time only: Node for the dashboard bundle, and the `wasm32-unknown-unknown` target plus
  the pinned wasm-bindgen CLI for the verifier. Neither reaches the runtime image.
- Runtime: `debian:bookworm-slim` plus `ca-certificates`. reqwest uses rustls, so no
  OpenSSL is required. It carries the `oxsum` binary.
- The image never bakes in `.env` (`.dockerignore` excludes it); `DATABASE_URL` points
  at an external PostgreSQL and is supplied at `docker run`, along with the `OXSUM_*`
  settings above. `OXSUM_ADDR` defaults to `0.0.0.0:3000` inside the image so the port
  mapping works. The schema is migrated at startup, so there is no separate migrate step.

The `Release image` workflow builds this image on every push to `main` (docs-only and
markdown-only pushes are skipped) and smoke-tests it over HTTP against a real PostgreSQL —
that is the only gate that runs the artifact. Pull requests are covered by CI's compile and
test jobs; the image itself is not built on every PR.

## Publishing `verify-oxsum-bill` to npm

The package is built and packed once by the candidate's clean consumer check.
The release path consumes that original verified `.tgz`; it never invokes
`npm publish` in `web/verify-sdk` or repeats prepare/prepack. Build or version
changes require a new candidate run. A locally consistent tarball alone does not
authenticate successful candidate/full-CI provenance.

After explicit publication authorization, select the candidate run/attempt,
source SHA, embedded package version and an unoccupied distribution tag:

```bash
python3 scripts/publish-candidate-sdk.py /path/to/candidate-bundle \
  --sha <40-character-source-sha> --run <successful-candidate-run> --attempt <attempt> \
  --version <embedded-sdk-version> --tag candidate-<version-with-dashes> \
  --registry https://registry.npmjs.org/ --receipt /new/sdk-publication-receipt.json
```

Requirements: Python 3.10+, Node 24/npm 11, authenticated `gh` for candidate
artifact reads, and `OXSUM_NPM_TOKEN` supplied from a secret store with publication
authority for `verify-oxsum-bill`. The helper never sources `.env`. The manual
`release-sdk.yml` workflow provides the same selectors and repository secret;
it does not run on pushes and has no image/deployment action.

The helper snapshots and validates the version-2 candidate online, verifies the
selected SDK version and original SRI, then runs real `npm publish <absolute-tgz>
--ignore-scripts`. Isolated cwd/config/cache/environment prevent ambient npmrc,
proxy or Node preload overrides; package `publishConfig` is refused. npm sees a
bounded ephemeral loopback relay and token. The remote token remains in Python,
which only sends it to the exact selected registry origin and refuses redirects.
The relay checks the original attachment bytes/SRI and only replaces transport
metadata's tarball URL; the gzip archive is unchanged. No lifecycle hook, build,
repack or automatic npm provenance generation occurs.

Before writing, the selected distribution tag must be absent or already point
to the same version. A version already present succeeds only if metadata SRI and
downloaded bytes match exactly and the tag already matches. Different version
bytes, an occupied tag, absent tag on an existing version, auth/network/proxy
errors and redirected/foreign tarball URLs fail closed. The helper does not
move `latest` or any existing tag to a new version. Tag changes require their
own explicitly reviewed registry operation. Publication jobs serialize; external
writers must observe the same exclusion because npm tags have no compare-and-swap.
A partial publish without a receipt is incomplete; retry the same candidate/tag
with a new receipt path to verify it. A missing tag requires explicit repair.

The independent receipt binds source/workflow, candidate manifest hash, original
tarball SHA256 and SHA512 SRI, package/version, registry/tag and final verification.
Keep it before the workflow artifact expires. This tooling is not a claim that
the package was publicly published; no public npm request was made in its local
rehearsal. The historical 2026-11-04 npm E404 observation remains historical.

```bash
python3 scripts/test-candidate-sdk.py
python3 scripts/test-candidate-sdk.py --local-registry /new/sdk-rehearsal-output
```

The real npm rehearsal creates task-owned loopback HTTP endpoints, captures the
actual publish request, verifies uploaded/downloaded compressed bytes, proves
lifecycle hooks did not run, and refuses repeat/version/tag/redirect conflicts.
Only explicit `--local-fixture` with numeric `http://127.0.0.1:<port>/` can bypass
online provenance; it marks receipts `local-fixture` and cannot publish remotely.
Its synthetic package contents do not establish final SDK candidate acceptance.

## Deployment runbook

The deployment is one binary plus one PostgreSQL database; every operational
procedure below is a property of those two pieces. For the managed
self-hosted shape — Compose profiles, initialization, secrets linting,
operator configuration — `docs/self-hosted-install.md` is the runbook and
`docs/self-hosted-upgrade.md` the upgrade/rollback/restore half (issue
#619); what follows here is the same procedures in their development form.

### Backup and fresh restore

Use the [explicit backup/restore procedure](operator-backup-restore.md), which
requires a selected container/database or PostgreSQL URL, publishes a private
snapshot-bound dump/manifest pair and restores only into a new database. Both
shell wrappers remain available; they never source `.env` or discover containers.

```bash
env -u DATABASE_URL CONTAINER=oxsum-core-postgres-1 POSTGRES_DB=oxsum \
  scripts/backup.sh /private/backups
```

`backup-restore-drill.sh` now requires explicit `SOURCE`, `TARGET`, `OUT_DIR` and
connection selection. It never drops the target. Its count comparison verifies
transport/inventory, not whether a server can serve traffic or old proofs.
`rehearse-backup-restore.py --output /new/private/path` tests both connection
modes and snapshot isolation in two disposable containers, retaining private
measured evidence. Old October 22 same-cluster drill measurements do not certify
the current artifact or a representative capacity dataset.

A snapshot includes `oxsum` and every ledger schema; never back up only `oxsum`.
The helper's repeatable-read snapshot also binds its inventory counts. Retain
sealing/signing keys and runtime image identity separately from the database.
WAL/PITR remains separate operator tooling. Measure your dataset's duration and
disk needs, and check `max_locks_per_transaction`, shared memory and available
storage: the historical ~46,000-table fixture exhausted stock settings. The
Compose PostgreSQL configuration remains the documented starting point, not a
capacity promise. The helper reports settings; it never changes the cluster.

Restore preserves the backed-up schema first; migration is an explicit subsequent
step using the selected artifact. Confirm exact schema/runtime, retained keys,
old bill evidence and new calls before shifting traffic. In-flight reservations
and operation recovery require their own drill; a successful dump/restore count
check does not exercise those workers.

The [development artifact recovery rehearsal](artifact-recovery-rehearsal.md)
accepts frozen old/current/compatible binaries and verifier tarball identities,
then exercises two customers, real SIGTERM drain, upgrade, separate restore,
retained keys, offline old/new proofs and new calls. It is a development tier,
not final-candidate evidence.

### Migration ordering

There is one ordering rule: **the newest binary migrates**. oxsum's own tables
migrate inside `Db::migrate` at startup — `crates/core/migrations/` applied in
file order, each file in one transaction with its `oxsum._migrations` row, under
a database-wide advisory lock so two booting processes cannot race. Ledger
schemas are lazier: doubleentry's `migrate` creates `ledger_<tenant_id>` the
first time a wallet is opened — including at restore time, when a wallet opens
on a schema that is already there and the check is a no-op.

For an upgrade, that means: stop the old binary (or let the advisory lock queue
the new boot behind a draining one), start the new binary, watch the log for the
migration lines. A migration that fails refuses the boot and rolls its
transaction back, so a bad migration file cannot leave a half-applied schema —
but it does leave the service down; test upgrades on a restored dump first.

### Upgrade and rollback

Upgrade is pull the image, stop, start: migrations run, the pool opens, the
server serves. The contract `openapi.yaml` versions additively, so a client that
worked against release N works against N+1.

"Stop" is the first of the signals the server drains on: `SIGTERM` (what
`docker stop` and systemd send), `CTRL_C` on an interactive console, or
`CTRL_BREAK` on Windows. The listener closes and in-flight requests finish.
The drain is bounded: a connection a client never lets go of cannot hold the
process open, so nine seconds after the signal the process exits regardless —
just under `docker stop`'s default SIGTERM grace — and the sweeper settles
whatever the exit left in flight.

Rollback depends on schema and behavior compatibility. `/readyz` requires exact
schema equality, so an older binary cannot be assumed ready against an additive
newer schema. Retain the prior digest, environment and a verified pre-upgrade
backup. A schema-changing rollback restores into a fresh database; no down
migration exists. A same-schema retained artifact may be restarted only after
an operator compatibility decision and the deployment helper's identity checks.

Image promotion consumes the validated candidate archive and creates one
explicit version tag plus an immutable digest receipt. Deploy is a separate
manual workflow with no build/publication call: it verifies candidate provenance,
observed runtime version and published digest, then runs migration and serving
from that exact artifact. The managed helper retains current/previous receipts,
refuses unmanaged containers and drift, and never guesses an automatic rollback
after a failed migration. Follow `docs/self-hosted-upgrade.md` for maintenance,
legacy adoption, failure recovery and same-schema versus restored rollback.

The CI gate builds and smoke-tests the image on every pull request, so a release
that cannot boot never reaches the registry; what it cannot smoke-test is your
traffic — the dump before the upgrade is the answer to that.

### Key rotation

| Secret | Rotates by | What it costs |
| --- | --- | --- |
| `OXSUM_ADMIN_TOKEN` | Restart with the new value | The running admin sessions — the token is the credential, so the old one simply stops working |
| `OXSUM_HEAD_SIGNING_KEY` | Restart with the new seed | Old head and price-checkpoint signatures stop verifying under the new key. The verifying key is published at `GET /api/v1/log/key`, so verifiers fetch the new one — but a head a client archived under the old key is that client's trust anchor; publish rotations so archives know to re-anchor |
| `OXSUM_SECRET_KEY` | **Not a restart** | It seals the upstream credentials in `oxsum.channel_credentials` and the webhook signing secrets in `oxsum.webhook_endpoints`; a new value cannot open them. Rotate by re-entering every channel's upstream keys and recreating the webhook endpoints, *then* restart with the new value — or lose the ability to read what was sealed |
| Database credentials | PostgreSQL's own role/password machinery | Update `DATABASE_URL` and restart |
| Organization API keys | `DELETE`/`POST /api/v1/org/keys` | Per-organization, self-service, no restart |
| Integration credentials | Owner `POST /api/v1/business/integration-credentials`, then `POST /api/v1/business/integration-credentials/{credentialId}/revoke` | Issue a replacement with explicit scopes/customers/expiry and save its secret once. Finish old holds before revocation where possible; the replacement cannot close them. Trusted recovery handles orphaned holds. Legacy global service credentials are disabled and cannot be reissued. |

Two of these are the expensive ones by design: `OXSUM_SECRET_KEY` is the root of
what the database holds sealed, and the head-signing seed is the root of what
verifiers trust — both are the kind of secret a deployment protects in a vault,
never rotates casually, and never loses without a plan for the re-entry the
table above names.

## Continuous integration

CI and the candidate workflow install mold 2.40.0 and sccache 0.19.0 through
`scripts/install-ci-tools.py` (#822). The helper requires Python 3.11+, curl
with `--retry-all-errors`, and x86_64 Linux. It fetches exact official GitHub
asset IDs over HTTPS, with four finite attempts and a 250-second process cap
per URL. On transport failure only, the corresponding pinned official browser
release URL is tried with the same bound. Integrity, oversized-response, local
I/O and TLS trust failures fail closed. Both routes require the same pinned
size/SHA-256 before extraction. Installation uses a private
staging directory and atomic rename into `RUNNER_TOOL_CACHE`; no privileged
tar pipeline runs. An existing cache must match the verified tree or the job
fails without replacing it. Mold's bin directory enters `GITHUB_PATH`.
Sccache's verified cache uses the Actions tool-cache layout; the existing
Mozilla action is pinned to `v0.19.0` and retains runtime-cache setup and
post-run statistics without a latest-release lookup.

The local `install-ci-tool` composite action (#828) restores and saves each
tool's compressed archive independently, keyed by installer content, tool,
runner OS and architecture. There are no partial restore keys. Every restored
archive is copied into private staging and checked again against the pins;
cache provenance never substitutes for byte verification. Valid restored bytes
permit installation without network access. Corrupt caches fail without
extraction and require removal of the affected Actions cache before retry.
Archives are saved immediately after installation so downstream test failures
do not force another cold download. `.github/actions/**` changes select the
Rust gate tier just like workflow changes.

`python3 scripts/test-ci-tools.py` uses only a temporary loopback HTTPS server
and test certificate to check retry, permanent failure, partial/HTML/corrupt
responses, unsafe archives and cache preservation. `fmt` runs this suite.
[Source and hash evidence](roadmap/operator-launch/evidence/verified-ci-tools.md)
records the official assets. Existing required checks and full candidate gates
remain unchanged. New candidate runs require a source revision containing this
installer, like the workflow's other source-owned gate helpers; an older
historical SHA does not silently borrow a helper from a newer revision.


`.github/workflows/ci.yml` runs on every push to `main`, every pull request that
touches more than documentation (`paths-ignore` skips `docs/**` and `*.md`), and a
nightly `schedule` at 03:17 UTC. Each push to `main` gets its own concurrency group
keyed on the commit (issue #549): GitHub cancels a *pending* run when a newer one
joins its group even with `cancel-in-progress: false`, so a shared main group let a
burst of merges drop a push's gate. The nightly and a manual `workflow_dispatch`
ignore the path filter and force the full tier, so main is re-verified within a day
regardless of what the last push touched — including a docs-only push, which
`paths-ignore` never triggered at all.

The gate has two tiers (issue #552). A pull request pays the fast tier only — the compile-time
checks, the lib/doc tests and the two stage builds — and merges on those. The heavy
tier (`build-tests`, the three shards, `test-extras`) runs on pushes to `main`, the
nightly and a manual dispatch, where nothing is waiting on it. The five required checks (`fmt`, `clippy`, `test`,
`wasm`, `frontend`) are all in the fast tier, so branch protection needs no change,
and `clippy --all-targets` still compiles every test target — a type or borrow error
cannot merge. The price is explicit: an integration or database failure can reach
`main` and surface only after the merge, so a red `main` is fixed forward or reverted
immediately. The local pre-push bar stays at fmt + clippy + the touched crates' tests
(issue #370). Build, test and clippy jobs use `dtolnay/rust-toolchain@stable` —
the runner's stable, no version pin — with the components they need. The `fmt`
job is the exception: it installs the exact nightly named in `rustfmt-nightly`,
so the formatter matches the one the local scripts run:

- `fmt`: `scripts/fmt-check.sh`, using the nightly named in `rustfmt-nightly`
  (the same file the local scripts read). The job installs that toolchain with
  its `rustfmt` component; build and clippy jobs stay on stable.
- `clippy`: `cargo clippy --workspace --all-targets` with `-D warnings`. No database
  is needed to compile: the tree uses no `sqlx::query!` macros, so compilation never
  touches a live database.
- `wasm`: builds the image's `wasm` stage (`docker build --target wasm .`) before
  merge, but only when the change can affect it — `Dockerfile`, `.dockerignore`,
  the manifests and lockfile, `scripts/`,
  `crates/verify/` or a workflow file (issue #296). The job always runs and
  succeeds as a no-op otherwise, so it can be a required check; it is the only
  PR-time gate that compiles the verifier for wasm32.
- `frontend`: builds the image's `frontend` stage (`docker build --target frontend .`)
  before merge when `web/`, `crates/server/openapi.yaml`, the `Dockerfile` or a
  workflow file changes (issue #309). Always runs, no-op otherwise: it is the
  PR-time check that `npm ci`, the openapi codegen and the Vite build all succeed.
- `test`: the fast half of the split (issue #237): `cargo test --workspace --lib`
  plus the doctests, with no postgres service and no integration binaries — the
  heavy test targets (testcontainers, sqlx test deps) never compile in this job,
  so a unit break answers in a few minutes instead of waiting on the full suite.
- `build-tests` (pushes to `main`, the nightly, manual dispatch): `cargo nextest archive --workspace` compiles every test
  binary once into a `.tar.zst` artifact (issue #276). A second archive builds
  doubleentry's postgres suite with the feature on — its
  `#![cfg(feature = "postgres")]` gate means a plain `--workspace` archive would
  ship an empty binary. `Free disk space` lives here, the only job that still
  compiles the heavy targets.
- `test-shard` (matrix of three, `main` pushes, nightly and manual dispatch): downloads the archive and runs
  `cargo nextest run --archive-file … --partition count:i/3`, so the suite's
  wall-clock is a third of one runner's. Each shard gets its own
  `postgres:17-alpine` service and runs the scratch-database sweep before the
  tests — an interrupted run's residue cannot bleed into the next shard's
  timing (issue #271).
- `test-extras` (pushes to `main`, the nightly, manual dispatch): downloads the same artifact, extracts both archives, and runs
  the suites that cannot live under nextest — `generative`, `seed`, `jobs`
  coordinate through process-global state — by invoking the binaries straight
  from `target/debug/deps`, the same one-binary-per-process model `cargo test`
  gives them. It also runs the ledger-gated doubleentry Postgres conformance
  suite off the second archive (the vendored crate almost never changes, so
  the gate keys on `crates/doubleentry/`, `crates/core/migrations/`,
  `Cargo.lock`, or `ci.yml` itself), and sweeps stale testcontainers beside
  it (issue #277).

  Docker made doubleentry's suite the one that could not run on a plain
  developer machine — `DOUBLEENTRY_DATABASE_URL` lifts that for local runs —
  which is why the conditional lives in CI rather than a local gate.

The gate is built for wall-clock, not only for coverage (issue #550). `filter` and
`fmt` are the only jobs the compile steps wait on — `clippy` runs beside them,
because its ~130s of lints have nothing to give a build that is already compiling
the same tree. Test binaries are archived with symbols stripped: the `nextest`
artifact was 1242 MB and every shard spent over a minute downloading it. The
disk-space step deletes its four trees in parallel rather than one after another,
and `release-image.yml` imports the `wasm` and `frontend` stage caches that
`ci.yml` exports, so the post-merge image build no longer rebuilds both stages
from scratch. The audit workflow installs `cargo-audit` from its prebuilt
release binary instead of compiling it.

The retired `browser` job that used to gate the Leptos/wasm bundle went with the
Topcoat dashboard (issue #256): `browser_contract` is an ordinary integration
test now — no build step, no `wasm32` target — so it runs inside
`test-integration` like every other suite. The `wasm` job added later gates a
different artifact: the verifier crate the `/verify` page serves, built into the
image's `wasm` stage rather than a Leptos site.

The compile jobs cache the cargo registry and `target/` through
`Swatinem/rust-cache`, so a run that touches one crate does not recompile the
dependency tree. Compiled object files themselves are cached across runs through
`mozilla-actions/sccache-action` (`RUSTC_WRAPPER=sccache`, GitHub cache backend —
issue #237), which is what cuts cold-run compile time once a commit reuses any
of the workspace's dependency graph. On the `main` push the three test shards and
test-extras all run in parallel once build-tests hands them the archive; a pull
request never waits for any of them (issue #552).

A second workflow, `.github/workflows/release-image.yml`, gates the release image
itself: it runs on pushes to `main` and on `workflow_dispatch` — not on pull
requests, where a Docker build per push duplicated the compile for no extra signal.
It builds the `Dockerfile` with `docker/build-push-action` (`load: true`, so the image
lands in the runner's own daemon and is never published) and
`cache-from`/`cache-to` of `type=gha`, so a rebuild after a one-line change reuses the
Rust layers instead of paying for another 20-40 minute release compile. The
built image is then run with `--network host` against the same `postgres:17-alpine`
service container: the runner is Linux, and the service publishes its port on the
runner's loopback, so `DATABASE_URL` names `127.0.0.1:5432` to match that choice. The
workflow waits for `pg_isready`, then polls `http://127.0.0.1:3000/healthz` in a
bounded retry loop and asserts that:

- `/healthz` answers 200. The image migrates at startup and refuses to boot at all
  against a database it cannot reach, so a 200 also means the schema is in place.
- `/app` answers 200 with `id="root"`, and the hashed `/app/assets/*` script its
  shell names answers 200 — proof that the frontend stage's bundle was embedded
  (issue #234).
- `/login` answers `301` to `/app/login`: the Topcoat pages retired to the SPA
  (issue #256).
- `/style/main.css` and `/verify` answer 200; `/verify` is the server-rendered
  page that hosts the browser verifier.

When an assertion fails the step prints `docker logs` and exits non-zero, so a red run
is diagnosable from its own log rather than by re-running it.

## React dashboard (`web/`, issue #234)

The React app under `web/` is the shipped dashboard — it reached parity and the
Topcoat pages retired to it (issue #256). It talks to `/api/v1` only — no
backend changes were needed.

```bash
npm --prefix web ci          # install the toolchain (Node 22+)
npm --prefix web run test:contract # check OpenAPI security declarations
npm --prefix web run codegen # check the contract, then regenerate src/api/schema.d.ts
npm --prefix web run dev     # vite dev server, proxies /api to localhost:3000
npm --prefix web run build   # emit web/dist
```

The contract check runs before code generation, including the Docker frontend
stage and its CI job. It rejects unknown security schemes, combined cookie and
machine requirements, denied-key contradictions and changes to the audited
commercial/financial credential boundaries. It parses the full OpenAPI YAML;
these checks do not replace HTTP authorization tests.

The server embeds `web/dist` via `rust-embed` and serves it at `/app`
(`crates/server/src/spa.rs`): extension-less routes fall back to
`index.html`, hashed `assets/` get immutable caching, and a missing asset is
a real 404. Debug builds read `dist` from disk, so a `npm run build` is
visible without a cargo rebuild — this is `rust-embed`'s default, so the
workspace keeps its `debug-embed` feature off (enabling it embeds even in
debug builds). Release builds embed the directory at compile time — the
Dockerfile's `frontend` stage produces it,
so the release image is still one binary. `web/dist` is git-ignored except a
committed placeholder `index.html` that keeps the embed folder present on a
fresh checkout and tells a developer to run the two `npm` commands above when
the server serves it unbuilt. Styling follows DESIGN.md: its tokens are mapped onto
Tailwind v4 theme colors in `web/src/index.css`.

## Test strategy

- Unit tests: inside each module's `#[cfg(test)]`, covering pure logic such as tenant id validation and configuration parsing.
- Integration tests:
  - `crates/core/tests/` exercises the wallet's invariants on real PostgreSQL: isolation, concurrent no-overdraw, hold/settle, idempotency, tamper detection, restart recovery, concurrent migrate; `crates/core/tests/identity.rs` covers registration, organizations and API keys, including that the plaintext secret is nowhere in the database; `crates/core/tests/sessions.rs` covers login, session authentication, logout and the key role scopes, including that the plaintext token is nowhere in the database and that a session dies with its membership.
  - `crates/core/tests/generative.rs` generates random sequences of top-ups, holds, settlements, replays and key collisions and checks each step against an in-memory model of the wallet account's two layers: the outcome the model predicted, the available balance, that it never went negative, the log size, and a proof for every entry the case wrote (including that a changed amount stops verifying). 1000 cases by default, `PROPTEST_CASES` to narrow it locally; a failure prints its seed and the shortest failing sequence. Since settlement-hold pairing (issue #10) a settlement names the hold it releases, and the oracle predicts `HoldNotFound` for a settlement naming a key that is not an outstanding hold. The suite loads `.env` before reading `DATABASE_URL` (issue #18): with the variable only in `.env` it runs for real, and with no database anywhere it skips all cases with a warning rather than failing.
  - `crates/server/tests/` covers the HTTP layer: key auth, error format, full request chains, and that one organization's key cannot reach another's ledger; `crates/server/tests/sessions.rs` covers the login/logout/session endpoints, the cookie attributes, and the member/owner key rules over HTTP. `crates/server/tests/dashboard.rs` covers the billing WebSocket's contract — the snapshot of open holds, the live events, and that an unknown session is refused; `crates/server/tests/browser_contract.rs` covers the server-rendered sliver (`/verify`, its stylesheet) and the `301` map that sends the retired page paths to `/app` (issue #256).
- doubleentry's own tests: changing `crates/doubleentry` requires all of them passing, including its conformance suite.
- E2E: the full top-up → call → verify flow is covered. `crates/server/tests/chat_flow.rs` drives login, a top-up, one streaming chat turn through a scripted upstream, the billing events, and then fetches the settlement's proof bundle and verifies it with `oxsum_verify::verify_bundle`; `demo/demo.py` uses deployment owner consent, customer invitation and a labeled synthetic manual receipt, then runs the official OpenAI SDK against a local stub and verifies the immediate settled entry with the packed SDK after its server/database are gone. It fails closed on scratch provisioning, exact charge/reservation or verifier failures; no root `.env`, ordinary database fallback or table reset is used. TODO.md carries the current status.
- Real-provider E2E: `crates/server/tests/real_upstream.rs` runs the same bill path against a provider that is not a stub — registration, a manual-rail top-up, the model catalog, one non-streaming and one streaming completion against the real upstream, then reads the billing record back and verifies the proof bundle, the charge recomputation and the operator-signed tree head with the same `oxsum_verify` calls the browser makes. It is `#[ignore]`d and additionally skips unless `OXSUM_E2E_UPSTREAM_BASE_URL`, `OXSUM_E2E_UPSTREAM_API_KEY` and `OXSUM_E2E_MODEL` are all set, so it never runs in the ordinary gate or CI; the provider key lives only in the shell environment. Run it against a clean development database — the suite writes a channel sealed with its own key and needs `prepare` to open every stored channel:

  ```bash
  docker exec -i oxsum-postgres-1 psql -U postgres -d oxsum -f - < scripts/reset-dev-database.sql
  OXSUM_E2E_UPSTREAM_BASE_URL=https://api.stepfun.com/step_plan/v1 \
  OXSUM_E2E_UPSTREAM_API_KEY=sk-... \
  OXSUM_E2E_MODEL=step-5-preview \
    cargo test -p oxsum-server --test real_upstream -- --ignored --nocapture
  ```
- E2E: the full top-up → call → verify flow is covered. `crates/server/tests/chat_flow.rs` drives login, a top-up, one streaming chat turn through a scripted upstream, the billing events, and then fetches the settlement's proof bundle and verifies it with `oxsum_verify::verify_bundle`; the standalone demo replacement is tracked by #728 / PR #751 and needs its own post-integration clean-checkout acceptance. Test-only funding is not a customer self-credit API. TODO.md carries the current status.
- Real-provider E2E: `crates/server/tests/real_upstream.rs` is explicitly ignored
  and requires provider variables. It is not ordinary regression coverage or a
  spending guard. The current bounded provider harness is tracked by #740;
  pricing, input/output bounds and durable attempt reservations must be settled
  before any external request. Use an isolated database and a task-owned config,
  not a reset of the ordinary development database. The
  [historical T31 call](roadmap/self-hosted-v1/evidence/T31-followup.md) establishes
  one nonstreaming DeepSeek response, not the full protocol/provider matrix.

- Pool-saturation load check (issue #235): `a_saturated_pool_refuses_backpressure_never_a_500` in `crates/server/tests/gateway.rs` fires fifty concurrent turns through a three-connection pool against a 250 ms scripted upstream — saturation must answer 429 `POOL_BUSY` with `Retry-After`, never a 500. `#[ignore]`d like the real-provider suite; run it when the pool or the gateway's database access moves:

  ```bash
  cargo test -p oxsum-server --test gateway -- --ignored --nocapture
  ```

## Regression checklist

Before committing, confirm:

- [ ] Branch name matches the branch naming policy above, and the PR links its issue in the title or body where applicable
- [ ] `scripts/fmt-check.sh` and `cargo clippy --workspace --all-targets` produce no warnings
- [ ] With `DATABASE_URL` available, the relevant `cargo nextest` suites pass; cross-crate changes also pass `cargo nextest run --workspace`. Doctests and final candidate full CI remain separate required gates.
- [ ] Interface changed: `openapi.yaml`, implementation, tests and callers are all in sync
- [ ] `crates/doubleentry` changed: carries an `oxsum change` comment and an entry in docs/decisions.md
- [ ] UI changed: loading, empty and error states all checked
- [ ] Related docs updated (see the doc update rules in AGENTS.md)

## Release

The release is a single binary plus a Docker image (TODO.md B-10, closed 2026-10-03):
the `Dockerfile` builds the `web/` frontend, compiles `oxsum-server` (which
embeds `web/dist`), and packages the one binary into the image described above. CI
(`.github/workflows/ci.yml`) is the green gate on a fresh clone, and every pull request
also builds the image and runs it against PostgreSQL
(`.github/workflows/release-image.yml`, issue #49), so the artifact a release ships is
checked long before the release is cut.

The release-candidate gate (issue #620, T29) sits between "main is green" and
"a version is published": `gh workflow run release-candidate.yml -f sha=<sha>`
re-runs the fast gate, the wasm/frontend builds, the packed-SDK consumer check
and a no-push image build on one exact promoted SHA, and produces
`candidate-bundle` — the manifest, `SHA256SUMS` and a release-notes draft.
`scripts/release-candidate-smoke.sh` then runs the candidate image on a
throwaway compose stack. `docs/self-hosted-release.md` is the runbook: gate
order, artifact inventory, and the promotion path into `release.yml`'s manual
dispatch.

## Business ownership upgrade preflight (#593)

Before starting an upgraded server, back up the database and encryption/signing keys using the deployment runbook. Run the candidate binary with `oxsum ownership-preview` against the intended database to inspect customer, channel, plan and ledger-schema counts. This subcommand executes before `Db::migrate` and provider preparation; it changes no schema or ownership and makes no provider call. It handles a fresh database (zero counts) and the legacy schema. Normal server startup applies additive migration 0072 and records the mapped counts once.

Inspect `GET /api/v1/admin/business` with deployment authority after startup. `initializedAt: null` and an empty business_staff table are intentional: existing customers are never silently promoted. The authenticated setup flow is delivered in T03. New orgs map automatically; their ledgers remain lazy. A schema downgrade is unsupported: restoring the matching database/key backup is the rollback route. Do not dispatch the live Deploy workflow to rehearse migration; use an isolated database and candidate binary.

## Initial business owner setup (#629)

After inspecting the ownership preview/receipt, use the deployment token to POST `/api/v1/admin/business/setup-tokens` with a unique `idempotencyKey` and the intended `ownerEmail`. Deliver its returned secret directly to that person; do not log it or append it to a URL. It expires after 30 minutes. An existing account logs in and POSTs `/api/v1/business/setup` with `{token, businessName}` using the session cookie. A fresh install uses `/api/v1/business/setup/register` with the same fields plus the invited email and a password of at least 12 characters, then logs in through the normal route. Store the returned initial customer API key securely; it is not business authority.

Check `/api/v1/admin/business`: initialization should be non-null and the migration receipt remains unchanged. A process restart never reopens setup. If the mint response was lost, its retry reveals metadata only; mint a new key. If acceptance timed out, inspect initialized status before retrying. Invalid/expired tokens change nothing; initialized businesses require the separate ownership-transfer/recovery workflow, not another public setup. This API stage does not add a setup UI or complete staff role/policy enforcement (T03.b/T24). Use an isolated database for rehearsal; Deploy is not a test command.

### Business authority validation (#630)

Use `cargo nextest run -p oxsum-core -p oxsum-server -E 'binary(business_staff) or binary(business_setup) or binary(oidc) or binary(oauth) or binary(totp)'` for setup, context, policy and second-factor boundaries. These suites create isolated PostgreSQL databases and use real setup/session APIs. Legacy HTTP suites call the scratch-only `oxsum_testkit::set_registration_fixture` when their purpose assumes open registration; it refuses databases outside the test prefix and is never linked into production policy handling. New installs must complete owner setup and set registration policy before following public signup examples.

### Customer lifecycle validation (#633)

Focused checks: `cargo nextest run -p oxsum-core --test customer_lifecycle --test customer_upgrade --test dunning --test suspension` and `cargo nextest run -p oxsum-server --test customer_lifecycle --test suspension`. Customer tests exercise real setup and session authority, archive/admission races, independent reasons, audit rollback and historical proof preservation in isolated PostgreSQL. The HTTP suite sends actual gateway calls to a loopback provider before/after restoration; no paid call is required.

### Customer invitation upgrade and checks (#634)

Migration 0076 extends existing links without changing their IDs, hashes or expiry. It adds nullable pending-customer terms and business ownership, then keeps the original organization/profile identity trigger compatible with signup/team/demo paths. Existing links remain single-use; the current API requires idempotency keys for new links and direct member POSTs return 403. No new environment setting is required. Run core `invitation_consent` and `invitation_upgrade`, server `invitation_consent` and the existing invitations/members/email/antibot suites against testkit scratch databases. The new creation/revoke endpoints send no mail unless the team request explicitly supplies `email`; tests use a local SMTP stub.

### Scoped integration smoke client (#638)

`python3 examples/scoped-integration/client.py --help` documents the standard-library
client. `OXSUM_INTEGRATION_SECRET` is an example-only environment variable containing
an owner-issued oxs-int- secret; it is not read by the server. The client reads only
a selected customer unless the explicit `meter --confirm-metering` command is used.
Run it against isolated test funds first. The production-router test
`cargo nextest run -p oxsum-server --test integration_credentials` launches this
client against loopback PostgreSQL-backed fixtures and checks replay/isolation.
See the [example runbook](../examples/scoped-integration/README.md) for issuance,
required event prices and exact negative cases. No paid provider is called.

### Developer examples and the fresh-install gate (#617)

`docs/integrations.md` is the supported-surface matrix — every claim cites the
suite that proves it; `docs/unsupported.md` is the refusal catalog. Runnable
walkthroughs live beside the existing examples:

- `examples/consumption/` — the full customer journey (invite → redeem →
  manual-receipt top-up → metered gateway call → usage/bill → evidence bundle
  → offline verify with `verify-oxsum-bill`).
- `examples/scoped-management/` — mint a scoped integration credential, read
  its customer, bill an event, read the operation back.
- `examples/webhook-tools/` — `sign-and-replay`, `dedup-check` and
  `retry-storm` against the versioned merchant intake.

`scripts/verify-examples.sh` builds the server and SDK, provisions a random scratch
PostgreSQL database through the explicitly selected admin connection, and runs
all examples on a loopback listener with generated owner credentials. It never
loads the checkout's `.env`, seeds a default password, falls back to an ordinary
database, or changes example dependencies in the source checkout.

```bash
python3 -m venv .venv-examples
.venv-examples/bin/pip install -r scripts/verify-examples-requirements.txt
PYTHON=.venv-examples/bin/python scripts/verify-examples.sh
```

Prerequisites: PostgreSQL 17 with CREATEDB authority, Node 22+, Rust, the wasm32
target and the lockfile-matched `wasm-bindgen` CLI. The default provisioning URL
is the local Compose postgres database. `--database-url` (or exported
`DATABASE_URL`) selects one explicit endpoint; only `sslmode` is accepted as a
URL query option. Creation and removal use the same connection. The supplied
database is never used by the application. A failed create starts no listener;
a failed cleanup is a failed run with the owned database name retained.

Use `--binary /path/to/oxsum --sdk-tarball /path/to/package.tgz` to exercise exact
existing artifact bytes without rebuilding. The runner copies both before use
and records their SHA-256 digests. `--examples-root /path/to/checkout` selects
maintained example sources (including the shared consent bootstrap); the
default is this checkout. `--output /new/private/directory` retains a sanitized
`result.json`, source-file hashes, the SDK tarball and private logs. Logs may
contain disposable credentials and remain mode 0600 in a mode 0700 directory;
console output contains assertion counts only.

Any failed or skipped essential assertion fails the runner. The only allowed
skip is the fixture's explicitly absent witness signature, separately counted.
The server and scratch database are stopped/removed before the independent
consumer verifies its retained fixture. The fresh customer's closed-day bundle
is empty; verifying the immediate new settlement belongs to `demo/demo.py`.
All funding and callback traffic here are local fixtures, not payment-provider
certification. The release-candidate gate uses the same runner.

Isolation failure-path checks:
`python3 -m unittest discover -s scripts -p test_verify_examples.py`.

### Business provider authority tests (#641)

`cargo nextest run -p oxsum-core --test business_providers --test provider_upgrade --test channels -p oxsum-server --test business_providers --test admin` exercises real setup/session authority, atomic audit faults, replay/rotation, pool concurrency across business and recovery paths, foreign configuration and pre-0078 provider preservation. Use isolated PostgreSQL 17 testkit databases. No provider call is made on configuration save. The focused command does not replace cross-crate workspace/shared-process gates or T06.b destination-policy tests.

### Provider destination policy (#642)

New/legacy channels default to public HTTPS. Approve an internal service through the owner connection API (or authenticated deployment recovery) with `destinationPolicy: {"network":"internal","allowedIps":["10.0.0.10"]}`. The allowlist is exact, at most 16 addresses, and changes require owner authority. DNS answering an unapproved address refuses the request. Use network DNS/firewall configuration that matches these explicit addresses; environment HTTP proxy variables do not affect provider requests. Redirects are never followed. A TLS certificate must still match the configured hostname. Link-local metadata and special infrastructure addresses remain forbidden even with an internal approval.

The bootstrap environment accepts `OXSUM_UPSTREAM_DESTINATION_POLICY` as the same JSON object; default is public. It is used only when seeding an empty channel directory, never to overwrite a running deployment's settings. Existing HTTP/internal channels must be reviewed and reauthorized before traffic resumes; migration preserves their secrets and history but does not infer approval. Use the explicit owner connection check for a non-inference HEAD, then independently test supported model behavior with separate billable-call authorization.

Migration 0079 snapshots upgrade-time coordinates for old task rows. Stop admission and drain old tasks before upgrading whenever possible: earlier versions did not retain the submitting endpoint/credential, so the upgrade cannot recover a route already changed before it ran. New tasks retain their original sealed credential and destination policy until terminal. Preserve the sealing key for these snapshots in backups/restore; changing a channel key or endpoint does not revoke an already admitted task. Cancel/drain old tasks and revoke the provider credential upstream when required. No new cross-customer ledger or proof identity is introduced.

Focused checks: `cargo nextest run -p oxsum-core --test provider_destination_upgrade --test business_providers -p oxsum-server --test provider_destinations --test tasks --test gateway`. The policy/DNS/redirect matrix is in the core and integrations lib tests. All test providers are loopback stubs with explicit internal approval; no paid call is part of the gate.

### Customer serving permissions (#644)

Run `cargo nextest run -p oxsum-core --test serving_access -p oxsum-server --test serving_catalog --test gateway --test fallbacks --test experiments --test tasks --test groups --test heads --test apis --no-fail-fast` with the isolated local test database. Core checks verify key/customer/channel intersections, permission snapshots, denied credential opening and fail-closed unknown/revoked/expired keys. HTTP regressions check catalog/estimate/checkpoint filtering, original experiment names, fallback aliases and task recovery after permission edits. The [protocol matrix](roadmap/self-hosted-v1/PROTOCOL-MATRIX.md) records current adapter limits; full cross-crate validation remains required.

### Merchant encryption startup check

Business merchant versions use the existing `OXSUM_SECRET_KEY` AES-GCM key. The server refuses startup when any current or historical merchant credential cannot open, or when merchant rows exist without the key. Preserve this key with database backups: rotated historical signing contexts still need it. Merchant settings saves validate structure and destination syntax locally and make no external request. Core and router regression tests are `cargo nextest run -p oxsum-core -p oxsum-server --test merchants`; they use isolated scratch databases and no real merchant credentials or payment calls.

## Isolated streaming capacity measurement

`DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres scripts/soak.sh`
runs the small, explicitly ignored `capacity` suite against a per-run scratch
database. `scripts/soak.sh --d09` selects 1,000 independent customer billing
accounts, 10,000 keys, 100 concurrent five-second stub streams and 20 scheduled
admissions/second for five minutes. No existing customer database or real
provider is used. The script requires a database and fails rather than skipping.

Override `SOAK_CUSTOMERS`, `SOAK_KEYS_PER_CUSTOMER`, `SOAK_CONCURRENCY`,
`SOAK_RATE`, `SOAK_SECONDS`, `SOAK_STREAM_MS`, or `SOAK_POOL` as needed; record
every override and host/DB resources with the result. `SOAK_REPORT` saves JSON.
Serialize heavy builds and other load tests while measuring. The harness checks
every operation, usage charge and wallet balance against seven minor units per
request, and requires zero open holds and reservations without running a cleanup
sweeper. Percentiles use nearest rank. Total latency minus the scripted upstream
delay is an overhead upper bound, not a pure gateway CPU measurement.

See [the measured D09 result](roadmap/self-hosted-v1/evidence/closeout-capacity.md)
for performance limits and the distinction between admitted rate and throughput
including the final stream drain. Background job retry/failure coverage remains
in `faults.rs`; this test does not run a job worker.

The `payment-fulfillment` periodic job drains accepted payment events and up to
100 paid orders every five seconds, under a per-kind leader lock. A worker's
five-second polling interval adds scheduling latency; this is separate from
daily reconciliation. Inspect the normal jobs surface for retries or dead runs.


Job due-time fixtures must use PostgreSQL `clock_timestamp()` when exercising
`claim_due_jobs`, whose due predicate uses the database's clock. A host-current
instant can still be in the future on a Docker/remote database. The failure
fixture checks the returned backoff against a database instant before failure,
then deliberately makes its own row due with SQL for the next attempt. It keeps
the production due predicate and five-attempt budget unchanged (#770).
### Local payment recovery fault smoke

After preparing the pinned stack in `infra/hyperswitch/` (#757), pass its tool
directory and the exact binary to the isolated fault runner (#760):

```bash
python3 scripts/hyperswitch-recovery-smoke.py \
  --sandbox /absolute/path/to/infra/hyperswitch \
  --binary /absolute/path/to/oxsum
```

It requires the dummy-only local merchant, starts a loopback fault proxy, and
uses the stack's isolated smoke database/listener. Both successful switch
create responses are dropped; oxsum must recover by GET without reissuing.
An explicit duplicate refund is also refused by the switch and repeated reads
preserve identity/amount. The runner changes only in-memory routing, leaves
the persistent stack configuration intact, and shuts down its proxy/listener
and own database. No real PSP or model is called; provider-delivered callbacks
and browser checkout remain distinct acceptance.

### Legacy deposit refund recovery smoke

After preparing #757's pinned local dummy stack, run:

```bash
python3 scripts/deposit-refund-smoke.py \
  --sandbox /absolute/path/to/infra/hyperswitch \
  --binary /absolute/path/to/oxsum
```

The runner reuses the sandbox's isolated database/listener harness and creates
one legacy deposit. It drops the successful refund POST response, checks exact
retrieval recovery and repeated local reversal, then applies duplicated locally
signed callbacks. It requires the dummy-only fixture marker and stops its own
proxy/listener and drops only its scratch database. The legacy deposit's wallet
units are reported separately from provider minor units; no order conversion is
implied. No real PSP, model, automatic callback or browser claim is made.

### Sale editor verification (#731)

Run `node --experimental-strip-types --test web/tests/sale-draft.test.ts` on
Node 22+ for exact money/rule round trips, bounds, invalid precision and UTC
half-open schedule boundaries. Build with `npm --prefix web run build`.
`cargo nextest run -p oxsum-core --test sale_books --test sale_heads -p oxsum-server --test business_prices`
checks authority, concurrent revisions/windows, pinned snapshots, signed sale
binding and all production-router operations. Browser acceptance additionally
requires creating, publishing and activating a book, two customer assignments,
a retained draft after conflict and a call whose exported charge matches preview.
### Collection configuration and exact money checks

`OXSUM_DISPLAY_CURRENCY` and `OXSUM_DISPLAY_RATE_PER_CREDIT` bootstrap Business
collection terms once when both are configured. After persistence, change
collection through the owner's Business settings; editing env or a customer's
display preference does not reprice payments. No configured collection terms
means new provider/manual fiat orders fail closed. Existing orders retain their
pinned conversion. Legacy credit-denominated admin receipts retain their units.

Check the touched exact decimal/quote helpers with:

```bash
node --experimental-strip-types --test web/tests/money.test.ts
```

The script uses the project's supported Node 22+ runtime and no network or paid
service. Backend collection fixtures explicitly adopt their rate; setting only a
customer display preference no longer configures collection.

### Recover a historical plan-refund allocation

`cargo run -p oxsum-core --example recover_plan_refund -- --refund <uuid>
--session-file <private-file>` previews one confirmed plan refund against the
explicit `DATABASE_URL`; `--apply` commits its source-linked correcting entry
and document allocation. A personal business-context `FundsWrite` session is
required. The command does not load `.env`, migrate the database or call a PSP.
The [recovery runbook](plan-refund-recovery.md) defines amount checks, replay,
review suspense and unsupported legacy combinations.


### Plan statement lifecycle locks (#811)

With the normal explicit local PostgreSQL test URLs, run:

```bash
cargo nextest run -p oxsum-core --test plan_statement_locks --test plan_statements --test plan_refunds --test journals --test plan_refund_conservation
cargo nextest run -p oxsum-server --test plan_statements
```

`plan_statement_locks` uses dedicated scratch databases and actual PostgreSQL
transactions. It observes a blocked backend before checking `NOWAIT` ownership;
no fixed sleep decides which operation holds the lock. The refund/review case
pauses a verified refund at the real ledger append lock. The stale-candidate
case holds the owning plan while committing payment or a future due date, then
checks the next sweep leaves the plan unsuspended. The keyset progress test
uses a one-row page with a locked oldest plan, ensuring the next page is still
processed. These fixtures do not contact a payment provider.

### Correct historical duplicate plan usage

After migration 0111, use `cargo run -p oxsum-core --example correct_plan_usage --
--journal UUID --session-file PATH --reason TEXT --key KEY` with an explicitly
supplied `DATABASE_URL`. The default previews without writes; `--apply` commits
one verified correction or an audited refusal. Owner/finance personal business
authority is required. The [runbook](plan-usage-correction.md) defines supported
sources, payable handling, exit status, exact-source review and inert replay.

### Business model editor checks (#818)

`node --experimental-strip-types --test web/tests/provider-draft.test.ts` covers
complete optional/conditional/supplier-cost round trips, exact amount bounds,
mode changes, fallback order and explicit cache expiry. The frontend CI gate
runs it alongside the existing money and sale editor checks. `npm run codegen`
and `npm run build` in `web/` validate the unchanged business contracts and UI.
The current `business_providers` HTTP fixture remains the runtime authority,
replay and no-forbidden-effects check. Browser desktop/mobile, both locales,
keyboard and retry acceptance must be separately observed; a build is not that
evidence.


### Statement financial review (#820)

Run the PostgreSQL conservation, retained-void, admission and lifecycle cases with:

```bash
cargo nextest run -p oxsum-core -p oxsum-server \
  --test statement_review_finance --test plan_statements \
  --test plan_statement_locks --test journals --test plan_refunds
```

The fixtures preserve historical document-only voids and duplicated usage through
normal immutable guards and official ledger APIs. They compare actual Cash,
Receivables and RefundPayable postings, block real wallet admission, and inject a
statement-write fault after the ledger append to prove transaction rollback.
No provider is called. The [evidence](roadmap/operator-launch/evidence/statement-review-finance.md)
records baseline failures and final checks; complete release acceptance is separate.

### Dispute resolution transaction checks (#826)

With an explicit local test `DATABASE_URL`, run:

```bash
cargo nextest run -p oxsum-core --test dispute_atomicity --test refunds --test plan_refunds --test plan_usage_corrections
cargo nextest run -p oxsum-server --test refunds
```

`dispute_atomicity` observes real PostgreSQL blockers while a chargeback waits
at the ledger append lock, then proves a conflicting or same-lost decision waits
behind its owner. It also checks plan-before-dispute ordering with `NOWAIT`,
injects an audit-insert failure after ledger append, and compares original ledger,
journal, statement, dispute and audit snapshots after rollback. Old middle-phase
entries are created through the existing public ledger seam and refused without
new cash. The corrected-source fixture repeats the rollback check for review
suspense. No payment provider or live customer data is used.

### Checkout route regression checks (#835)

With the explicit local PostgreSQL test URL, run core `merchant_routes`,
`merchants` and `payment_orders`, and server `checkout_routes`, `payments` and
`merchants`. The HTTP fixture uses a loopback Hyperswitch-shaped service, tests
new owner-created merchants without environment bootstrap, and counts provider
calls to distinguish recovery from a new checkout. Migration 0112 requires the
full workspace gate before integration. Browser/PSP certification is separate.

### Non-plan chargeback source checks (#840)

With explicit local PostgreSQL URLs and an empty worktree `.env`, run:

```bash
cargo nextest run -p oxsum-core --test nonplan_chargebacks --test refunds --test dispute_atomicity --test bundles --test payment_orders --test merchant_events
cargo nextest run -p oxsum-server --test refunds
```

`nonplan_chargebacks` covers original unspent/spent/repayment funding, later
top-ups, pending reservations, exact partial amounts, fractional rejection,
prior refunds, excessive cumulative case cash, unsupported legacy effects,
missing fulfillment, source locking and atomic promo rollback. Concurrency tests
observe real blocked PostgreSQL backends. No PSP or paid service is called.
The [source-review runbook](dispute-resolution-review.md) describes review
suspense and the remaining #841/#842 historical correction boundaries.

Non-plan fulfillment uses a shared `Db` pre-checkout queue of
`max(1, (max_connections - 1) / 3)` slots; reuse/clone the process handle rather
than creating independent wrappers around one pool. It needs three connections
for the guard and existing independent grant operations; a smaller pool returns
`PoolBusy` before a grant. The production recommendation is still a floor of ten.
Contended source locks do not park pool connections: finance and inbox retries
receive `PoolBusy`; fulfillment retries off-pool within five seconds. Separate
processes coordinate through the same PostgreSQL try-lock. These are bounded
retry semantics, not an unlimited-load guarantee.


### Daily plan recognition

Migration 0113 retains old versions as policy 1 and pins new publications to policy 2. The durable hourly `plan-recognition` job catches up at most 200 eligible periods per pass through `Db::run_plan_recognition`; busy plan rows are skipped without starving later candidates. Current database time is authoritative. Exact period/day keys and the surrounding plan/journal/ledger transaction make retries safe; missed days post on the current UTC booking date with their original service dates retained. No new environment variable or recognition HTTP endpoint is required.

Focused daily accounting acceptance: `cargo nextest run -p oxsum-core --lib -E 'test(plan_recognition)'` with the standard PostgreSQL variables. It uses original immutable service bounds established by a private test-only activation clock, verified merchant fixtures and the real ledger. Legacy financial regression suites explicitly seed historical v1 publications instead of changing current publisher behavior.
