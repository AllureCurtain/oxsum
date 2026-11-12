---
name: Security report
about: Report a vulnerability class. Do not include secrets, exploits, or customer data.
labels: security
---

## Class of problem

<!-- Name the affected surface (route, config flag, or workflow) and the trust
boundary it crosses: customer to money, staff capability, tenant isolation,
secret handling, or proof integrity. Describe the shape, not a working exploit. -->

## What a safe report includes

- The version or commit you looked at, if you know it (`OXSUM_IMAGE_REF` or a git SHA).
- What an attacker would already need (for example "a customer session" or "the operator token").
- The outcome you are worried about, in one or two sentences.

## What to leave out

- `OXSUM_SECRET_KEY`, `OXSUM_ADMIN_TOKEN`, database credentials, or any sealed provider or rail credential. Describe the shape ("a leaked `oxs-int-` credential"), never the value.
- Customer personal data, real payment card or bank details, or a real ledger's contents.
- A working exploit, proof-of-concept request, or steps that would compromise a deployment you do not operate.

Private vulnerability reporting is preferred when the repository has it enabled. This template is the public fallback while it is off. See [SECURITY.md](../../SECURITY.md).
