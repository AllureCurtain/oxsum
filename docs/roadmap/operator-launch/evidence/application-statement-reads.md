# Bounded application statement reads (#939)

Date: 2026-11-10. Parent #747 / #725, N21 / J18.
Source: feat/application-statement-reads on PR #938 (`028c241b`).
Environment: local PostgreSQL scratch databases; HTTP router tests and synthetic
read-boundary fixtures are distinct from source-backed payment/refund assertions.
No external model, PSP, receiver or publication activity.

Application-only billingRead adds customer statement discovery and exact detail.
Current namespace/customer authority and existing credential revocation locking
share one repeatable snapshot. Archived history remains, open statements are
excluded and foreign/missing/open cursors return 404. Lists are newest-first,
limited to fifty; detail includes all lines up to one thousand. PostgreSQL measures
headers/lines before transferring variable data, and refuses over 4 MiB with 409.
No partial successful document, wallet opening, payment reservation or evidence
marker is introduced. The original customer statement types retain exact source
references and integer credit-minor amounts; checkout fiat remains separate.

Development validation:

- A 37-case focused core/HTTP/checkout/statement/billing run passed 36 and exposed
  an incorrect newly added assertion that every payment effect was a statement
  line. Original ledger behavior correctly separates AR from suspense. The test
  now compares the full read with the established customer detail and requires
  all original AR-bearing effect links; the corrected mixed payment/refund/loss
  case passed (1.579s). All 37 selected cases have passing evidence.
- Final three statement core tests passed (1.158s), covering current/legacy scope,
  expiry/revocation, archive, foreign/open/missing cursors, no ledger/payment/
  evidence side effects, 1000/1001-line limits and large line/header refusal.
- Strict core/server all-target Clippy passed (36.41s) after consolidating a test
  fixture module loaded twice; pinned format and diff checks passed.
- Five OpenAPI authority contract tests, generated types and production frontend
  build passed; the existing large-chunk warning remains.

No database schema or writer changes. Reuse unchanged prerequisite full-workspace
evidence; test affected application/statement/payment/HTTP reads and strict changed
crate Clippy. Full joined notification/application candidate validation remains.

Independent review, protected integration and real external example/candidate J18
are separate acceptance. This API-only slice adds no new portal flow.
