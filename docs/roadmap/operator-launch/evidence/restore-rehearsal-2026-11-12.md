# Live-database restore rehearsal — 2026-11-12

Issues #743 (N17) / #744 (N18) / J16. Ran `scripts/backup_restore.py`
(backup → restore) against the **live** `oxsum-accept` isolated database into a
separate PostgreSQL target — not the synthetic two-container fixture. This is
development-restore evidence on the local candidate stack, not production
restore authority.

## Observed

- **Backup**: `backup --container oxsum-accept-postgres-1 --database oxsum_accept`
  → manifest `oxsum-backup-20261112T040112Z-eb9c2f75….manifest.json`,
  artifact `sha256:8b708ac0…`, frozen dump verified against size+checksum.
- **Restore**: `restore --container oxsum-accept-restore … oxsum_restored` into
  a separate `postgres:17-alpine` container on the accept network. `restore()`
  created the target from `template0` and refused a pre-existing database —
  the explicit-target fence works.
- **Verified state** (receipt `oxsum-restore-38e6115b….json`): `postgresVersionNum
  170011`, `schemaVersion 0131_invoice_corrections`, counts organizations=1 /
  ledgerSchemas=0 / statements=0 / deposits=0 / usageRecords=0, restoreDuration
  **625 ms**. Restored `oxsum.organizations`=1, `oxsum.users`=1 match source.
- **Existing-target refusal**: a second `restore` into an already-present
  database is refused without dropping it.

## Limits

One initialized organization with no ledger traffic yet — counts are small
because the journey work was still in progress on the same stack. This proves
the portable backup/restore path and the separate-target preservation fence on
the real schema; it is not a measured production data-loss window. A
schema-change upgrade would additionally exercise the migrate step on the
restored copy. Native amd64 host, external (off-cluster) target and a larger
populated dataset remain for the declared-environment acceptance.
