# Source-linked request credit acceptance (#1234)

2026-11-20, local synthetic data; no provider calls or production deployment.

- Full workspace nextest: 2,599 passed, 6 runner-filtered skips, 491.637 seconds.
- Seven focused PostgreSQL core tests cover concurrent bounded credits, exact
  retry, changed intent, bonus/unknown/manual/corrupt source refusal, both audit
  failure rollbacks, scoped transactions/proofs, receipts, facts and sealed days.
- One HTTP test covers preview/apply, purchased credit, source-key safe receipt,
  sibling proof denial, changed replay and session revocation.
- An additional focused evidence publication test passed after the full run:
  the bounded posting-period capture commits the correction artifact plus both
  source/credit entries. Public mint still requires a closed period; yesterday's
  bundle separately excludes today's credit.
- Pinned fmt, strict core/server Clippy, OpenAPI contract/codegen and production
  frontend build passed. Independent agent review approved after source,
  privacy, audit, period attribution, evidence and retained-intent fixes.
- Browser: previewed a 2,286-minor historical charge, confirmed a 100-minor
  credit, deliberately dropped its successful server response, reloaded, found
  the source and recovered the exact retained intent. Database remained one
  credit totaling 100. Receipt shows 2,286 original / 100 credit / 2,186 net;
  readable export includes credit hash and actual ledger ordering.
- At 390 CSS pixels, document width is 390 and wide tables retain tabindex=0
  horizontal scroll regions. Screenshots show the retained uncertain intent
  and recovered mobile receipt. Only seeded demo credentials/data were used.

Financial source postings and historical proofs are unchanged. Plan fees,
monthly debt and cash refunds are unaffected. Period credits are a separate
figure; plan-fee reconciliation explicitly excludes them. This engineering
acceptance is not live supplier certification or independent operator adoption.
