# Development guide

## Requirements

- Rust stable: no toolchain pin — rustup uses whatever `stable` resolves to. The workspace MSRV is `rust-version = "1.98"` in the root manifest; cargo will refuse to build on anything older.
- Docker: runs local PostgreSQL 17, also used by doubleentry's Postgres tests

On Windows with Git Bash, Git's own `/usr/bin/link` shadows MSVC's `link.exe` and linking fails with `link: missing operand`. Either build inside a VS Developer shell, or export the `vcvars64.bat` environment into the current shell first.

## First setup

```bash
cp .env.example .env
docker compose up -d
cargo build --workspace
```

### Pre-commit hook (optional)

`.githooks/pre-commit` runs `cargo fmt --all --check` plus a *scoped* `cargo clippy`
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

`cargo run -p oxsum-server -- seed` fills a fresh deployment with a believable world and exits
without serving: the `demo@oxsum.local` owner and a second member `dev@oxsum.local` (both under
the password `demo-password-1234`), the member under a committed-spend budget, three API keys —
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
nothing else. With `OXSUM_SECRET_KEY` set and no catalog at all, the seed creates the demo channel
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

A successful run was exercised against Base Sepolia on 2026-10-25 (issue #372) — transaction `0xe25f9c399dbb2f620d236246ac382b8d7cf9760bc6ec4d184442a7bff0cc4038` on `https://sepolia.basescan.org` — so the row sits at **live-verified** in `docs/rails.md`. The wallet needs only faucet USDC; the facilitator fronts the gas.

Two wire details the script exists to absorb, both of which cost a debugging round-trip during bring-up:

- The USDC Base Sepolia contract's EIP-712 `name` is the string `"USDC"`, **not** `"USD Coin"` — Circle renamed between FiatToken v1 and v2, and the domain separator commits to whichever string the contract signed under. A signer using the wrong name produces a valid-looking signature that `ecrecover` rejects at settle time with `FiatTokenV2: invalid signature`.
- `coincurve`'s recoverable signature carries the recovery id `v ∈ {0, 1}`; Ethereum's `ecrecover` and the x402 wire want `v ∈ {27, 28}`. The script shifts it — leaving it unshifted surfaces as `ECRecover: invalid signature 'v' value` from the facilitator.

### Real Airwallex rail exercise

The Airwallex rail (issue #285) is verified at the API and webhook layers against `api-demo.airwallex.com`. The hosted-page checkout is deliberately not mounted — see `docs/decisions.md` — so the exercise proves the deposit's `pending → confirmed → credited` walk, not the payer's browser leg.

`.env` carries the rail's five vars:

```
OXSUM_AIRWALLEX_CLIENT_ID=…
OXSUM_AIRWALLEX_API_KEY=…
OXSUM_AIRWALLEX_API_URL=https://api-demo.airwallex.com       # demo, not prod
OXSUM_AIRWALLEX_CHECKOUT_URL=https://checkout-demo.airwallex.com
OXSUM_AIRWALLEX_WEBHOOK_SECRET=whsec_…
```

Then the walk:

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

The Hyperswitch rail (issue #319) is a self-hosted [Hyperswitch](https://github.com/juspay/hyperswitch) switch running beside oxsum, not a Juspay-hosted dependency — the hosted sandbox's registration has been broken upstream for a while, and a local switch is what a production deploy would run anyway.

The deployment assets live in `infra/hyperswitch/` — a slimmed `compose.yml`, its `config/` and `scripts/`, and `setup.sh`. First time only:

```bash
./infra/hyperswitch/setup.sh           # clones juspay/hyperswitch into .repo (gitignored)
docker compose -f infra/hyperswitch/compose.yml up -d
```

The clone is a cache for the migration_runner's mount (the `migrations/` and `justfile` live upstream, not here); delete `.repo/` and re-run `setup.sh` to refresh it. `setup.sh` also normalizes line endings and comments out upstream's `drop_id.patch`, which does not apply to the schema its own branch generates.

First boot runs several minutes of migrations, then the router, the SDK bundle server, the Control Center and `create-default-user`, which seeds `demo@hyperswitch.com` / `Hyperswitch@123`. Sign in at `http://localhost:9000` and read the three values the rail needs — the API key under Developers → API Keys (`snd_…`), `payment_response_hash_key` under Settings → Payment Settings, and the publishable key (`pk_snd_…`). Configure a Stripe connector with a `sk_test_…` key, then fill `.env`:

```
OXSUM_HYPERSWITCH_API_KEY=snd_…
OXSUM_HYPERSWITCH_WEBHOOK_SECRET=<payment_response_hash_key>
OXSUM_HYPERSWITCH_PUBLISHABLE_KEY=pk_snd_…
OXSUM_HYPERSWITCH_BASE_URL=http://localhost:8080
OXSUM_HYPERSWITCH_SDK_URL=http://localhost:9050
OXSUM_PUBLIC_URL=<the public origin the webhook reaches — a tunnel in local dev>
```

Ports avoid oxsum's own Postgres and any local Redis: pg `5433`, redis `6380`, superposition `8081`, router `8080`, SDK `9050`, Control Center `9000`. The SDK origin matters twice: `OXSUM_HYPERSWITCH_BASE_URL` is what the *server* posts payments to, `OXSUM_HYPERSWITCH_SDK_URL` is where the *browser* loads `HyperLoader.js` for the checkout widget (issue #320) — with the hosted sandbox the two are `sandbox.hyperswitch.io` and `beta.hyperswitch.io/v1`. Webhooks need a public URL — `cloudflared tunnel --url http://localhost:3000` or `ngrok http 3000`, then register `<tunnel>/api/v1/webhooks/hyperswitch` in the Control Center and point `OXSUM_PUBLIC_URL` at the tunnel origin.

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
| `OXSUM_DB_MAX_CONNECTIONS` | Optional; size of the one pool every tenant shares (default 20). Each connection is a PostgreSQL backend process, so this is the process's whole budget rather than a per-tenant allowance. Stay below the database's own `max_connections` (PostgreSQL's default is 100); size it at roughly twice the expected number of concurrent gateway requests, floor 10 — saturation answers 429 `POOL_BUSY` with `Retry-After`, never a 500 |
| `OXSUM_ADDR` | Optional listen address; defaults to `127.0.0.1:3000` |
| `OXSUM_UPSTREAM_BASE_URL`, `OXSUM_UPSTREAM_API_KEY`, `OXSUM_MODELS` | The bootstrap channel for a database that has none, set together or not at all. With none of them a fresh deployment serves the wallet only. `OXSUM_MODELS` is JSON: each model maps to `inputPricePerMillion`, `outputPricePerMillion` and `maxOutputTokens`, in minor units per million tokens. `maxOutputTokens` is the ceiling for a request that asks for none and the freeze is computed from it, so it must be the model's real limit rather than a safe guess. A malformed value, or a base URL that is not an http(s) URL, refuses to boot rather than serving a price it had to guess |
| `OXSUM_SECRET_KEY` | 32 bytes, base64, sealing the upstream credentials stored in `oxsum.channels` and the webhook signing secrets in `oxsum.webhook_endpoints` (AES-256-GCM). Required as soon as a channel exists — including one seeded from the environment, which is why setting `OXSUM_MODELS` means setting this too — and for the webhook sender to run at all: without it `POST /api/v1/webhooks` is refused and no worker is spawned. The server refuses to start when it cannot open a stored credential, rather than failing a request that has already been relayed. Generate one with `openssl rand -base64 32`. Losing it means re-entering every upstream credential and rotating every webhook secret |
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
| `OXSUM_SMTP_URL` | Optional; the lettre connection string for outbound mail — `smtps://user:pass@host` for implicit TLS, `smtp://user:pass@host` for STARTTLS-when-offered. The credentials live in the URL, so keep it out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_MAIL_FROM` | Optional; the `From:` mailbox of the verification and reset mails, e.g. `oxsum <noreply@example.com>` |
| `OXSUM_PUBLIC_URL` | Optional on its own, required by the mailer and OAuth client; the deployment's public base URL the mailed links, the OAuth callback and the device grant's `verificationUri` are built on, e.g. `https://wallet.example.com` — no trailing path |
| `OXSUM_GITHUB_CLIENT_ID` | Optional; the GitHub OAuth app's client id — set together with `OXSUM_GITHUB_CLIENT_SECRET` and `OXSUM_PUBLIC_URL`. The callback the GitHub app registers is `<public-url>/api/v1/auth/oauth/github/callback` |
| `OXSUM_GITHUB_CLIENT_SECRET` | Optional; the OAuth app's client secret — keep it out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_GITHUB_WEB_URL` | Tests only; the provider's web origin the authorize redirect and token exchange go to, default `https://github.com` — a stub stands there |
| `OXSUM_GITHUB_API_URL` | Tests only; the provider's API origin, default `https://api.github.com` |
| `OXSUM_TURNSTILE_SITE_KEY` | Optional; the Cloudflare Turnstile site key the register page hands the widget — set together with `OXSUM_TURNSTILE_SECRET_KEY`, a subset is a startup error. Public by design: `auth/methods` publishes it |
| `OXSUM_TURNSTILE_SECRET_KEY` | Optional; the secret `siteverify` authenticates with — keep it out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_TURNSTILE_VERIFY_URL` | Tests only; the siteverify endpoint, default `https://challenges.cloudflare.com/turnstile/v0/siteverify` — a stub stands there |
| `OXSUM_STRIPE_SECRET_KEY` | Optional; the Stripe API key the checkout-session POST authenticates with (`sk_live_…`/`sk_test_…`) — set together with `OXSUM_STRIPE_WEBHOOK_SECRET` and `OXSUM_PUBLIC_URL`, a subset is a startup error. Unset, the rail is absent: `POST /api/v1/topups/stripe` answers 503 and `/api/v1/webhooks/stripe` 404. Keep it out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_STRIPE_WEBHOOK_SECRET` | Optional; the `whsec_…` secret that verifies the `Stripe-Signature` header on `POST /api/v1/webhooks/stripe` — the endpoint Stripe's dashboard registers is `<public-url>/api/v1/webhooks/stripe` |
| `OXSUM_STRIPE_API_URL` | Tests only; the Stripe API origin the checkout-session POST goes to, default `https://api.stripe.com` — `crates/server/tests/stripe.rs` stands a stub there |
| `OXSUM_AIRWALLEX_CLIENT_ID` + `OXSUM_AIRWALLEX_API_KEY` + `OXSUM_AIRWALLEX_WEBHOOK_SECRET` | Optional as a set with `OXSUM_PUBLIC_URL`; all four or none — a subset is a startup error (issue #285). The web-app Developer key pair is exchanged for a short-lived bearer token at `/api/v1/authentication/login` when `POST /api/v1/topups/airwallex` creates a PaymentIntent; the webhook secret verifies the `x-signature` header (hex HMAC-SHA256 of `x-timestamp + body`) on `POST /api/v1/webhooks/airwallex` — the endpoint Airwallex's web app registers is `<public-url>/api/v1/webhooks/airwallex`. Unset, the rail is absent: the intent answers 503, the webhook 404. Keep the secrets out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_AIRWALLEX_API_URL`, `OXSUM_AIRWALLEX_CHECKOUT_URL` | Tests only; the API and hosted-payment-page origins the intent POST and browser redirect go to, defaults `https://api.airwallex.com` and `https://checkout.airwallex.com` — `crates/server/tests/airwallex.rs` stands a stub there |
| `OXSUM_HYPERSWITCH_API_KEY` + `OXSUM_HYPERSWITCH_WEBHOOK_SECRET` + `OXSUM_HYPERSWITCH_PUBLISHABLE_KEY` | Optional as a set with `OXSUM_PUBLIC_URL`; all four or none — a subset is a startup error (issue #319). The API key authenticates `POST /payments` when `POST /api/v1/topups/hyperswitch` opens a deposit; the webhook secret is the business profile's `payment_response_hash_key`, verifying `X-Webhook-Signature-512` (hex HMAC-SHA512 over the raw body) on `/api/v1/webhooks/hyperswitch`; the publishable key is what the browser SDK initializes with. Unset, the rail is absent: the intent answers 503, the webhook 404. Keep the first two out of logs the way `OXSUM_SECRET_KEY` is kept |
| `OXSUM_HYPERSWITCH_BASE_URL` | The switch origin, default `https://sandbox.hyperswitch.io`; a self-hosted deployment points it at its own app server and `crates/server/tests/hyperswitch.rs` stands a stub there |
| `OXSUM_HYPERSWITCH_SDK_URL` | The `HyperLoader.js` origin the top-up page mounts Unified Checkout from (issue #320), default `https://beta.hyperswitch.io/v1` — the hosted sandbox's SDK bundle. A self-hosted deployment points it at its hyperswitch-web service (`http://localhost:9050` for the local sidecar). It travels to the browser in the intent's `sdkUrl` |
| `OXSUM_X402_FACILITATOR_URL` + `OXSUM_X402_NETWORK` + `OXSUM_X402_ASSET` + `OXSUM_X402_PAY_TO` | Optional as a set; all four or none (a subset is a startup error). The real x402 rail (issue #282): `POST /api/v1/topups/x402` delegates verify/settle to an x402-rs facilitator sidecar (`OXSUM_X402_FACILITATOR_URL`, e.g. `http://localhost:8080` or the hosted `https://facilitator.x402.rs`) over its V1 HTTP API. `NETWORK` is the V1 chain name (`base-sepolia` for tests, `base` for production), `ASSET` the accepted token contract (USDC on base-sepolia: `0x036CbD53842c5426634e7929541eC2318f3dCF7e`), `PAY_TO` the deployment's receiving address. The settle answer's chain hash lands on `deposits.facilitator_tx_hash` |
| `OXSUM_X402_VERIFYING_KEY` | Optional; the hex Ed25519 public key the x402 stand-in rail pins as its trusted payer (issue #264) — the test-scheme fallback read only when the facilitator set is entirely absent. Unset both, the rail is absent and the endpoint answers 503; a malformed value is a startup error. The stand-in is documented in docs/decisions.md |
| `OXSUM_E2E_UPSTREAM_BASE_URL`, `OXSUM_E2E_UPSTREAM_API_KEY`, `OXSUM_E2E_MODEL` | Tests only; all three together opt `crates/server/tests/real_upstream.rs` into reaching a real provider — the base URL, the provider's key, and the model name it serves. Unset, the test skips before touching the network; `cargo test` never sets them and neither does CI. Keep the key in the shell environment, never in `.env` or any tracked file — the test seals it into `oxsum.channels` like a real deployment |
| `OXSUM_UPSTREAM_PRICE_SYNC` | Optional; `true` schedules the daily `sync-upstream-prices` job that probes each channel's `GET {baseUrl}/api/v1/pricing` and opens upstream-price-drift rows where the stored `upstream` rates disagree (issue #222). The job also needs `OXSUM_SECRET_KEY` to unseal channel credentials — it is only enqueued when both exist. Default unset: the job never runs, and channels pointing at non-oxsum upstreams are better left unprobed anyway — only an oxsum-compatible catalog answers that path |
| `OXSUM_UPSTREAM_BILL_SYNC` | Optional; `true` schedules the daily `upstream-bill-sync` job that pulls each channel's provider-billed cost for the day just closed and records it beside the day's summed `upstream_cost_minor` in `oxsum.upstream_bills`, flagging drift over the alert threshold as `anomaly.detected` (issue #283). The job also needs `OXSUM_SECRET_KEY` to unseal each channel's `bill_key_sealed` — the admin billing credential an admin channel write attaches with `billKey` — and is only enqueued when both exist. Enabling it without the sealing key is a startup error, the same shape `OXSUM_UPSTREAM_PRICE_SYNC` takes. Default unset: the job never runs and `providerBilledMinor` on the margin view stays null |
| `OXSUM_RETENTION_PROVIDER_RAW_DAYS` | Optional; days a usage row keeps `usage_details.provider_raw` before the daily `retention` job nulls it — the privacy window, since the raw payload may carry prompt fragments. Default 7; the normalized columns are kept forever |
| `OXSUM_RETENTION_DELIVERIES_DAYS` | Optional; days a delivered or failed `webhook_deliveries` row is kept. Default 30 |
| `OXSUM_RETENTION_JOBS_DAYS` | Optional; days a finished `oxsum.jobs` row is kept — `dead` rows included, so the window is also how long `jobs_dead` drift evidence survives. Default 90 |
| `RUST_LOG` | Optional `tracing-subscriber` filter; defaults to `info` — applies to exported traces too when `OXSUM_OTLP_ENDPOINT` is set |
| `OXSUM_OTLP_ENDPOINT` | Optional; the OTLP collector's HTTP endpoint, e.g. `http://localhost:4318` — the exporter appends `/v1/traces` (issue #224). Unset, tracing stays strictly local. Spans carry only the `gen_ai.*` semantic-convention vocabulary — operation, system, model, token counts — never a request id, organization, or key |
| `OXSUM_DISPLAY_CURRENCY` | Optional; the deployment-wide display unit an organization without its own setting renders credits in — ISO 4217, e.g. `USD`. Set together with `OXSUM_DISPLAY_RATE_PER_CREDIT`, a subset is a startup error. An organization's own `PATCH /api/v1/org` setting wins over this default (issue #257) |
| `OXSUM_DISPLAY_RATE_PER_CREDIT` | Optional; the decimal literal one credit displays as in `OXSUM_DISPLAY_CURRENCY`, e.g. `0.0014`. Stored and wired as text — money never goes through a float |
| `OXSUM_DISPLAY_DECIMALS` | Optional; how many decimals the fiat rendering rounds half-up to (0–6). Default 2 |

The mailer trio is all three or none: a subset refuses to boot, because a half-configured mailer would only fail at send time inside a request. Unset, the deployment sends no email — registration answers `verificationSent: false`, forgot-password still answers its indistinguishable 200, and `POST /api/v1/auth/verify/request` alone reports the absence with 503. The GitHub OAuth pair follows the same rule — a client id or secret without `OXSUM_PUBLIC_URL` refuses to boot, and unset the login page draws no GitHub button and the OAuth endpoints answer 404. So does the Stripe trio (`OXSUM_STRIPE_SECRET_KEY`, `OXSUM_STRIPE_WEBHOOK_SECRET`, `OXSUM_PUBLIC_URL`): a subset refuses to boot, and unset, the intent endpoint 503s and the inbound webhook 404s.

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
advisory lock so two starting processes cannot race. A failed migration refuses to boot; there is no
separate migrate command to remember. Ledger schemas are untouched by it: doubleentry's `migrate`
creates `ledger_<tenant_id>` on first use.

## Commands

| Purpose | Command |
| --- | --- |
| Start the database | `docker compose up -d` |
| Start the server | `cargo run -p oxsum-server` |
| Seed the demo world | `cargo run -p oxsum-server -- seed` (populates, prints the credentials, exits — see "Demo data") |
| All tests | `cargo nextest run --workspace` — runs the full suite locally (loads `DATABASE_URL` from `.env` or the shell; `cargo test --workspace` is the fallback and covers doctests). Worth it for cross-crate changes; otherwise CI runs the same coverage on every PR — see "CI" below |
| Focused while iterating | `cargo nextest run -p oxsum-<crate>` — only the crates the change touched; the local pre-push gate is fmt + clippy + these focused runs, with CI holding the full suite |
| Run the end-to-end demo | `python3 demo/demo.py` (needs `pip install -r demo/requirements.txt`; starts its own mock upstream and server) |
| Generative sequences, quick run | `PROPTEST_CASES=50 cargo test -p oxsum-core --test generative` |
| Real-provider end-to-end check | `cargo test -p oxsum-server --test real_upstream` (needs env vars — see "Real-provider smoke check") |
| doubleentry's Postgres tests | `cargo test -p doubleentry --features postgres --test postgres` (spins up its own container via testcontainers; `DOUBLEENTRY_DATABASE_URL`/`DATABASE_URL` set → that server instead, no Docker) |
| Format | `cargo fmt --all` |
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

## Deployment runbook

The deployment is one binary plus one PostgreSQL database; every operational
procedure below is a property of those two pieces.

### Backup

Everything that matters lives in the database: the shared `oxsum` schema and one
`ledger_<tenant_id>` schema per organization. A dump of the whole database is the
backup — do not filter to the `oxsum` schema, or every organization's ledger
comes back empty:

```bash
pg_dump --format=custom --file=oxsum.dump "$DATABASE_URL"
```

Two cluster settings matter once a deployment carries many organizations (one
`ledger_*` schema each):

- **`max_locks_per_transaction`.** `pg_dump` takes a lock per table, and the
  stock 64 per transaction fails with `out of shared memory` once a database has
  grown past a few hundred organizations. Set it to at least 1024 in the cluster
  (`ALTER SYSTEM SET max_locks_per_transaction = 1024`, then restart); the
  Compose database ships that value.
- **Docker's `/dev/shm`.** A restore builds indexes in parallel and needs more
  than the default 64 MB — without it `pg_restore` fails with `could not resize
  shared memory segment … No space left on device`. The Compose service sets
  `shm_size: 1gb`; a self-hosted PostgreSQL wants the host's usual
  `shared_buffers`-sized `/dev/shm`.

`scripts/backup-restore-drill.sh` proves the dump both restores and serves; it
is the next section.

The ledgers are append-only, so a dump is a consistent snapshot by construction:
nothing it captured is ever rewritten underneath the backup. Take the dump
against a quiet hour if the database is busy — `pg_dump` holds its snapshot
isolation for the run — and keep dumps long enough to cover the audit window the
deployment answers for, because a restored ledger is the only way to re-prove an
old bill after data is gone.

Continuous archiving (WAL shipping / PITR) is the production-grade alternative
and works unchanged — the database is ordinary PostgreSQL — but it is the
operator's tooling to run; oxsum ships no backup machinery of its own.

One non-database item survives a dump: nothing. Signing seeds, the credential
seal and the admin token are environment configuration, not data — losing the
dump loses the books; losing the environment loses only what "Key rotation"
below spells out.

### Restore

Restore into an empty database of the same major PostgreSQL version, then start
the server against it:

```bash
createdb oxsum-restored
pg_restore --dbname=oxsum-restored --no-owner --no-privileges oxsum.dump
# point DATABASE_URL at oxsum-restored and start the binary
```

`pg_restore` replays every schema — `oxsum`, every `ledger_*`, the migration
ledger `oxsum._migrations` — so the server boots onto the restored state without
a migrate step. Startup migrations are idempotent: a dump of an older release
restores, then the newer binary's `Db::migrate` applies only what is missing.

After a restore, in-flight state is what the dump held: open holds older than
`OXSUM_HOLD_TIMEOUT` sweep to zero on the next pass, which is the designed answer
to a gateway that went away mid-turn — the client's reservation releases, nothing
is charged for a turn nobody can settle.

### The restore drill

`scripts/backup-restore-drill.sh` dumps the source, restores it into a second
database on the same cluster, and compares the counts that prove the move —
organizations, statements, deposits, usage records and the `ledger_*` schemas —
exiting non-zero on a mismatch. It never restores over its source, and after the
counts match it prints the command that starts a server against the restored
copy.

A recorded run (2026-10-22, local Compose, the development database reset to the
demo world):

```text
$ CONTAINER=oxsum-postgres-1 bash scripts/backup-restore-drill.sh
dumping oxsum to /tmp/oxsum-backup.dump
dump size: 135111 bytes
restoring into oxsum_drill_restore
check                  source     restored
organizations               2            2
statements                  0            0
deposits                    3            3
usage records               0            0
ledger schemas              1            1
the restored copy matches the source.
```

Then the three checks that say the restore serves:

```text
start the server against the restored copy
  DATABASE_URL=postgres://postgres@127.0.0.1:5432/oxsum_drill_restore \
  OXSUM_ADDR=127.0.0.1:3012 cargo run -p oxsum-server

GET  /healthz                                 200
POST /api/v1/auth/login (demo@oxsum.local)    200
GET  /api/v1/bills?limit=1                    the seeded top-ups
GET  /api/v1/entries/{entryId}/proof          the bundle
POST /verify  bundle=… expected_hash=…        "The bill verifies."
```

The same run also produced the two settings above the hard way. The first
attempt ran against a development database that had accumulated 4,192 test
organizations from before the per-suite isolation (about 46,000 tables): the dump
failed at the stock lock limit, the restore failed at Docker's stock `/dev/shm`,
and the container needed minutes to fsync after a restart. After the reset to the
demo world — 2 organizations, 11 ledger tables, 10 MB — the whole drill runs in
seconds. The lesson is the reset, not the drill: a development database left
unreset is what turns a five-second check into an hour, and the reset path is
`scripts/reset-dev-database.sql` (see "Test residue in the development database").

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

Rollback is `docker run` the previous image — with one honest limit: migrations
are forward-only. A release that added a table or relaxed a constraint leaves a
database the older binary tolerates (the schema is a superset of what it
expects), but a release that changed *behavior* — settlement semantics, a
constraint tightened the other way — may not round-trip. The safe procedure:

1. Dump the database before the upgrade (`pg_dump` above).
2. Upgrade; verify `/healthz`, then a real turn.
3. Roll back by restoring the pre-upgrade dump only when the release's changes
   made the schema or the books incompatible — a plain revert keeps the data a
   rollback would otherwise strand behind a constraint it predates.

Every deploy workflow tags the image `sha-<git-sha>` beside `latest`, so the
rollback is `docker pull ghcr.io/allurecurtain/oxsum:sha-<previous>` and restart
— no rebuild. The `deploy.yml` `image_tag` input accepts `sha-<sha>` directly
for exactly this; a manual SSH `docker run` of the same tag does it without
going through CI.

The CI gate builds and smoke-tests the image on every pull request, so a release
that cannot boot never reaches the registry; what it cannot smoke-test is your
traffic — the dump before the upgrade is the answer to that.

### Key rotation

| Secret | Rotates by | What it costs |
| --- | --- | --- |
| `OXSUM_ADMIN_TOKEN` | Restart with the new value | The running admin sessions — the token is the credential, so the old one simply stops working |
| `OXSUM_HEAD_SIGNING_KEY` | Restart with the new seed | Old head and price-checkpoint signatures stop verifying under the new key. The verifying key is published at `GET /api/v1/log/key`, so verifiers fetch the new one — but a head a client archived under the old key is that client's trust anchor; publish rotations so archives know to re-anchor |
| `OXSUM_SECRET_KEY` | **Not a restart** | It seals `oxsum.channels`' upstream credentials and `oxsum.webhook_endpoints`' signing secrets; a new value cannot open them. Rotate by re-entering every channel's upstream key and recreating the webhook endpoints, *then* restart with the new value — or lose the ability to read what was sealed |
| Database credentials | PostgreSQL's own role/password machinery | Update `DATABASE_URL` and restart |
| Organization API keys | `DELETE`/`POST /api/v1/org/keys` | Per-organization, self-service, no restart |
| Service credentials | `DELETE`/`POST /api/v1/admin/service-credentials` | Mint a new one, deploy it to the service, revoke the old one — metered calls fail `UNAUTHORIZED` between revoke and redeploy |

Two of these are the expensive ones by design: `OXSUM_SECRET_KEY` is the root of
what the database holds sealed, and the head-signing seed is the root of what
verifiers trust — both are the kind of secret a deployment protects in a vault,
never rotates casually, and never loses without a plan for the re-entry the
table above names.

## Continuous integration

`.github/workflows/ci.yml` runs on every push to `main` and every pull request that
touches more than documentation (`paths-ignore` skips `docs/**` and `*.md`). Together
the jobs below are the full gate — a regression cannot reach main through a PR, so
the local pre-push bar stays at fmt + clippy + the touched crates' tests (issue
#370). The toolchain comes from `dtolnay/rust-toolchain@stable` — the runner's
stable, no version pin — with the `rustfmt, clippy` components where they are
needed:

- `fmt`: `cargo fmt --all --check`.
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
- `build-tests`: `cargo nextest archive --workspace` compiles every test
  binary once into a `.tar.zst` artifact (issue #276). A second archive builds
  doubleentry's postgres suite with the feature on — its
  `#![cfg(feature = "postgres")]` gate means a plain `--workspace` archive would
  ship an empty binary. `Free disk space` lives here, the only job that still
  compiles the heavy targets.
- `test-shard` (matrix of three): downloads the archive and runs
  `cargo nextest run --archive-file … --partition count:i/3`, so the suite's
  wall-clock is a third of one runner's. Each shard gets its own
  `postgres:17-alpine` service and runs the scratch-database sweep before the
  tests — an interrupted run's residue cannot bleed into the next shard's
  timing (issue #271).
- `test-extras`: downloads the same artifact, extracts both archives, and runs
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
of the workspace's dependency graph. The three test shards and test-extras all
run in parallel once build-tests hands them the archive; `main` requires every
job to pass — a pull request cannot merge while any of them is red.

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
npm --prefix web run codegen # regenerate src/api/schema.d.ts from crates/server/openapi.yaml
npm --prefix web run dev     # vite dev server, proxies /api to localhost:3000
npm --prefix web run build   # emit web/dist
```

The server embeds `web/dist` via `rust-embed` and serves it at `/app`
(`crates/server/src/spa.rs`): extension-less routes fall back to
`index.html`, hashed `assets/` get immutable caching, and a missing asset is
a real 404. Debug builds read `dist` from disk (`debug-embed`), so a
`npm run build` is visible without a cargo rebuild; release builds embed the
directory at compile time — the Dockerfile's `frontend` stage produces it,
so the release image is still one binary. `web/dist` is git-ignored except a
committed placeholder `index.html` that keeps the embed folder present on a
fresh checkout. Styling follows DESIGN.md: its tokens are mapped onto
Tailwind v4 theme colors in `web/src/index.css`.

## Test strategy

- Unit tests: inside each module's `#[cfg(test)]`, covering pure logic such as tenant id validation and configuration parsing.
- Integration tests:
  - `crates/core/tests/` exercises the wallet's invariants on real PostgreSQL: isolation, concurrent no-overdraw, hold/settle, idempotency, tamper detection, restart recovery, concurrent migrate; `crates/core/tests/identity.rs` covers registration, organizations and API keys, including that the plaintext secret is nowhere in the database; `crates/core/tests/sessions.rs` covers login, session authentication, logout and the key role scopes, including that the plaintext token is nowhere in the database and that a session dies with its membership.
  - `crates/core/tests/generative.rs` generates random sequences of top-ups, holds, settlements, replays and key collisions and checks each step against an in-memory model of the wallet account's two layers: the outcome the model predicted, the available balance, that it never went negative, the log size, and a proof for every entry the case wrote (including that a changed amount stops verifying). 1000 cases by default, `PROPTEST_CASES` to narrow it locally; a failure prints its seed and the shortest failing sequence. Since settlement-hold pairing (issue #10) a settlement names the hold it releases, and the oracle predicts `HoldNotFound` for a settlement naming a key that is not an outstanding hold. The suite loads `.env` before reading `DATABASE_URL` (issue #18): with the variable only in `.env` it runs for real, and with no database anywhere it skips all cases with a warning rather than failing.
  - `crates/server/tests/` covers the HTTP layer: key auth, error format, full request chains, and that one organization's key cannot reach another's ledger; `crates/server/tests/sessions.rs` covers the login/logout/session endpoints, the cookie attributes, and the member/owner key rules over HTTP. `crates/server/tests/dashboard.rs` covers the billing WebSocket's contract — the snapshot of open holds, the live events, and that an unknown session is refused; `crates/server/tests/browser_contract.rs` covers the server-rendered sliver (`/verify`, its stylesheet) and the `301` map that sends the retired page paths to `/app` (issue #256).
- doubleentry's own tests: changing `crates/doubleentry` requires all of them passing, including its conformance suite.
- E2E: the full top-up → call → verify flow is covered. `crates/server/tests/chat_flow.rs` drives login, a top-up, one streaming chat turn through a scripted upstream, the billing events, and then fetches the settlement's proof bundle and verifies it with `oxsum_verify::verify_bundle`; `demo/demo.py` runs the same flow end to end with the official OpenAI SDK and prints both proof bundles for `/verify` to check. TODO.md carries the current status.
- Real-provider E2E: `crates/server/tests/real_upstream.rs` runs the same bill path against a provider that is not a stub — registration, a manual-rail top-up, the model catalog, one non-streaming and one streaming completion against the real upstream, then reads the billing record back and verifies the proof bundle, the charge recomputation and the operator-signed tree head with the same `oxsum_verify` calls the browser makes. It is `#[ignore]`d and additionally skips unless `OXSUM_E2E_UPSTREAM_BASE_URL`, `OXSUM_E2E_UPSTREAM_API_KEY` and `OXSUM_E2E_MODEL` are all set, so it never runs in the ordinary gate or CI; the provider key lives only in the shell environment. Run it against a clean development database — the suite writes a channel sealed with its own key and needs `prepare` to open every stored channel:

  ```bash
  docker exec -i oxsum-postgres-1 psql -U postgres -d oxsum -f - < scripts/reset-dev-database.sql
  OXSUM_E2E_UPSTREAM_BASE_URL=https://api.stepfun.com/step_plan/v1 \
  OXSUM_E2E_UPSTREAM_API_KEY=sk-... \
  OXSUM_E2E_MODEL=step-5-preview \
    cargo test -p oxsum-server --test real_upstream -- --ignored --nocapture
  ```

- Pool-saturation load check (issue #235): `a_saturated_pool_refuses_backpressure_never_a_500` in `crates/server/tests/gateway.rs` fires fifty concurrent turns through a three-connection pool against a 250 ms scripted upstream — saturation must answer 429 `POOL_BUSY` with `Retry-After`, never a 500. `#[ignore]`d like the real-provider suite; run it when the pool or the gateway's database access moves:

  ```bash
  cargo test -p oxsum-server --test gateway -- --ignored --nocapture
  ```

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
the `Dockerfile` builds the `web/` frontend, compiles `oxsum-server` (which
embeds `web/dist`), and packages the one binary into the image described above. CI
(`.github/workflows/ci.yml`) is the green gate on a fresh clone, and every pull request
also builds the image and runs it against PostgreSQL
(`.github/workflows/release-image.yml`, issue #49), so the artifact a release ships is
checked long before the release is cut.
