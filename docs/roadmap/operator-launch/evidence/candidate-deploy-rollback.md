# Candidate deploy + compatible-rollback rehearsal — 2026-11-12

Issues #739 (N13) / #743 (N17) / #744 (N18). Real images on the local
`oxsum-accept` isolated stack. This is **candidate-image deployment** evidence,
not a verified-candidate `deployment_plan` (that still needs authorized GHCR
provenance) and not live-service authority. The host is arm64 Docker Desktop;
the images are `linux/amd64` under emulation — the same qualification the
earlier N13 digest-deploy evidence recorded.

## Environment

- Compose project `oxsum-accept`, isolated `oxsum_accept` PostgreSQL, Mailpit.
- Candidate `ghcr.io/allurecurtain/oxsum:sha-cc16bb8fe5d6875ee2bb054e2c436cc788fc94ef`
  — gitSha `cc16bb8f…`, schema `0131_invoice_corrections`, version `0.1.0`,
  repo digest `sha256:13384f6297bf…` (shared with `sha-candidate-v2`).
- Same-schema prior artifact `ghcr.io/allurecurtain/oxsum:sha-433fc4f4`
  — schema `0131_invoice_corrections`, repo digest `sha256:de73a319b618…`,
  `gitSha` reports `unknown` (no baked revision label).

## Observed deployment sequence

| Step | `OXSUM_IMAGE_REF` | `oxsum-migrate` | `/readyz` | `/api/v1/version` |
| --- | --- | --- | --- | --- |
| baseline | `sha-433fc4f4` | exit 0 | ready | `gitSha unknown, schema 0131, 0.1.0` |
| deploy candidate | `sha-cc16bb8f…` | ran, exit 0 | ready | `gitSha cc16bb8f…, schema 0131, 0.1.0` |
| same-schema rollback | `sha-433fc4f4` | re-check, exit 0 | ready | `gitSha unknown, schema 0131, 0.1.0` |
| re-deploy candidate | `sha-cc16bb8f…` | ran, exit 0 | ready | `gitSha cc16bb8f…, schema 0131, 0.1.0` |

Each transition ran the one-shot `oxsum-migrate` service before `oxsum`; the
container only opened `/readyz` after its schema-current healthcheck passed.
`OXSUM_IMAGE_REF` controlled both migrate and serving images; absent ref fails
rendering. `oxsum._migrations` holds **126** applied migrations throughout; the
single organization and its data persisted across every switch (verified before
and after). No image was rebuilt; the digest `sha256:13384f62…` for the deployed
candidate matched its manifest.

## What this does and does not establish

- Establishes: a real candidate image migrates, serves readiness, reports exact
  `gitSha`/`schemaVersion`/`version`, and a same-schema rollback selects a
  retained compatible artifact and returns to it, on real PostgreSQL.
- Does **not** establish: authorized GHCR-provenance promotion
  (`deployment_plan`), a live public deployment, a schema-change upgrade, or any
  of the J01–J18 portal journeys — those remain their own gates. Host is the
  local arm64 dev machine, not the declared Linux/amd64 reference.
