# Integrated payment replay and upgrade regression repair (#793)

Base: `c79a588455b31e9cc7ceafd9341806a83f1dfc62`, whose CI run 42446328206 failed all three integration shards. This is development evidence, not candidate acceptance.

The unchanged bundle request may omit its deprecated promotion assertion (zero), while the stored order carries the nonzero published grant. The integrated replay comparison incorrectly treated these representations as different terms. Replay now validates only an explicit nonzero assertion against the original grant, preserving purpose, amount, currency, reference and plan-quote checks. A retired offer's admitted order still recovers; no new order or grant is created.

Collection and bundle upgrade checks now assert the new `bundle_offer` and `plan_payment` columns are present and null for historical orders, then compare every original field unchanged. This preserves the history invariant without treating a newly added nullable field as a rewritten old value. Existing unsafe-writer and pending-order fences remain checked.

The current API/product/user guides now agree on authority: bundles use published offers, plan periods use the customer's sold quote, and other conversions use Business collection terms. Duplicate order documentation and a retained merge-conflict marker were removed; historical decision entries remain historical.

## Validation

With explicit `DATABASE_URL` and `DOUBLEENTRY_DATABASE_URL` pointing at the local PostgreSQL 17 maintenance database, testkit created and removed isolated scratch databases:

- Baseline `bundles`, `bundle_upgrade`, `collection_upgrade`, `plan_payments`: 14 tests, 10 passed and four failed; no skips. Both replay tests and both upgrade snapshots reproduced.
- The same selection after repair: 14 passed, no skips. Added assertions check replay before/after retirement, matching/nonmatching explicit promotion, changed amount/currency/reference, no second order and missing/altered plan fingerprints. Existing concurrent retirement, paid grant, expiry and refund checks remain active.
- Strict workspace Clippy (`--all-targets -- -D warnings`): passed.
- Independent source review of the diff found no blocking issue; it also identified the corrected authority documentation. This was an agent review, not a fabricated GitHub reviewer approval.

Sanitized local command logs are in ignored `local-notes/operator-launch/repair-validation/payment-{baseline,focused,clippy}.log`. No real provider, payment or external mail request was made; the shared model budget is unchanged.

## Remaining integration

The statement/prepayment conflict between #784 and #780 is separately tracked in #795. Run the combined workspace suite without fail-fast and follow merged-main CI after those focused repairs; the original shards stopped early, so their failure list was not complete. #779's three financial defects, N11 daily recognition and final browser/candidate gates remain open.
