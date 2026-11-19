# Billing workflow implementation and acceptance

This program implements the 2026-11-20 billing handoff. It does not authorize
production deployment or claim live provider certification or operator acceptance.

| Area | Existing foundation | Verified gap / issue | Acceptance evidence |
| --- | --- | --- | --- |
| Readable receipt | Immutable settlements, sale snapshots, operations and verifier | Durable exact-ID lookup and safe portal composition, #1233 | [Local checks, browser evidence and independent review](roadmap/operator-launch/evidence/charge-receipts/README.md); merged PR #1238 |
| Request correction | Transactional append and Business authority | Source-bound purchased-credit append and portal, #1234 | [Concurrency, recovery, evidence and browser acceptance](roadmap/operator-launch/evidence/request-corrections/README.md); independent review approved; merged PR #1239 |
| Versioned policy | Sale-book publication and admission transaction | Immutable offers, explicit consent and admission binding, #1235 | [Consent, admission, cache and browser evidence](roadmap/operator-launch/evidence/billing-policies/README.md); independent review approved; merged PR #1240 |
| Missing usage | Atomic settlement and crash sweeper | Legacy estimation has no finite evidence window, #1236 | [Recovery tests and browser acceptance](roadmap/operator-launch/evidence/usage-recovery/README.md); independent review approved; merged PR #1241 |
| Supplier reconciliation | Provider clients and cost observations | OpenAI parsing/scope/pagination and immutable statement history, #1237 | [Contract, replay, coverage and browser evidence](roadmap/operator-launch/evidence/supplier-statements/README.md); independent review approved; PR gates pending; authentic supplier statement unavailable |

Each issue has independent completion criteria. Historical settlement bytes and
legacy estimation remain unchanged. New exception terms require prospective
customer acceptance. Purchased-credit corrections do not imply cash refunds or
support mixed-funding corrections. Supplier aggregate totals do not establish
request completeness or equality with retail revenue.

External product validation remains tracked by #740 (provider certification) and
#745 (two independent operator pilots). Synthetic fixtures, browser checks and
independent code reviews are engineering evidence, not those external results.
