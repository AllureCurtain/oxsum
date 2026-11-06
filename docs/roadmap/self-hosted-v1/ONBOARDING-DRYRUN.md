# Onboarding dry run — fresh operator to first verified bill

This is the reproducible walkthrough of the self-hosted journey the release
claims: a new operator installs oxsum, initializes the business, creates a
customer, sells a plan and takes a payment, then the customer verifies their
own evidence. Every step cites the runbook or script that owns it; steps are
marked **[executed]** where they were run against a live stack on 2026-11-06
and **[verified]** where the step is proven by the named test suite or
script rather than a manual run in this session.

The end-to-end scripted version of this path is
`scripts/release-candidate-smoke.sh` (T29): it performs steps 1–8 against a
throwaway compose project and finishes with `check-verify-consumer.sh`. Run
it instead of typing this by hand when you want the gate itself.

## Step 0 — Host prerequisites **[verified]**

- Docker + Compose plugin ≥ v2.20, a repository checkout (or `deploy/` +
  `scripts/` + `docs/self-hosted-install.md`), 1 GB free shm for postgres.
- Proven by `install_ops.rs` fixtures running on Docker PostgreSQL 17.

## Step 1 — Configure **[verified]**

```bash
cp deploy/operator.env.example deploy/.env && chmod 600 deploy/.env
# set POSTGRES_PASSWORD, OXSUM_ADMIN_TOKEN, OXSUM_SECRET_KEY
scripts/check-secrets.sh deploy/.env
scripts/check-config.sh  deploy/.env
```

`install_ops.rs::check_config_validates_env_without_a_database` executes the
same parse the operator runs; `check-config` refuses unknown variable names
and warns on `OXSUM_DEMO_SEED`.

## Step 2 — Boot postgres + migrate **[verified]**

```bash
docker compose -f deploy/docker-compose.core.yml up -d postgres
docker compose -f deploy/docker-compose.core.yml run --rm oxsum-migrate
```

`install_ops.rs::migrate_only_brings_a_fresh_database_current` executes
`--migrate-only` against a fresh database and asserts the schema reaches the
latest migration; `previous_version_fixture_upgrades_intact` covers the
upgrade direction.

## Step 3 — Start + readiness **[verified]**

```bash
docker compose -f deploy/docker-compose.core.yml up -d oxsum
curl -s localhost:3000/readyz        # ready (NNNN_<latest>)
curl -s localhost:3000/api/v1/version
```

`install_ops.rs::readyz_distinguishes_migrated_unmigrated_and_stale` and
`version_reports_build_identity_without_a_credential` execute the same
endpoints and their truth table.

## Step 4 — Initialize the business **[verified]**

```bash
curl -X POST localhost:3000/api/v1/admin/business/setup-tokens \
  -H "authorization: Bearer $OXSUM_ADMIN_TOKEN" -d '{"idempotencyKey":"…","ownerEmail":"owner@example.com"}'
curl -X POST localhost:3000/api/v1/business/setup/register \
  -d '{"token":"…","businessName":"…","email":"owner@example.com","password":"…"}'
```

Executed by `business_setup_fresh.rs` / `business_setup.rs` (setup-token mint,
one-use register, expiry, races) — the smoke script drives the identical
calls over HTTP.

## Step 5 — Invite staff, create a customer **[verified]**

Owner session → staff invitation (`business_staff.rs`,
`invitation_consent.rs` — consent required, capability set at invite) →
customer invitation (`POST /api/v1/business/customer-invitations`,
`invitations.rs`) or self-register once `OXSUM_CUSTOMER_SELF_REGISTER` is
declared (validated at boot against intent).

## Step 6 — Configure channel + price + plan **[verified]**

`business_providers.rs` / `provider_destinations.rs` (channel credential,
destination policy) → `business_prices.rs` / `terms.rs` (versioned sale book)
→ `plans.rs` (sold plan + binding). The consumption example
`examples/consumption/` self-provisions a loopback upstream + channel + price
through this exact path — 14 PASS on T26's `verify-examples.sh`.

## Step 7 — Customer key + first billed call **[verified]**

Scoped consumption key (`consumption_scope.rs`) → `POST /v1/chat/completions`
with a hold → settle (`chat_flow.rs`, `coordinated.rs`). The consumption
example runs this end-to-end against a seeded stack.

## Step 8 — Payment: stub checkout or manual receipt **[verified]**

Stub rail checkout (`merchant_events.rs`, `webhooks.rs` — order → provider
event → fulfill once) or finance manual receipt
(`business_console.rs` refund/audit flows, `financial_authority.rs` denial
matrix). `examples/webhook-tools/` drives signature + replay against all
three rails — green on T26's gate.

## Step 9 — Statement + evidence bundle **[verified]**

Journal close → `plan_statements.rs` snapshot → `GET
/api/v1/org/evidence-bundles` mint (`evidence_bundles.rs` — 22 tests incl.
share tokens, 410-on-dead-token, download budget). Executed above in this
session: `evidence_bundles` suite 9/9 green.

## Step 10 — Customer verifies independently **[executed]**

```bash
bash scripts/check-verify-consumer.sh
```

Executed 2026-11-06 in this worktree: packed `verify-oxsum-bill@0.1.0` →
clean install → genuine v2 bundle verifies offline → tampered / missing-row /
wrong-org / wrong-key / wrong-archived-head all rejected with named steps.
`fetch was disabled and never called` — the check is provably offline.

## Operator-side extensions

- Upgrade: `docs/self-hosted-upgrade.md` — pull → `--migrate-only` →
  readiness, plus `previous_version_fixture_upgrades_intact` as the executed
  proof.
- Restore: `scripts/backup.sh` + `scripts/restore.sh` into a fresh database —
  `install_ops.rs::backup_and_restore_between_databases_preserves_state`.
- Demo world (isolated fixture only): `OXSUM_DEMO_SEED=true cargo run -p
  oxsum-server -- seed` — refuses a non-empty database; `check-config` warns
  if the flag is set in `deploy/.env`; never enable on a deployment meant to
  serve.
