# Support

## Self-host operators

Before filing, collect the fixed diagnostic report. Store the operator token
alone in a private file (mode `0600`); do not put it in a command argument:

```bash
python3 scripts/operator-diagnostics.py --url http://127.0.0.1:3000 \
  --token-file /private/operator-token --env-file deploy/.env \
  --output /private/diagnostics.json
```

The output contains build/schema identity, health states, configuration presence
and aggregate recovery queues. It excludes addresses, identities, amounts,
payloads, logs and secret values. Without `--token-file`, only public health and
version are collected. `--env-file` is optional and reports fixed presence
groups; it is parsed as text, never executed. Both input files must be private
regular files. The output must be a new path and is created with mode `0600`.
Review the report before sharing: counts and configuration flags can still be
operationally sensitive. See [diagnostics and recovery](docs/operator-diagnostics.md)
for field meanings and next steps. A successful collection is not proof that
providers, mail or payments work.

- Installation / upgrade / restore: follow [the installation runbook](docs/self-hosted-install.md) and
  [the upgrade/restore runbook](docs/self-hosted-upgrade.md) first — the "If boot refuses" section lists
  the refusal lines and their fixes.
- Config problems: `scripts/check-config.sh deploy/.env` and
  `scripts/check-secrets.sh deploy/.env` diagnose placeholders, typos and
  half-configured sets before you file.
- Backup/restore: `scripts/backup.sh` writes a private version-2 manifest (snapshot
  schema/counts, size, SHA-256 and timing) against an explicit target. Use the
  [backup/restore procedure](docs/operator-backup-restore.md); review its counts
  and the restore receipt before sharing. Never attach the database dump.
- File at <https://github.com/AllureCurtain/oxsum/issues/new> with
  `self-hosted` context and the reviewed diagnostic report. Do not attach complete
  Compose logs or environment files. Inspect startup logs locally when needed;
  share only a reviewed excerpt with credentials, URLs and customer data removed.

## Customers of a hosted deployment

Retain the export, expected hash, trusted operator key, earlier head and verifier
artifact/checksum while they are available. These inputs permit later offline
verification within the [proof scope](docs/proofs.md); see
[the customer guide](docs/verify-your-bill.md). If verification *fails* on
a genuine export, that is a reportable event: keep the exported bundle file
intact (it is evidence, do not edit it) and report the verifier's named
rejection (`ArtifactMismatch`, `MissingArtifact`, `Malformed`, …) to your
operator — the rejection text names which check failed.

## Security issues

Never file publicly — see `SECURITY.md`.

## What a good report contains

1. The reviewed sanitized diagnostic report or public build identity.
2. The exact request (or UI action) and the response code/body — redact
   tokens, emails, amounts if sensitive.
3. Whether a retry changed the result (retain the idempotency key privately and
   describe reuse; do not trigger another real payment or paid inference to
   produce a support report).
4. Which environment: self-hosted compose, dev worktree, or the public demo.

A failed verification report should name the exact verifier result and whether
head signatures, price/sale binding, arithmetic and anchored completeness were
checked, absent or failed. Keep the original export; use a sanitized separate
fixture for public reports. Contact your own operator for account/payment
questions. The public demo and historical acceptance records do not establish
current credentials, service levels or certification of your deployment.
