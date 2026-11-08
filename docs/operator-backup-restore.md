# Explicit backup and fresh-database restore

The production pair uses Python 3.10+ and PostgreSQL client tools, either inside
an explicitly selected container or on the caller's PATH. It never discovers a
container, sources `.env`, deletes an existing database or runs migrations.
Choose one connection mode; conflicting container and URL settings refuse.

## Select the connection

For the documented local Compose database, explicitly name the intended
container and source database. The wrapper reads only these supplied variables:

```bash
env -u DATABASE_URL CONTAINER=oxsum-core-postgres-1 POSTGRES_DB=oxsum \
  scripts/backup.sh /private/backups
```

Container mode uses the container's local PostgreSQL socket and user `postgres`
(or explicit `PGUSER`/`POSTGRES_USER`). If socket authentication needs a password,
supply `PGPASSWORD` through a private process environment; it is not placed in
Docker's command arguments. `CONTAINER` names are never guessed. Docker's host
and context settings still select the daemon, so confirm them before running.

For PostgreSQL tools installed locally, use a private regular file containing
one PostgreSQL URL. It must name the host, user and database; password, port and
TLS parameters can be present. No group/other file permissions are accepted.

```bash
env -u CONTAINER -u DATABASE_URL python3 scripts/backup_restore.py backup \
  --database-url-file /private/database-url /private/backups
```

An explicitly exported `DATABASE_URL` is also supported by the shell wrappers.
The URL is decoded into libpq environment fields so credentials never appear in
`psql`, `pg_dump` or `pg_restore` arguments. Ambient libpq settings and password
files are ignored. Supported URL options are `sslmode`, `sslrootcert`, `sslcert`
and `sslkey`; unknown/duplicate options refuse instead of silently redirecting
the connection. TLS certificate files must be available on the selected host.
Container mode supports local Unix-socket PostgreSQL, not a hidden remote target.

Both modes use the same connection for snapshot, metadata, dump and target
checks. An empty `CONTAINER` selects no container; it does not autodetect one.
Database names are bounded ASCII identifiers, with hyphens permitted. Names
outside that subset need a separately reviewed PostgreSQL procedure.

## What completes a backup

One exported repeatable-read snapshot supplies the dump and the manifest's
schema/counts. Writes committed after export do not enter either. The helper
uses a private staging directory, flushes the dump and JSON, then publishes
non-overwriting files with unique names. The `.manifest.json` is published last
and is the completion marker. A dump left alone after interruption is incomplete
and is never accepted by the restore helper.

Both files have mode `0600`; a newly created output directory has mode `0700`.
The dump includes every database schema, sealed credentials and customer data.
Protect and encrypt the pair at rest and in transit. Environment secrets such
as `OXSUM_SECRET_KEY`, `OXSUM_HEAD_SIGNING_KEY`, `OXSUM_ADMIN_TOKEN`, provider
configuration and the selected image/candidate identity require separate private
retention; they are not recovered from a database dump.

The strict version-2 manifest stores a relative adjacent dump filename, byte
size, SHA-256, PostgreSQL numeric version, schema version, five aggregate counts,
dump duration and database size/lock/pool/shared-buffer observations. It stores
no connection URL, user or password. Counts cover organizations, ledger schemas,
statements, deposits and usage records; they are an inventory check, not a proof
of financial conservation. Settings/physical database size are observed during
the snapshot, not transactional logical contents. Copy the pair together to
another directory or host without editing paths. A hash binds the pair's bytes;
it is not a signature or protection against an attacker replacing both files.

## Restore into a fresh target

Select the intended target cluster explicitly. The connection's database is
protected: restore never creates over that name. It also refuses `postgres`,
`template0`, `template1` and every already-existing target, including a previous
failed rehearsal. Use a new name; no helper drops the old one.

```bash
env -u DATABASE_URL CONTAINER=oxsum-recovery-postgres-1 POSTGRES_DB=postgres \
  scripts/restore.sh /private/backups/oxsum-backup-<utc>-<id>.manifest.json oxsum-restored
```

Local URL mode uses the same `--database-url-file` option with `restore` and an
explicit protected/source database in the URL. The helper validates the complete
manifest, freezes the adjacent nonsymlink private dump and checks size/hash
before database operations. A bare `.dump`, old unversioned manifest, missing
checksum, path traversal or extra field is refused. Existing backups remain
valuable: keep them intact and use a separately reviewed manual restoration
procedure, or create a new version-2 backup from the still-available source. Do
not invent new manifest assertions for unknown old bytes.

The target PostgreSQL major cannot be older than the source. Prefer the same
supported PostgreSQL major and suitable client versions; the tools must exist
in the selected execution environment. A parameterized existence check and
quoted `CREATE DATABASE ... TEMPLATE template0` protect target identity. Creation
races refuse. `pg_restore --single-transaction --exit-on-error --no-owner
--no-privileges` restores into that new database; schema/count checks precede a
private success receipt. The receipt records the actual target name, artifact
hash, elapsed time and observed metadata. It contains operational data and
requires the same private handling as the manifest.

On failure after creation, the target is retained and a failed receipt records
the phase. A lost creation response records an unknown creation outcome; inspect
the cluster before retrying. There is no guessed cleanup or automatic drop. A
lost success receipt after restoration is not an accepted restore: verify the
retained target and artifact privately. Restoring a dump executes database
objects from that dump; accept backups only from your trusted operator boundary.

After a verified restore, use the selected old or new artifact's `--migrate-only`
against the restored database, then check exact runtime identity/readiness,
retained signing/sealing keys, old evidence and a new call. The helper does not
perform these acceptance steps, switch production traffic or claim a measured
RPO/RTO for another dataset. See the [upgrade runbook](self-hosted-upgrade.md).

## Rehearse without deleting an existing database

The same-cluster wrapper requires every coordinate:

```bash
env -u DATABASE_URL CONTAINER=oxsum-core-postgres-1 SOURCE=oxsum \
  TARGET=oxsum-rehearsal-fresh OUT_DIR=/private/rehearsal \
  scripts/backup-restore-drill.sh
```

It uses the production pair, retains the restored database and refuses an
existing target. Local URL mode derives the source from the URL and still
requires `SOURCE` to identify the intended source in the wrapper; they must
match. For task-owned synthetic transport tests, run:

```bash
python3 scripts/rehearse-backup-restore.py --output /new/private/rehearsal
```

This creates two disposable PostgreSQL containers from the pinned image,
exercises snapshot isolation across a concurrent insert, moves the pair,
restores into the other container, checks synthetic private/ledger rows,
refuses an existing target and exercises real local-URL libpq mode. It installs
Python only in a disposable container for that mode, then removes its containers
and anonymous volumes. Artifacts remain in the explicit new output directory.
The Compose starting configuration uses `max_locks_per_transaction=1024` and
`shm_size: 1gb`; retain those reviewed settings for multi-schema deployments and
measure requirements on the actual dataset. A tiny fixture at PostgreSQL
defaults does not justify reducing them.

This synthetic rehearsal does not certify candidate upgrades, real account
balances, old bill proofs, key rotation, active-operation drain or serving after
restore. Keep those N17/J16 gates separate.
