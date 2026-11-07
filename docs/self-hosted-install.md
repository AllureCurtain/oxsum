# Self-hosted installation

One binary plus one PostgreSQL, run by Docker Compose. This runbook takes a
host from nothing to a serving oxsum deployment; the upgrade and restore
procedures have their own runbook (`docs/self-hosted-upgrade.md`), and the
development quick start stays in `docs/development.md` — this file is the
*deployment* path.

What an install produces:

- `postgres` — PostgreSQL 17, the only stateful service (all of oxsum's state
  is database rows: the shared `oxsum` schema plus one `ledger_<tenant>`
  schema per organization).
- `oxsum-migrate` — a one-shot container that runs `oxsum --migrate-only` and
  exits; the serving container refuses to start until it has.
- `oxsum` — the serving binary on `127.0.0.1:3000` by default.

## 0. Prerequisites

- Docker with the Compose plugin (`docker compose version`), v2.20 or later
  — the `service_completed_successfully` dependency condition needs it.
- A copy of this repository (or just the `deploy/` directory, this file, and
  `scripts/` — the compose files reference nothing else).
- 1 GB free memory for PostgreSQL's `/dev/shm` (the compose file sets it;
  restores build indexes in parallel and a smaller `/dev/shm` fails them).

## 1. Configure

```bash
cp deploy/operator.env.example deploy/.env
chmod 600 deploy/.env
$EDITOR deploy/.env
```

The minimum a boot needs:

```dotenv
POSTGRES_PASSWORD=<openssl rand -hex 32>
OXSUM_ADMIN_TOKEN=<openssl rand -hex 32>
OXSUM_SECRET_KEY=<openssl rand -base64 32>
```

- `POSTGRES_PASSWORD` — the only database credential; the compose file builds
  `DATABASE_URL` from it for the oxsum containers.
- `OXSUM_ADMIN_TOKEN` — the operator token on `/api/v1/admin` and `/metrics`.
  The initialization calls below authenticate with it.
- `OXSUM_SECRET_KEY` — seals provider and rail credentials stored in the
  database. Required the moment a channel or payment rail exists — which the
  initialization below can create — and **losing it strands every sealed
  credential**. Back it up with the same care as the database.

Check the file before booting:

```bash
scripts/check-secrets.sh deploy/.env   # placeholders, permissions
scripts/check-config.sh  deploy/.env   # completeness, typos
```

`check-config` fails on any variable name no example documents — a typo'd
key is a silent misconfiguration — and warns if `OXSUM_DEMO_SEED` is on.
Demo data is opt-in (`OXSUM_DEMO_SEED=true`), only ever lands on an empty
database, and a deployment you mean to run must never set it.

### Where secrets live

`deploy/.env` is a Docker Compose env_file, plain `KEY=value` lines, mode
600, never committed (`deploy/.env` is gitignored). That is the supported
shape — every variable the server reads lands there.

Docker *secrets* (file-mounted under `/run/secrets`) are the stronger
alternative for a swarm or a secrets-managed host. The server reads
environment variables only; to use Docker secrets, translate them in the
compose service:

```yaml
    secrets: [oxsum_admin_token]
    entrypoint: >
      sh -c 'export OXSUM_ADMIN_TOKEN=$$(cat /run/secrets/oxsum_admin_token)
             && exec oxsum'
```

Either way: **never** commit a real value. `check-secrets.sh` catches the
placeholder values the examples ship, and refuses a world-readable file.

## 2. Boot the database

```bash
docker compose -f deploy/docker-compose.core.yml up -d postgres
docker compose -f deploy/docker-compose.core.yml ps
```

Wait for `postgres` to show `(healthy)` — `pg_isready` is its healthcheck.

## 3. Migrate the schema

```bash
docker compose -f deploy/docker-compose.core.yml run --rm oxsum-migrate
# expect: schema migrated to the latest migration included in this artifact
```

`oxsum --migrate-only` applies every pending migration and exits. It is
idempotent — running it again on a current schema is a no-op that still
prints the version — and it is the *same* migration pass the serving binary
runs at boot; the compose dependency just makes the step visible and lets an
operator run it deliberately. A failed migration refuses with the file named
and leaves the database at the last good version, never half-applied.

## 4. Start the server

```bash
docker compose -f deploy/docker-compose.core.yml up -d oxsum
docker compose -f deploy/docker-compose.core.yml ps
```

Expected:

```text
NAME                        STATUS
oxsum-core-postgres-1       Up (healthy)
oxsum-core-oxsum-1          Up (healthy)
```

`(healthy)` on `oxsum` is *readiness*, not liveness — the healthcheck polls
`GET /readyz`, which answers 200 only when the database answers and its
schema is current. Probe the endpoints yourself:

```bash
curl -s http://127.0.0.1:3000/healthz          # ok       — process up
curl -s http://127.0.0.1:3000/readyz           # ready with the schema required by this artifact
curl -s http://127.0.0.1:3000/api/v1/version   # {"data":{"version":...,"gitSha":...,"schemaVersion":...}}
```

