# Self-hosted upgrade and rollback

Upgrading a self-hosted oxsum is pulling a new image and restarting;
the schema migrates forward on the way. Rolling back is restoring the
pre-upgrade dump — migrations are one-way, and this runbook exists because
"downgrade the image" is only half the answer (issue #619).

The rule that shapes everything below: **the newest binary migrates, and
migrations are forward-only.** A restored dump of an older release migrates
again — `oxsum --migrate-only` is idempotent — but there is no path that
*un*-applies a migration. A rollback is a restore, never an in-place revert.

## Before you upgrade

```bash
# 1. A backup you have verified — the rollback is this file.
scripts/backup.sh backups/

# 2. What you are running now, for the compare afterwards.
curl -s http://127.0.0.1:3000/api/v1/version
# {"data":{"version":"0.1.0","gitSha":"…","schemaVersion":"0095_plan_statements"}}
```

The manifest's `schemaVersion` tells you exactly which schema level the dump
holds; the version endpoint's `schemaVersion` tells you the level the *new*
binary will migrate to.

## Upgrade

```bash
# Point the deployment at the new image.
$EDITOR deploy/.env          # OXSUM_IMAGE_TAG=v0.2.0  (or sha-<sha>)

docker compose -f deploy/docker-compose.core.yml pull
docker compose -f deploy/docker-compose.core.yml run --rm oxsum-migrate
# expect: schema migrated to <new version>

docker compose -f deploy/docker-compose.core.yml up -d
docker compose -f deploy/docker-compose.core.yml ps
```

`up -d` recreates `oxsum` on the new image; the running container drains on
SIGTERM (in-flight requests settle, the drain is bounded at nine seconds),
then the new binary starts against an already-current schema.

Verify:

```bash
curl -s http://127.0.0.1:3000/readyz          # ready (<new schema>)
curl -s http://127.0.0.1:3000/api/v1/version  # gitSha moved to the new build
```

Then one real operation — a login, a bill page, a small gateway turn if a
channel exists — before calling the upgrade done. `/readyz` proves the
schema; only traffic proves the service.

### What "mid-migration" looks like

If a migration is long, `oxsum` shows `Up (unhealthy)`/`(health: starting)`
while `oxsum-migrate` is still running — the serving container waits on the
migrator's `service_completed_successfully`. That is the intended shape: the
schema leads, the service follows. Do not `docker compose up` past it by
starting `oxsum` alone; wait on the migrator's log:

```bash
docker compose -f deploy/docker-compose.core.yml logs -f oxsum-migrate
```

## Rollback

A rollback restores the pre-upgrade dump and pins the old image tag. The
database *contents* come back to the backup's instant — everything written
since is gone, which is why the backup is step 1, not a nicety.

```bash
# 1. Restore the pre-upgrade dump into a fresh database name — never over
#    the live one. The script refuses both mistakes.
scripts/restore.sh backups/oxsum-backup-<utc>-*.manifest.json oxsum-rollback

# 2. Bring the restored schema to the level the rollback binary serves
#    (the dump held an older level; --migrate-only closes the gap the old
#    image expects).
DATABASE_URL=postgres://postgres:<password>@localhost:5432/oxsum-rollback \
  docker run --rm --env-file deploy/.env \
    ghcr.io/allurecurtain/oxsum:<old-tag> --migrate-only

# 3. Point the deployment at the restored database and the old image.
$EDITOR deploy/.env
#   OXSUM_IMAGE_TAG=<old-tag>
#   POSTGRES_DB=oxsum-rollback

docker compose -f deploy/docker-compose.core.yml up -d
curl -s http://127.0.0.1:3000/api/v1/version    # gitSha back to the old build
```

The non-database half of rollback is the environment: a dump does not
contain `OXSUM_SECRET_KEY` or `OXSUM_ADMIN_TOKEN` — they are configuration,
not data. The restored deployment needs the *same* sealing key the dump's
credentials were sealed under; `deploy/.env` already holds it if the file
was not rotated between backup and rollback. A rotated key is a different
procedure — `docs/development.md` ("Key rotation").

### In-flight state after a restore

A restore puts back what the dump held: settled books are exact; open holds
older than `OXSUM_HOLD_TIMEOUT` sweep to zero on the next pass — the
designed answer to a gateway that went away mid-turn, and no turn is
charged that nobody can settle.

## Restore into a separate environment

Disaster recovery is the same procedure pointed at a different cluster:
stand up the compose stack on the new host, `scripts/restore.sh` the dump
into its postgres service, `--migrate-only`, then boot. `deploy/.env`
travels with the deployment — the dump needs the same `OXSUM_SECRET_KEY`
to open the credentials inside it.

## Measured, not promised

The restore drill (`scripts/backup-restore-drill.sh`, and
`scripts/backup.sh` + `restore.sh` as the production pair) is the evidence:
on a development database of ~2 organizations it runs in seconds, on a
database with thousands of ledger schemas the dump and restore take minutes
each — the runbook says *measure your database*, because the recovery time
is dominated by how many `ledger_*` schemas `pg_restore` rebuilds. No RPO or
RTO figure is promised here that a drill has not measured on your data.
