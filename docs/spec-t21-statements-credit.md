# T21 — Customer credit, plan statements and overdue handling (issue #612)

Design decisions for the implementation. Builds on T20's `invoice_journals`
(the accounting-side per-period bill) with the customer-visible *statement*
that wraps it, the overdue → grace → suspend dunning policy, and a credit
limit enforced at admission.

## The naming collision this resolves first

`oxsum.statements` already exists (issue #124): the monthly *usage* statement
an organization owes against when its billing is "borrow first, settle
monthly" — the credit-line receivable for metered usage, with its own
`payment_status` (`pending → paid | overdue | suspended`) and the `dunning_*`
org columns (issue #258). That document is *usage-arrears* — it bills what was
already spent.

T21's statement is *plan-fee*: the customer-facing billing document one
`plan_periods` close produces, wrapping the `invoice_journals` receivable in
customer language. The two are not the same document and must not share a
table — a name collision on `oxsum.statements` would silently conflate the
credit-line meter bill with the plan-fee invoice.

So T21 names its tables `oxsum.plan_statements` and
`oxsum.plan_statement_lines`, and its state vocabulary deliberately reuses the
journal's (`issued`/`paid`/`past_due`/`void`) rather than the usage
statement's (`finalized`/`overdue`/`suspended`), because a plan statement's
lifecycle is driven by the journal's, not by month-end aggregation.

## The model: the statement is the journal's customer face

A journal line says `base_fee` / `usage_overage` / `promo_credit` — accounting
vocabulary. A statement line says `planFee` / `usage` / `discount` /
`refundCredit` / `disputeChargeback` / `paymentApplied` — the customer's bill.
One `plan_statements` row per `(customer_plan_id, period_index)` mirrors one
live `invoice_journals` row; the statement is issued the moment the journal
posts, inside the same transaction the close already runs.

- `due_at` = `issued_at + net_terms` — `net_terms_days` on `customer_plans`,
  default 14, overridable per binding (an enterprise binding may negotiate
  net-30; a self-serve one stays net-14).
- `total_minor` is the statement lines' sum, mirroring the journal's.
- `paid_minor` tracks what payments reconciled against it — the same
  receivable the journal's `settled_minor` carries, seen from the customer's
  side.

## Lifecycle and what drives each transition

`open → issued → paid | past_due | void`.

- **open → issued**: `journal_closing_period_in` writes the statement at
  `open` then `issued` in the same transaction — `open` exists only inside it,
  like the journal's `draft`. The statement lines are projected from the
  journal lines in customer vocabulary.
- **issued → paid**: `apply_order_to_journals_in` — T20's seam, extended —
  marks the statement when it marks the journal `paid`. A `paymentApplied`
  line appends reducing `total_minor − paid_minor` to zero. A partial payment
  leaves `issued` with `paid_minor > 0`.
- **issued → past_due**: the `statement-past-due` job flips `issued` rows
  whose `due_at` passed. The plan's own `past_due` (T19) and the statement's
  are the same fact — the statement is the customer-readable record of it.
- **issued/past_due → void**: staff `release-review` voids a statement with
  reversing lines; the statement is immutable from `issued` on (trigger).
- **past_due → issued**: staff `release-review` restores the issued standing
  (extends the grace window) — the row guard's one backward step.

## Overdue detection and the suspend policy

`statement-past-due` is a new `kinds::` job — leader-elected, minute cadence
(the receivable should mark past-due promptly once its terms lapse; the scan
is one indexed UPDATE bounded per pass). It does two things per past-due
statement:

1. Flip the statement `issued → past_due`. The plan's `past_due` state is
   already the plan-side fact; the statement carries the customer-facing due
   date and the audit of when it lapsed.
2. Past grace — `grace_days` on `customer_plans`, default 7 — the sweep
   stamps `customer_plans.dunning_suspended_at`, the same shape
   `organizations.dunning_suspended_at` (issue #258) already proves: the
   plan's D06 lifecycle (`past_due`) stays what it is; suspension is the
   admission gate the hold path reads, not a lifecycle state. A `past_due`
   statement inside grace still serves (quota + overage per
   `overage_enabled`); past grace the wallet-side draw refuses —
   subscription-quota draws (already paid for) keep serving, open operations
   settle normally. A payment clearing the statement clears
   `dunning_suspended_at` in the reconcile's own transaction, restoring
   admission automatically.

## Credit limit at admission

`customer_plans.credit_limit_minor`, NULL = unlimited. The cap applies to the
*wallet-side overage* — the `freeze − subscription.drawn_minor` remainder a
plan-bound turn holds against the wallet — never to subscription-drawn quota
(already paid for). Admission computes:

```
outstanding = Σ(remaining on issued|past_due statements) + current_holds
refuse when outstanding + this_overage > credit_limit_minor
```

`current_holds` is the live pending debit the pools already carry — what a
racing hold would compete for. The check rides the same org advisory lock
`admit_subscription_draw` already takes, so a racing payment's reconcile or a
second hold cannot observe a stale outstanding figure.

## The dispute chargeback

A lost dispute on a `planPeriod` order restores the receivable on the journal
(`recharge_journal_dispute_in`, T20 — `paid → posted`, `disputed_minor`
rising). The statement side must surface the same restored debt in customer
terms: the recharge appends a `disputeChargeback` line onto the *next* open
statement — the one the customer still owes against — or a fresh `open`
statement when none stands, so the restored receivable is never invisible.

## Manual review release

`POST /api/v1/business/statements/{id}/release-review` under `FundsWrite`:
`action=extendGrace` walks `past_due → issued` (extends the grace window),
`action=void` voids the statement with reversing lines. Every release writes a
`finance_audit` row naming the staff actor, the reason, and the statement —
the same audit family `journal.void` already uses.

## API

- `GET /api/v1/org/statements{,/{id}}` — the customer's own plan statements
  (`Personal`, like `/org/plan/journals`).
- `GET /api/v1/business/statements{?state,organizationId}` — the staff
  cross-organization list (`ReportsRead`); `state=pastDue` is the dunning
  work list.
- `POST /api/v1/business/statements/{id}/release-review` — `FundsWrite`,
  `{idempotencyKey, action, reason}`.
- `GET /api/v1/org/plan` gains `creditLimitMinor`, `outstandingMinor`,
  `nextDueAt` — the customer's own credit standing beside its plan.

## What T22/T23/T24 read off this

- `plan_statements` `issued|past_due` with `total_minor − paid_minor` is the
  open receivable an invoice-repayment order settles against — the
  customer-facing number T22's repayment surface shows.
- `credit_limit_minor` + the outstanding formula is the facility figure a
  dunning report or an admin customer view reads.
- `replaces_statement_id` is where a void-and-reissue statement correction
  links its successor — the T22 correction seam.
