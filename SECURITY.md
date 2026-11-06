# Security policy

## Supported versions

oxsum ships as a single binary + PostgreSQL (self-hosted). Security fixes
land on `main`; the supported target is the latest merged `main` / release
candidate — there is no backport line while the release gates are open.
Pin a deployed version with `OXSUM_IMAGE_TAG` in `deploy/.env`; upgrade per
`docs/self-hosted-upgrade.md`.

## Reporting a vulnerability

Report privately through **GitHub's private vulnerability reporting**
(Security tab → "Report a vulnerability") on this repository. If that is
unavailable, open an issue describing only the class of problem — never a
working exploit, credentials, or customer data.

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
