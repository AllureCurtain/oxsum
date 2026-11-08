# Plan refund cash recovery

Use this operation for a confirmed historical `plan_period` refund whose cash
or customer documents did not receive the correct allocation. It never creates
or retries an external refund. A new authenticated callback uses the same
allocation engine automatically. Ordinary cash returns do not reduce unrelated
outstanding invoice debt.

Run from an identified checkout against the intended database after migration
0110. Preserve a backup and the original refund/order/note/entry identities.
Provide a current personal business-context session with `FundsWrite` in a
private file; the command never prints it and does not discover `.env` files.

```bash
DATABASE_URL='postgres://…' cargo run -p oxsum-core --example recover_plan_refund -- \
  --refund <exact-refund-uuid> --session-file /private/path/session
```

The default is a dry run: no cash, allocation or document write is committed.
`cashMinor` is the complete confirmed external amount. Its `deferredMinor`,
`earnedMinor`, `payableMinor` and `unallocatedMinor` must sum to it. The preview
identifies the original order/period/journal and any original legacy entry.

After reviewing that exact proposal, apply the same refund:

```bash
DATABASE_URL='postgres://…' cargo run -p oxsum-core --example recover_plan_refund -- \
  --refund <exact-refund-uuid> --session-file /private/path/session --apply
```

The committed result includes `entryId`; a replay has `alreadyApplied: true`
and does not append another cash entry. `plan_refund.recover` records the staff
actor and allocation. The original note's applied entry never changes. For an
old `Dr Discounts / Cr Receivables` refund, the correction restores those
receivables and records the cash that actually left. A previously correct cash
entry is retained; only missing customer projections and source allocation are
added. A pre-close refund immediately reduces deferred money and is consumed
once at close.

Inspect the original journal and statement after apply. Example in minor
units: a legacy bill of 4,000 with 2,500 paid has 1,500 outstanding. A confirmed
2,000 cash refund leaves a 2,000 bill, 500 paid and the same 1,500 outstanding;
cash falls by 2,000. Refunding cash is not forgiveness of that debt.

`unallocatedMinor > 0` means an earlier chargeback or other exhausted original
source prevents full allocation. The complete cash outflow is still retained;
the difference is `Equity:Adjustments` reconciliation suspense, with a
`bookedWithReview` audit. It must receive an explicit finance decision and must
not be presented as earned revenue or a customer receivable. A failed/released
row with a late success retains its terminal state and reversed note; its cash
effect nevertheless consumes refundable balance and prevents activation.

Recovery refuses unknown/unconfirmed refunds, a wrong tenant wallet, a missing
original payment, unrecognized legacy entry shapes, ambiguous multiple legacy
refunds, or a legacy refund followed by a void requiring combined source
analysis. Retain the error and evidence for explicit correction; do not modify
posted entries, suppress triggers, insert fabricated payment rows, or issue a
second PSP refund to work around it. Daily recognition and policy migration are
separate work under N11.


A later chargeback also records its full cash amount. Only the still-collected
part of the original bill restores customer debt; any part already refunded
becomes explicit review suspense. `dispute.resolve` retains that exact split.
The same rule applies if the callback and dispute resolution race, or if a
voided bill already owes a refund payable.
