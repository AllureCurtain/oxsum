# Support

## Self-host operators

Before filing, collect the deployment's identity:

```bash
curl -s http://127.0.0.1:3000/api/v1/version    # version + gitSha + schemaVersion
docker compose -f deploy/docker-compose.core.yml logs oxsum oxsum-migrate
```

The version payload names the exact build; the migrate container's log names
any failed migration file. Paste both (values, not secrets) into the report.

- Installation / upgrade / restore: follow [the installation runbook](docs/self-hosted-install.md) and
  [the upgrade/restore runbook](docs/self-hosted-upgrade.md) first — the "If boot refuses" section lists
  the refusal lines and their fixes.
- Config problems: `scripts/check-config.sh deploy/.env` and
  `scripts/check-secrets.sh deploy/.env` diagnose placeholders, typos and
  half-configured sets before you file.
- Backup/restore: `scripts/backup.sh` writes a manifest (schema version,
  counts, SHA-256); quote it when reporting a restore issue.
- File at <https://github.com/AllureCurtain/oxsum/issues/new> with
  `self-hosted` context, the version payload, and sanitized logs.

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

1. `version`/`gitSha`/`schemaVersion` output.
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
