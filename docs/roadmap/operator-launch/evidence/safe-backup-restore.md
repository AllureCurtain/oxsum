# Explicit private backup and restore (#827)

The N17.b production pair no longer discovers Compose containers, executes `.env`,
uses different connections for dump and metadata, accepts unsigned incomplete
manifest assertions, or deletes an existing drill target. Shell entry points now
share one Python standard-library implementation with explicit container or URL
selection, exported snapshot metadata, private portable version-2 manifests,
frozen byte verification, fresh-database creation, transactional restore and
private success/failure receipts. See [operator instructions](../../../operator-backup-restore.md).

## Executed evidence

- Ten Python fault tests passed: explicit target selection, URL/credential argv
  boundary, strict manifest/duplicate JSON, size/hash/symlink/permission rejection,
  existing and unsafe target refusal, parameterized creation, transaction flags,
  retained failure/unknown-create receipts, snapshot publication order,
  non-overwriting atomic JSON, error redaction and a partial-line timeout.
- `cargo nextest run -p oxsum-server --test install_ops`: seven passed, including
  real full-schema backup/restore preserving a customer and its credential,
  previous-version migration, exact readiness and config checks. No production
  Rust implementation changed; the fixture no longer drops a pre-existing
  target before invoking restore. Strict workspace Clippy passed.
- Bash syntax, pinned formatting and workflow lint passed.
- `python3 scripts/rehearse-backup-restore.py --output /tmp/oxsum-safe-backup-827-final`
  passed in two separate disposable PostgreSQL containers using pinned image
  `postgres@sha256:266d3c257594e195b309ae9d1805e54c788f1d7b375f56bf6c367aac6c917e7b`
  (PostgreSQL 17.11). Both containers and anonymous volumes were removed; synthetic
  artifacts remain private at that local path. No shared application database or
  existing service was used as source or target for this rehearsal.

The rehearsal exported a real snapshot, inserted a usage row after export, and
confirmed the dump and manifest retained the original 1,000 usage rows while the
source reached 1,001. It moved the pair into another directory, restored on the
other PostgreSQL container, compared synthetic credential/signing values and
ledger rows privately, and refused a second restore to the same target. It also
ran the implementation with genuine local `psql`/`pg_dump`/`pg_restore` and an
explicit TCP URL inside a disposable container, restoring that mode's 1,001 rows.

Measured synthetic fixture: two organizations, one ledger schema, one statement,
zero deposits, 1,000 usage rows; dump **5,822 bytes**, SHA-256
`dd82bd52ee4e7a604ff728e7460862ca2bd4963b61a5e39ec9399464689db806`;
dump **105 ms**, restore **155 ms**. Source database size was 7,837,363 bytes;
`max_locks_per_transaction=64`, `max_pred_locks_per_transaction=64`,
`max_connections=100`, `shared_buffers=134217728` bytes. These are observations
from this tiny synthetic fixture, not capacity recommendations or an RPO/RTO.

The final partial-line read timeout and atomic receipt refinement received
focused fault tests. The dual-container rehearsal is attributed to the prior
transport implementation; it was not rerun solely for documentation changes.

## Remaining acceptance

This unit verifies backup transport and target safety. It does not validate a
release candidate, a representative multi-customer ledger dataset, old bill
cryptographic proofs, real sealing/signing key continuity, active-operation drain,
same-schema artifact rollback, migration duration across supported releases or a
new served call after restore. Those remain N17.c/J16 and final candidate work.
No public publication, provider or PSP action occurred.
