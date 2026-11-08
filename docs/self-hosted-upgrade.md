# Self-hosted upgrade and rollback

Upgrading a self-hosted oxsum is pulling a new image and restarting;
the schema migrates forward on the way. Rolling back is restoring the
pre-upgrade dump — migrations are one-way, and this runbook exists because
"downgrade the image" is only half the answer (issue #619).

The rule that shapes everything below: **the newest binary migrates, and
migrations are forward-only.** A restored dump of an older release migrates
again — `oxsum --migrate-only` is idempotent — but there is no path that
*un*-applies a migration. A schema-changing rollback requires a restore. A retained artifact with the
exact same schema may be restarted only after the operator has also checked
behavior and data compatibility; equal schema alone does not prove that policy.

## Before you upgrade

```bash
# 1. A backup you have verified — the rollback is this file.
env -u DATABASE_URL CONTAINER=oxsum-core-postgres-1 POSTGRES_DB=oxsum \
  scripts/backup.sh /private/backups

# 2. What you are running now, for the compare afterwards.
curl -s http://127.0.0.1:3000/api/v1/version
# {"data":{"version":"0.1.0","gitSha":"…","schemaVersion":"0095_plan_statements"}}
```

The pair now requires an explicit container/database or PostgreSQL URL, a private
version-2 portable manifest, and a fresh restore target. No `.env` is sourced or
existing database dropped. See [backup and restore](operator-backup-restore.md)
for target selection, credentials, old-backup compatibility and retained failure
receipts.

The manifest's `schemaVersion` tells you exactly which schema level the dump
holds; the version endpoint's `schemaVersion` tells you the level the *new*
binary will migrate to.

## Upgrade

```bash
# Point the deployment at the new image.
$EDITOR deploy/.env          # OXSUM_IMAGE_REF=ghcr.io/allurecurtain/oxsum@sha256:<verified-digest>

docker compose --env-file deploy/.env -f deploy/docker-compose.core.yml pull
docker compose --env-file deploy/.env -f deploy/docker-compose.core.yml run --rm oxsum-migrate
# expect: schema migrated to <new version>

docker compose --env-file deploy/.env -f deploy/docker-compose.core.yml up -d
docker compose --env-file deploy/.env -f deploy/docker-compose.core.yml ps
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
docker compose --env-file deploy/.env -f deploy/docker-compose.core.yml logs -f oxsum-migrate
```

## Managed host deployment and compatible rollback (#810)

The manual Deploy workflow is independent of Release. It takes an explicit
`ghcr.io/allurecurtain/oxsum@sha256:<digest>`, source SHA and successful candidate
run/attempt. It downloads and validates the candidate online, requires observed
runtime version evidence, checks registry config/layer identity and produces a
trusted plan. It has only Actions/content/package read permissions; it cannot
build or publish. The target must have Linux Docker with `image save --platform` support
(API 1.48+), Python 3.10+, GHCR pull
authority, `/opt/oxsum/oxsum.env` and a private mode-0700
`/opt/oxsum/deployment-state`. Pin the host key in `DEPLOY_KNOWN_HOSTS`; the
workflow never trusts an unauthenticated `ssh-keyscan` result.

For direct operation, first prepare the plan on a trusted operator machine:

```bash
python3 scripts/deployment_plan.py /path/to/candidate-bundle \
  --sha <source-sha> --run <candidate-run> --attempt <attempt> \
  --image-ref ghcr.io/allurecurtain/oxsum@sha256:<digest> \
  --output /new/deployment-plan.json
```

Transfer that plan and the reviewed helper sources through an authenticated
channel. A plan is a transport record, not a signature: do not accept an edited
or third-party plan as provenance. The workflow creates and transfers it in the
same trusted job. On the intended Linux host:

```bash
python3 scripts/deploy-candidate.py deploy --plan /trusted/deployment-plan.json \
  --env-file /opt/oxsum/oxsum.env --state-directory /opt/oxsum/deployment-state \
  --port 3001 --receipt /new/deployment-receipt.json
```

The helper pulls the digest and saves it to check actual config/platform/source
and layer bytes before any downtime. It snapshots the operator environment once
for both containers, runs config preflight, stops the managed service, migrates
with that exact digest, starts it and checks both `/readyz` and all three version
fields (`gitSha`, package `version`, `schemaVersion`). It also checks the landed
container's immutable image reference. A successful receipt and current/previous
state are retained privately. The operator file hash pins the database/key
configuration without logging its contents. The helper refuses an unmanaged
existing `oxsum` container; it never adopts one by guessing its provenance.

A failed migration, start or readiness check creates no success receipt and
never triggers automatic rollback. The service may remain stopped after a
migration failure; inspect the host privately and recover explicitly. A failed
post-start verification can leave the new container running but unaccepted.
Retained state remains the last accepted deployment, so a subsequent command
refuses drift rather than pretending the failure succeeded. Back up before any
upgrade, and resolve the failure before resuming managed deployment.

To move a legacy unmanaged service to this path, first complete its backup and
restoration rehearsal, prepare the verified replacement plan, then explicitly
stop/remove the old container during the authorized maintenance window. Preserve
its image reference, environment and backup separately. Run the helper as the
first managed deployment; it records no invented previous receipt. Until a
second managed deployment succeeds, managed rollback has no retained predecessor.

For a **same-schema** retained predecessor, the operator must confirm behavior
compatibility. No environment/database/key change is allowed across the current
and previous successful receipts:

```bash
python3 scripts/deploy-candidate.py rollback \
  --image-ref ghcr.io/allurecurtain/oxsum@sha256:<retained-previous-digest> \
  --source-sha <retained-previous-source-sha> --confirm-compatible-rollback \
  --env-file /opt/oxsum/oxsum.env --state-directory /opt/oxsum/deployment-state \
  --port 3001 --receipt /new/rollback-receipt.json
```

This selects only the stored previous artifact, pulls it by digest, and repeats
all config/migration/readiness/version checks. The migration command is a no-op
at the same schema. A mismatched schema is refused: `/readyz` requires exact
migration-version equality, even when a newer schema is merely additive. Follow
the separate-database restoration procedure below for schema changes. No tool
here performs an automatic down migration or restores over a live database.

## Rollback

A schema-changing rollback restores the pre-upgrade dump and pins the old image digest. The
database *contents* come back to the backup's instant — everything written
since is gone, which is why the backup is step 1, not a nicety.

```bash
# 1. Restore the pre-upgrade dump into a fresh database name — never over
#    the live one. The script refuses both mistakes.
env -u DATABASE_URL CONTAINER=oxsum-core-postgres-1 POSTGRES_DB=oxsum \
  scripts/restore.sh /private/backups/oxsum-backup-<utc>-<id>.manifest.json oxsum-rollback

# 2. Bring the restored schema to the level the rollback binary serves
#    (the dump held an older level; --migrate-only closes the gap the old
#    image expects).
docker run --rm --network <reviewed-database-network> \
  --env-file /private/rollback.env \
  ghcr.io/allurecurtain/oxsum@sha256:<retained-old-digest> --migrate-only
# rollback.env must point DATABASE_URL at oxsum-rollback on that network and
# retain the original sealing/signing keys. A host env assignment alone is
# not automatically passed through docker run.

# 3. Point the deployment at the restored database and the old image.
$EDITOR deploy/.env
#   OXSUM_IMAGE_REF=ghcr.io/allurecurtain/oxsum@sha256:<retained-old-digest>
#   POSTGRES_DB=oxsum-rollback

docker compose --env-file deploy/.env -f deploy/docker-compose.core.yml up -d
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


## Development artifact recovery evidence

The reusable [artifact recovery rehearsal](artifact-recovery-rehearsal.md) takes
explicit binaries and a verifier tarball, freezes their hashes and exercises real
stream drain, schema upgrade, separate PostgreSQL restore, retained credentials
and signing identity, compatible same-schema rollback, old proofs and new calls.
The [recorded development run](roadmap/operator-launch/evidence/artifact-recovery.md)
is a small isolated fixture; it does not certify the current release candidate,
all previous releases or production recovery objectives.
