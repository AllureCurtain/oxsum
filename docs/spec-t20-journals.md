# T20 — Plan fee journals and revenue allocation (issue #611)

Design decisions for the implementation. Follows T19's shape: a period exists
only because its authorization does; the journal records what that period
*cost* once it closes. Follows T17's correction model: `credit_notes`,
`disputes`, `finance_audit` are reused, never forked.

## The question

A `planPeriod` order pays money; a `plan_periods` row runs the entitlement.
Nothing recorded what a closed period *billed* — the invoice the customer or
staff can read back, the revenue vs receivable vs liability the ledger must
keep separate, and the record a refund/dispute/correction hangs on. T20 adds
the per-period invoice journal.

## The model: billing in arrears, with deferred revenue

The spec's two clauses — "period close writes journal" and "payment applies
journal" — only reconcile if the fee bills *in arrears*: the order prepays,
the close bills. So:

- A fulfilled `planPeriod` order prepays its period: `debit Assets:Cash /
  credit Liabilities:UnearnedPlanFees`, posted inside
  `activate_plan_period`'s transaction beside the period the order
  authorized. Plan-fee cash never enters the wallet pools.
- The close (boundary rollover, `activate_plan_period`'s renewal arm,
  boundary cancel — all sharing `journal_closing_period_in`) writes the
  journal + its accrual in the same transaction that marks the period:
  charges `debit Assets:Receivables / credit Income:PlanFees`, discounts
  `debit Income:Discounts / credit Receivables`, promo `debit Equity:Promo /
  credit Receivables`. Then the prepay reclassifies `debit
  UnearnedPlanFees / credit Receivables` up to the total.
- What the prepay did not cover stays `posted` — the open receivable, the
  same fact the plan's `past_due` state carries read from the ledger side.
- A later fulfilled order settles open journals oldest-first (`debit Cash /
  credit Receivables`, `paid_order_id` pinned at completion), then books the
  remainder as its own period's prepay. Arrears collect before new money
  defers.

Five accounts join the ledger's fixed set: `Assets:Receivables`,
`Income:PlanFees`, `Income:Discounts`, `Liabilities:UnearnedPlanFees`,
`Liabilities:RefundPayable`. Revenue, receivable and liability never blur —
a journal read can always answer billed vs owed vs held.

## Schema (migration 0094)

### `oxsum.invoice_journals`

The per-period invoice header.

- `journal_id` PK; `customer_plan_id`/`organization_id`/`business_id` the
  scope (denormalized so customer and staff reads need no join);
  `plan_period_id`/`period_index` the period it bills (period nullable for a
  staff reissue).
- `state`: `draft → posted → paid | void`. `draft` exists only inside the
  close transaction; `posted` unpaid; `paid` settled; `void` cancelled.
- `total_minor` the lines' sum; `settled_minor` what the clearing legs
  covered — `paid` when settled reaches total.
- `paid_order_id` the order whose payment completed settlement (the
  refund/dispute link); `disputed_minor` the contested figure;
  `replaces_journal_id` a reissue's predecessor; `void_reason`.
- `posted_at`/`paid_at`/`voided_at`.

Partial unique `(customer_plan_id, period_index) WHERE state <> 'void'` — at
most one live journal per period, so a replayed close is a no-op and a void
frees the slot for a reissue. `invoice_journals_posted` is the dunning read.

### `oxsum.invoice_journal_lines`

Append-only itemization: `base_fee` the sold version's price;
`usage_overage` the period's settled overflow at par (sold_plans carries no
overage rate — the settled charge is the price); `discount` a credit note or
a grant's write-off; `promo_credit` promo covering the fee; `tax` reserved.
Signed `amount_minor` — charges positive, credits negative — and `source_ref`
the fact the line came from (period id, credit-note id, order id).

## Immutability — trigger-enforced, never convention

- **Lines**: UPDATE/DELETE raise outright. Corrections append, nothing
  rewrites.