`/api/v1/version` is the deployment's name-tag: the binary's package
version, the commit it was built from, and the schema level it migrates to.
Record it — the upgrade runbook reads the same endpoint to confirm a pull
landed the intended build.

## 5. Initialize the business

Everything until now is infrastructure; this step mints the deployment's
business and its owner through the T03 setup API. Merchant configuration is a
separate step; owner setup does not silently create a working payment route.

### 5.1 Mint a setup invitation (operator token)

```bash
curl -s -X POST http://127.0.0.1:3000/api/v1/admin/business/setup-tokens \
  -H "authorization: Bearer $OXSUM_ADMIN_TOKEN" \
  -H "content-type: application/json" \
  -d '{"idempotencyKey":"'"$(uuidgen || cat /proc/sys/kernel/random/uuid)"'",
       "ownerEmail":"owner@example.com"}'
```

```json
{"data":{"id":"…","businessId":"…","ownerEmail":"owner@example.com","token":"<setup-token>","expiresAt":"…"}}
```

The `token` is the invitation; it expires and it is shown once.

### 5.2 Accept ownership in the browser

Open `http://127.0.0.1:3000/app/setup`. Supply the setup token privately to the intended owner; do not put it in a link, screenshot or log. The token is email-bound, expires after 30 minutes and can initialize the business only once.

A new owner enters the business name, invited email and a password of at least 12 characters, then explicitly accepts ownership. An existing user chooses **Already have an account? Sign in first**, completes the normal password/TOTP flow, and returns to setup to paste the token and accept as the displayed identity. Public signup never selects an owner.

After acceptance, sign in if necessary and select **Open business setup** to enter business context. Customer registration remains invitation-only. The form keeps its draft on an error; refreshing or leaving intentionally clears the token. If the response was lost, sign in with the intended account and reopen `/app/setup`: a current staff membership offers the console without repeating setup. If setup is still incomplete and the token expired, the deployer mints another token with a new idempotency key.

The API equivalents remain `POST /api/v1/business/setup/register` for a new identity and session-cookie `POST /api/v1/business/setup` for an existing user. The browser uses these same endpoints and their authority rules; see [API conventions](api.md#initial-business-owner-consent-629).
The owner logs into `http://127.0.0.1:3000/app` with that email and
password. Select Business context and open `/app/business`. The portal's
current editors and the Business APIs manage capabilities; browser setup and
onboarding extensions are tracked by #725. The legacy getting-started checklist
is not a complete readiness verdict. Never put the deployment token in a
customer browser. An existing identity signs in and accepts the setup token
through `POST /api/v1/business/setup` instead of registering again.

### 5.3 Optional: merchant payment rails

If `deploy/.env` completes a rail set (`OXSUM_STRIPE_*`,
`OXSUM_AIRWALLEX_*`, `OXSUM_HYPERSWITCH_*` or `OXSUM_X402_*`), boot with the
payment profile so the declaration is explicit:

```bash
docker compose -f deploy/docker-compose.core.yml \
               -f deploy/docker-compose.payment.yml up -d oxsum
```

Pair each rail's credentials with its merchant bootstrap:
`OXSUM_<RAIL>_MERCHANT_ACCOUNT_ID` and `OXSUM_<RAIL>_MERCHANT_MODE=test|live`.
`OXSUM_RAILS` (comma-separated) is the declared-intent check: name exactly
the rails you configured, and the boot refuses either direction of a
mismatch — a declared rail with no credentials, or credentials no
declaration names.

## 6. Verify

- `GET /api/v1/version` still answers; `gitSha` is the build you pulled.
- Log in as the owner, select Business context and verify the business identity
  and actual allowed actions. Verify the configured route and sale terms
  separately; a merchant row or reachable endpoint does not certify checkout.
- A first wallet request: register a customer organization
  (`POST /api/v1/auth/register`) once the business enables self-registration,
  or invite it from the owner session (`POST /api/v1/business/customer-invitations`).

## If boot refuses

The server names the variable in its refusal — the compose log carries the
exact line:

```bash
docker compose -f deploy/docker-compose.core.yml logs oxsum-migrate oxsum
```

- `OXSUM_* … are set together, or none` — a half-configured set (mailer,
  rail, OAuth). Complete the set or remove the part you set.
- `OXSUM_RAILS declares … but its OXSUM_… set is not complete` — the
  declared/intended mismatch above.
- `OXSUM_ADMIN_TOKEN must be at least 16 characters` — the placeholder is
  still in `deploy/.env`.
- `oxsum --check-config` inside the image runs the same parse without
  touching the database:

  ```bash
  docker compose -f deploy/docker-compose.core.yml run --rm --no-deps \
    oxsum --check-config
  ```

## Backup from day one

```bash
scripts/backup.sh backups/
```

produces `backups/oxsum-backup-<utc>-*.dump` plus a `.manifest.json` naming
the schema version, organization count and SHA-256. The dump contains live
secrets — encrypt at rest. `scripts/restore.sh` and
`docs/self-hosted-upgrade.md` cover the restore half.
