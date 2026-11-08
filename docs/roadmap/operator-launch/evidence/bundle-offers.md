# Published bundle authority — development evidence

Scope: #774, under #768 / N10 #736 / #725. Branch `fix/bundle-offers`, based on main `0314cbd`. This records development evidence, not an integrated release or real PSP certification.

## Implemented boundary

Commercial staff publish immutable bundle versions with an exact fiat price, purchased credit, promotional credit and optional validity in whole days. Publication and retirement require current `commercialWrite`, a reason, revision and replay identity. Other staff with `overviewRead` can inspect history. The customer catalog exposes only on-sale versions. A customer's `purposeRef` selects a version; `promoMinor` is a deprecated matching assertion, never grant authority.

Orders pin the complete published version and exact representable conversion before checkout. An insert trigger enforces those terms for old application writers as well. Retirement blocks new orders but preserves original checkout, grant and replay terms. Upgrade does not invent authority for legacy pending bundles: history and reconciliation remain visible, checkout/grant refuse, and automatic fulfillment excludes them so they cannot starve the queue. Already fulfilled history and refund eligibility remain intact.

Purchased funds and promotion have separate idempotent effects. Promotion expires relative to confirmed payment, including immediate expiry in the same transaction when recovery runs after its lifetime. A confirmed partial or full refund cancels remaining promotion without cash reimbursement. Held promotional credit returns to the closed lot and burns; decline/failure leaves promotion intact. Refunds cannot overtake unfinished bundle grant.

The business portal publishes/retires versions with confirmation, exact decimal parsing and unchanged-intent retry keys. Customer checkout selects published terms; order detail retains the original split and expiry. No browser-executed UI acceptance is claimed here.

## Executed checks

- 52 PostgreSQL boundary checks across seven bundle/refund/promotion/order suites passed in 10.007 seconds.
- 22 PostgreSQL upgrade/core/HTTP checks passed in 4.484 seconds. The upgrade fixture applies actual pre-0102 migrations, retains historical rows and effects, applies the new migration twice, and proves the old-writer fence.
- Strict workspace Clippy passed after the final recovery-queue change (4 minutes 53 seconds).
- Frontend TypeScript/Vite build, exact-decimal Node test, workflow actionlint and repository formatter passed. The frontend CI job runs the exact-money test on Node 22.
- Full workspace nextest passed: **1,818 tests, 6 opt-in skips, 232.217 seconds**, with explicit PostgreSQL and doubleentry PostgreSQL URLs and `--no-fail-fast`. An earlier 719-pass run stopped on the expected-table inventory's incorrect alphabetical placement; the inventory was corrected without changing the migration or relaxing the assertion.

The initial 16-test invocation without a database URL is excluded from PostgreSQL evidence. All results above used explicit local PostgreSQL URLs. The earlier interrupted full run never reached test execution and is not a passing run.

## Remaining integration and external checks

Integrate collection authority #765 so business currency/revision/history policy constrains checkout while bundle quantities remain offer-owned. Integrate refund correlation #759, provider recovery #760 and legacy reversal #763, then rerun the combined paths. These independent changes are not represented as merged by this branch.

Actual business/customer/finance browser execution remains pending because the task browser space disappeared. Real PSP sandbox credentials are absent. This change used only local fixtures and no model requests or payment charges. The final candidate must be a reviewed integrated SHA with executed full CI and artifact-level acceptance.
