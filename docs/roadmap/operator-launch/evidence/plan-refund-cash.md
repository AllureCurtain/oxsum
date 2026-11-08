# Confirmed plan-refund cash and legacy allocation recovery

Issue [#802](https://github.com/AllureCurtain/oxsum/issues/802), child of #779,
N10/N11. Development source and PostgreSQL fixture evidence, not PSP
certification. No provider, external payment or production upgrade was used.

## Correctness boundary

H2's missing pre-close cash/note and H3's substitution of receivable relief for
cash are fixed for the existing V1 close-time policy. Daily recognition remains
N11 work. Original sold terms and historical ledger entries are retained.
`plan_refund_effects` fixes one source allocation to the original order,
period/journal, credit note, trusted event and entry identity. All local effects
share one transaction. Failed/released rows that later receive a success keep
their terminal history; the cash allocation is still retained and consumes
refundable balance and activation eligibility.

H3's former expected split was incorrect: confirmed PSP cash is the full
refund amount. A historical 4,000 bill / 2,500 paid / 1,500 debt with a 2,000
refund becomes 2,000 billed / 500 paid / the same 1,500 debt, and Cash falls by
2,000. The short H2/H3 reproductions are now active and the full event-driven
matrix lives in `crates/core/tests/plan_refunds.rs`.

## Opening/action/closing examples

All figures are integer ledger minor units. Cash below uses ordinary positive
asset magnitudes; SQL ledger-net test helpers use credits-positive signs.

| Source | Opening | Confirmed action | Closing |
| --- | --- | --- | --- |
| Pre-close V1 | Cash 2,500,000; deferred 2,500,000 | Refund 1,000,000 | Cash/deferred 1,500,000; note applied; close bill/paid 1,500,000, deferred/AR zero |
| Full pre-close | Cash/deferred 2,500,000 | Refund 2,500,000 | Cash/deferred zero; zero-total bill and statement paid |
| Closed paid | Bill/paid 2,500,000; AR zero | Refund 1,000,000 | Bill/paid 1,500,000; AR zero; Cash 1,500,000 |
| Legacy partial bill | Bill 4,000,000; paid 2,500,000; AR 1,500,000 | Refund 2,000,000 | Bill 2,000,000; paid 500,000; same AR; Cash 500,000 |
| Voided bill | Refund payable 2,500,000 | Refund 1,000,000 | Payable/Cash 1,500,000; original void documents retained |
| Exhausted by chargeback | Cash zero; original settled zero; restored AR 2,500,000 | Refund 1,000,000 | Cash -1,000,000; AR unchanged; unallocated review suspense 1,000,000 |

The last row records the actual second external outflow without a negative
payment counter or invented debt. `bookedWithReview` audit retains the
unallocated source and is not a claim of full allocation.

## Legacy repair and isolation

The explicit recovery command defaults to a preview, uses an already
initialized wallet without bootstrap writes, and requires exact refund ID and
personal `FundsWrite` authority. Actual original posting direction decides the
repair. A wrong legacy `Cr Receivables` is corrected with appended cash/AR legs;
a correct legacy `Cr Cash` never causes another cash outflow. Original
`credit_notes.entry_id` is retained. Replays keep one allocation. Unsupported
combined historical corrections refuse for source review rather than guessing.

Shared `legacy_close` fixtures construct the old binary's documents and real
ledger entries through the official engine `append_in_tx` API and ordinary
immutable guards. They do not depend on continued production duplicate usage
billing or direct writes to ledger tables.

## Validation

Initial focused run on base `c97e5f7` plus this implementation:

- 71 PostgreSQL tests passed across `plan_refunds`, `plan_refund_conservation`,
  `journals`, `plan_statements`, `plans`, `plan_payments`, `payment_orders`, and
  `refunds`; one independent H1 reproduction was still ignored on that base.
- Fourteen new refund tests cover verified success/failure/late events,
  pre-close full/partial, paid/void/unactivated, concurrent refunds/duplicates,
  close race, interrupted document write with complete transaction rollback,
  wrong wallet/authorization, immutable allocation, and three legacy repairs.
- Strict workspace Clippy passed. A first Clippy invocation read stale shared
  metadata from another worktree; touching this crate's root forced the correct
  build. One collapsible-if lint was corrected. No tests were hidden or ignored.

Final rebased source checks and integration results are recorded below before
review. N11 reports/daily policy, H1 historical duplicate usage repair and N10's
statement repayment remain separately tracked; this evidence does not close
their acceptance criteria.
