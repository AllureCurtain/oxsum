# Historical duplicate usage correction

Issue [#809](https://github.com/AllureCurtain/oxsum/issues/809), under
[#779](https://github.com/AllureCurtain/oxsum/issues/779) and the forward repair
[#803](https://github.com/AllureCurtain/oxsum/issues/803). This operation corrects
one evidenced historical duplicate; it does not implement daily recognition,
statement repayment or a PSP payout.

## Scope and source boundary

Only an original posted/paid base-fee plus positive usage-overage journal is
eligible. The verifier checks its exact AR/PlanFees accrual, original period
window and all corresponding nonzero usage rows against sequenced, same-tenant
Income:Usage postings. It also checks every journal prepayment or historical
Cash/AR payment entry against confirmed same-customer/plan orders, then requires
the original statement's charges and order-linked payment lines to agree.

The report retains source line/entry identities, hashes, postings, period
bounds and payment amounts. Its evidence SHA-256 is included in the appended
correction's description. The correction debits duplicate PlanFees and credits
only the still-open original receivable and any collected excess as payable.
Neither Cash, customer pools nor Income:Usage change. All original rows/entries
and immutable writer guards remain; correction/document/effect/audit writes are
atomic and repeat/concurrent apply posts once.

Void/reissue, refund/dispute, tax/promo/prior corrections, reversed source
entries and missing/inconsistent payment or document evidence explicitly
refuse. Preview writes nothing. An apply refusal records its precise reason in
finance audit without money/document changes. Local CLI and core operation use
current personal owner/finance authority. No provider I/O or broad mutation is
performed.

## Opening/action/closing matrix

All figures use ledger minor units. Original fee 2,500,000 plus duplicated usage
1,500,000 produces a 4,000,000 bill, while usage income was already 1,500,000.

| Original collected | Correction Cr AR | Correction Cr RefundPayable | Closing bill / applied payment / open AR |
| --- | --- | --- | --- |
| 0 | 1,500,000 | 0 | 2,500,000 / 0 / 2,500,000 |
| 1,000,000 | 1,500,000 | 0 | 2,500,000 / 1,000,000 / 1,500,000 |
| 2,500,000 | 1,500,000 | 0 | 2,500,000 / 2,500,000 / 0 |
| 3,000,000 | 1,000,000 | 500,000 | 2,500,000 / 2,500,000 / 0 |
| 4,000,000 | 0 | 1,500,000 | 2,500,000 / 2,500,000 / 0 |

Each row debits PlanFees by 1,500,000. Cash stays at its opening value and
Income:Usage stays 1,500,000. The unpaid fixture deliberately leaves original
cash deferred, showing that this repair cannot consume an unrelated allocation.

## Validation

Initial implementation on `de3bbca` passed 19 focused PostgreSQL tests and
1,905 complete workspace tests (7 existing opt-ins/old-base H1 skipped), plus
strict workspace Clippy. Independent source review identified the need to
retain the correction-to-later-refund/chargeback boundary; this was added before
PR acceptance.

The expanded 52-test PostgreSQL run passed with zero skips: 13 correction
cases plus identity, refund and plan-refund regressions. It covers both source
orders, request/correction race, pre-provider fences, later full-cash review
suspense and replay, and void after correction. The final rebased implementation on main `45a833e` (source head `59401b2`)
passed all 1,912 workspace tests in 247 seconds, with six existing opt-in tests
skipped and all H1–H6 financial regressions active. Strict workspace Clippy
passed; an initial stale shared-target metadata result was corrected by
refreshing this crate's root mtime, with no implementation suppression.
Formatting, diff whitespace and 44 local documentation links passed.

Independent source review covered the original correction and the later-cash
source-map increment. It reported no remaining introduced blocker. Unsupported
combined histories still require explicit review, and later confirmed cash in
suspense requires manual allocation before any further payout.
The tests build actual source entries through wallet settlement and the ledger
engine's append API. Original journal/statement fixtures use ordinary writer
guards; no ledger tables are edited and no immutable triggers are disabled.
No production upgrade, genuine PSP or candidate acceptance is claimed.
