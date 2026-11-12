# Security policy

## Supported versions

oxsum ships as a single binary + PostgreSQL (self-hosted). Security fixes
land on `main`; there is no backport line while the release gates are open.
A merged commit is not automatically an accepted release candidate: deployment
requires the exact SHA/artifact gates in the release runbook.
Pin a deployed version with `OXSUM_IMAGE_REF` in `deploy/.env` (the immutable
`ghcr.io/allurecurtain/oxsum@sha256:…` reference from the promotion receipt).
Older checkouts that still set `OXSUM_IMAGE_TAG` must replace that variable
before using the current Compose profile. Upgrade per `docs/self-hosted-upgrade.md`.

## Reporting a vulnerability

Prefer **GitHub's private vulnerability reporting** (Security tab → "Report a
vulnerability") when that feature is enabled for the repository. As of
2026-11-12 it was disabled, so the path that works today is a public issue
from the [security report template](.github/ISSUE_TEMPLATE/security.md):

<https://github.com/AllureCurtain/oxsum/issues/new?template=security.md>

If that chooser entry is missing, open a normal issue whose title starts with
`Security:` and whose body describes only the class of problem. Never include
a working exploit, credentials, or customer data.

Please do **not** include:

- `OXSUM_SECRET_KEY`, `OXSUM_ADMIN_TOKEN`, database credentials, or any
  sealed provider/rail credential — if a report needs a secret, describe
  the shape ("a leaked `oxs-int-` credential"), never the value.
- Customer personal data, real payment card/bank details, or a real
  ledger's contents.
- A working exploit against a live deployment you do not operate.

A useful report names: the affected surface (route, config flag, or
workflow), the trust boundary it crosses (customer→money, staff
capability, tenant isolation, secret handling, proof integrity), and a
reproduction shape. The project answers reports on a best-effort basis
during the pre-release gate period; coordinated disclosure is appreciated.
