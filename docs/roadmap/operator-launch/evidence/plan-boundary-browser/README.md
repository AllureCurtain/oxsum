# Policy-2 plan boundary browser acceptance

Issue #1017, N10/J10. This development run uses four isolated synthetic
customers and a numeric-loopback signed payment protocol fixture. No genuine
PSP, real purchase, elapsed-month observation or final candidate is claimed.

## Original controlled-clock fixture

A temporary in-crate test used the existing `activate_plan_period_at` seam,
which is compiled only under `cfg(test)`. Production activation, quotation,
payment intake and fulfillment are otherwise unchanged. Each original paid
period was created at **2026-10-10 00:00 UTC**, ending **2026-11-10 00:00 UTC**.
The immutable policy-2 accounting snapshot was created in the same transaction
with those exact bounds, original fee **1,000,001 minor**, quota **7,000,000**,
and its payment-order authorization. Payment was received on November 10.
No service bounds, paid invoice, ledger entry or immutable effect was edited.
This specifically exercises a boundary awaiting worker recovery; it does not
prove a customer clicked before a real future boundary.

The ordinary production debug binary ran the browser and jobs. Initially only
the recognition/rollover jobs were scheduled for later; after browser commands,
their mutable job `run_at` values were made due. The real worker closed periods
and recognized fees. The temporary preparation test and quota probe were
removed from the worktree after use; hashes are in the manifest. The initial
accounting rows are retained unchanged in every subsequent snapshot.

## Observed browser and accounting results

| Customer / operation | Observed outcome |
| --- | --- |
| Renewal attempt fails | Actual review/route/order UI; signed `payment_failed`; failed order, still one period; books, invoice, accounting inputs and effects identical to initial snapshot. |
| Replacement attempt expires | Fresh UI payment; signed `payment_expired`; expired order, still one period and no monetary/entitlement effect. |
| Third attempt succeeds | 1.01 USD buys the exact 1,000,001-minor fee. One fulfilled order and period 1, November 10–December 10 UTC. A same-event replay and distinct success event leave periods, accounting, books and invoices identical. Early next renewal is refused. |
| Change | Browser schedules **Boundary changed**. Worker closes original period before swapping sold version; state `renewal_pending`, no fresh quota. New browser payment of 2.01 USD authorizes exact fee 2,000,003, quota 9,000,000 and overage enabled for one new period. Original fee/version accounting remains intact. |
| Cancel | Browser schedules cancellation. Worker moves plan/subscription to `cancelled`, closes original period and creates no new period. Page shows no live plan. Direct domain admission draws zero from cancelled quota. |
| No renewal | Worker expires the original period and marks plan `past_due`, leaving its original window and no new quota. Direct admission may still draw the allowed remainder of that original period, consistent with the contract. |

The bounded admission probe draws/settles **one synthetic quota minor** for the
renewed, changed and expired customers, and zero for the cancelled customer.
It tests quota admission, not a provider call or gateway bill. `quota-probe.json`
records the exact quota-period identities; `final.json` retains the resulting
counters. All actual money amounts come from the signed fixture orders and
production ledger operations. No spendable wallet credit was added.

Before the probe, final net-credit balances were:

| Customer | Cash | Plan income | Unearned plan fees |
| --- | ---: | ---: | ---: |
| Renewal | -2,000,002 | 1,000,001 | 1,000,001 |
| Change | -3,000,004 | 1,000,001 | 2,000,003 |
| Cancel | -1,000,001 | 1,000,001 | 0 |
| Expiry | -1,000,001 | 1,000,001 | 0 |

These integer ledger balances sum to zero for every customer. An original
completed fee is earned once; a new fee remains deferred until delivered days.

## Repairs, source and limits

The actual zero-debt expiry page incorrectly described overdue statement debt.
Issue #1018 / PR #1020 separates renewal/debt guidance. The 390px Billing table
wrapped amounts by character; #1019 / PR #1021 keeps them intact within local
horizontal scrolling. Both independent repair branches passed their own
browser checks; the composed final frontend also passed at 390×844, with
**待续费**, zero debt and intact exact amounts (`composed-billing-mobile.png`).

Initial backend/frontend source is `d3a09475675f69761c1ad1fa193268ade88d9f2b`
(main plus PR #979). Only frontend repairs were composed afterward; the debug
binary serves disk assets. `manifest.json` records original binary and source
identities plus all retained evidence hashes. Original accounting consistency,
failed/expired inactivity, replay equality, period chaining and ledger balance
assertions passed; the frontend build and pinned formatting passed. Existing
unchanged Rust validation is reused; this run adds no production Rust code.

The four guided browser scenarios, signed event intake, actual worker effects
and direct quota probe establish their declared scope. Full fresh-operator,
complete embedded, genuine PSP and exact-candidate J10 acceptance remain in
other tasks. TaskSpace 7 was finished with no kept pages. Both local listeners
18817/18818 were stopped and checked absent. No real provider request occurred.