- **Journal**: DELETE raises always. Identity, customer, period, currency
  and `replaces_journal_id` are frozen. `total_minor` may move only on a live
  journal and only to the lines' exact sum — the consistency the immutability
  guarantee leans on — and on a `paid` journal it may only *fall*, which is
  exactly the refund correction (a `paid` total may never rise).
- **State walk**: `draft → posted | void`, `posted → paid | void`,
  `paid → void`, plus `paid → posted` *only* while `disputed_minor` rises —
  the lost dispute's receivable restore, never a silent erasure. A `void`
  row is immutable outright.

The consequence the writer honors: the close walks `draft → posted → paid`
as two UPDATEs (the machine allows no `draft → paid` hop), and the recharge
raises `disputed_minor` in the same UPDATE that walks `paid → posted` — the
guard's one backward step demands it.

## Corrections route through the journal

- **Refund on a planPeriod order**: skips `refund_debit` — plan-fee cash was
  never wallet money. `apply_refund_to_journal_in` finds the journal the
  order paid (`paid_order_id`) or prepaid (the period's `authorization_ref`),
  appends the credit note as a `discount` line, posts `debit
  Income:Discounts / credit Cash` (paid) or `credit Receivables` (posted),
  drops `total_minor`/`settled_minor`, and marks the note `applied` with the
  correction entry's id. A mid-period refund finds no journal — the close's
  own applied-notes scan picks it up instead; the two paths are disjoint in
  time, so no note counts twice.
- **Dispute**: `dispute_opened` bumps `disputed_minor` on the journal the
  order paid. `lost` re-charges the receivable — `debit Receivables / credit
  Cash` under `dispute-recharge/{dispute_id}` — and walks `paid → posted`,
  the owed figure restored as past-due. A non-plan order's lost dispute keeps
  T17's `CreditLine` recharge untouched.
- **Staff void** (`POST /business/journals/{id}/void`, `FundsWrite`):
  reversing lines append so the bill nets to zero; the mirroring entry posts
  — the open receivable reversed on `posted`, the settlement unwound to
  `Liabilities:RefundPayable` on `paid` (what the customer is owed back) —
  `void` lands with the reason and a `journal.void` `finance_audit` row. A
  replayed key answers the standing voided journal.

Every posting derives its idempotency key from the journal or order id
(`plan-journal/{id}/accrual`, `payment-order/{order}/journal/{id}`,
`payment-order/{order}/prepay`, `plan-journal/{id}/prepay-settle`,
`order-refund-plan/{refund_id}`, `dispute-recharge/{dispute_id}`,
`plan-journal/{id}/void`), so a replayed close, activation or event books
once ever.

## API

- `GET /api/v1/org/plan/journals`, `GET /api/v1/org/plan/journals/{id}` — the
  customer's billing read (`Personal` capability, like `/org/plan`).
- `GET /api/v1/business/journals{?state,customerPlanId,periodIndex,after,limit}`
  and `…/{id}` — the staff cross-organization surface under `ReportsRead`;
  `state=posted` is the dunning work list.
- `POST /api/v1/business/journals/{id}/void` — `FundsWrite`, body
  `{idempotencyKey, reason}`.

## The deferred-recognition note

A22's "daily remainder" reads as the schedule a *period* recognizes: the
prepay defers the whole fee, the close recognizes it at the boundary — one
recognition event per period, integer-exact, never prorated mid-period
(D06's no-pro-rating rule holds). A finer daily-grain schedule would need a
separate recognition ledger the spec does not ask for; the journal's
`posted`/`paid` pair is the honest daily answer — the unearned balance sits
on `Liabilities:UnearnedPlanFees` until its period closes.

## T21 handoff

Statements and credit-line dunning read the receivable `Assets:Receivables`
carries: the `posted` journal is the open balance, `disputed_minor` the
contested share, `paid_order_id` the order a chargeback names, and
`invoice_journals_posted`/`?state=posted` the work list an overdue sweep
drains. `replaces_journal_id` is where a void-and-reissue statement
correction links its successor.
