# Atomic monthly issuance (#1009)

Base: `2ee706019a02f24b4b8ed4b949b8c5254b27a178`. Branch:
`fix/monthly-issuance-atomic`. Local PostgreSQL; no external provider or payment
requests. This repairs the existing monthly writer before the Business staff
workflow under #1006.

The prior writer committed its ledger marker before updating the document. A
failed update/enqueue left an orphan marker, and later source changes could adopt
it without checking the descriptor. Usage totals, lines and hashes came from
separate snapshots. Five new regression scenarios were executed against the
unchanged base production files and all five failed, including a retained orphan,
a changed digest, accepted incompatible replay and wrong-wallet acceptance.
The controlled repayment case also showed the missing admission serialization.

The repaired path uses one transaction/connection, organization admission/share
then ledger append/document locks, and one materialized SQL snapshot for usage
inputs. Ledger draws/repayments remain stable under the append lock. An already
issued statement returns its original fields and lines; exact legacy markers
replay while incompatible descriptors/marker shapes refuse.

Executed validation:

- **15/15** monthly core tests passed. Five new cases cover injected document and
  event-enqueue failures (no orphan anchor, no changed lines/row/event), concurrent
  exact retry (one marker/event), one-connection operation, immutable historical
  terms/window after later usage, foreign wallet refusal, exact/incompatible
  legacy marker replay, controlled late usage between source and credit reads,
  and a committed repayment before issuance acquires its locks.
- Core complete selection: **861 executed, 860 passed, one known baseline failed,
  zero skipped**. The failure is the unchanged manual-receipt bundle-upgrade
  assertion covered by PR #975 / issue #1002. It is not hidden or called green.
- **13/13** existing monthly HTTP tests passed, including statement generation,
  finalization, repayment, dunning, PDF and independent proof-bundle verification.
- Strict core all-target Clippy, pinned formatting and diff checks passed.
  No new public fields or routes, ledger schema/engine changes or frontend edits.
  Single-crate validation followed the updated test-scope rules; no repeated
  full-workspace build was needed.

These figures are development evidence. Independent approval, protected
integration, staff-authorized issuance/repayment and exact-candidate acceptance
remain pending. The source snapshot does not assert all historical usage has
arrived or that provider-reported counts are independently attested. Existing
payment reconciliation behavior remains outside this focused atomicity repair.
