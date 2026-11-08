# Recover an exact non-plan refund

Issue #841's recovery interface is being implemented. Do not run this workflow
until its migration and executable are integrated and validated. It applies to
wallet top-ups, bundles and legacy CreditLine repayments; plan-period refunds
and historical wrong-direction chargebacks use their separate recovery paths.

An actual provider cash return is separate from staff approval. The refund and
credit-note projections expose `cashEffect` when its complete Cash outflow has
been verified and retained. A late success can coexist with an unchanged
`declined`, `failed` or `released` refund or a `reversed` note. An unapproved cash
fact is review suspense and grants no retrospective authorization. A retained
effect prevents another payout and counts against the original refundable bound.
Null means this policy has not verified an effect, not that no cash was returned.

## Required evidence and authority

Stop and drain older refund, fulfillment and dispute writers before deploying
this change. Preserve the normal verified backup and the original evidence.
Never hand-edit ledger tables or disable immutable-history guards.

Select one exact refund and one trustworthy retained `refundSucceeded` inbox
receipt. The command checks the merchant/version, original provider payment,
customer, order, refund identifier, amount and currency. Missing coordinates can
only come from the immutable payment binding; contradictions are refused. An
old `succeeded` state without the exact receipt is insufficient. A payment-only
receipt is not assigned by guessing from today's pending refunds.

The command requires a currently valid personal owner or finance session in
Business context, including the account's second factor. A deployment token,
customer key or integration credential cannot authorize a correction. Put the
session token in a private file; do not pass it on the command line. Supply an
explicit `DATABASE_URL` for the reviewed installation. The command does not load
`.env`, migrate schemas, open missing wallets, scan all refunds or contact a PSP.

## Preview and apply

The interface under implementation is:

```bash
cargo run -p oxsum-core --example recover_nonplan_refund -- \
  --refund REFUND_UUID --receipt RECEIPT_UUID \
  --session-file /private/path/business-session \
  --reason "Reviewed retained provider refund evidence" \
  --key recovery-operation-key
```

The default preview returns a proposed or refused report and `sourceRevision`,
the SHA-256 of its complete source snapshot. It writes no ledger entry, document,
effect or audit. Inspect the customer/order/refund/receipt, original source and
cash hashes, allocation, historical boundary and review reasons. An operator
must not replace a refusal with a new guessed source.

Apply the same request with both `--apply` and
`--expected-revision SOURCE_REVISION`. The command checks live authority and
reconstructs the snapshot under the same source/refund/append locks. Any changed
source revision refuses without moving money. Preview again and review the new
facts before selecting a new request key. The same key binds the actor, exact
refund/receipt, reason and revision; changed reuse conflicts. An unchanged
committed retry returns the stored result without repeating cash or restitution.

Proposed or applied reports exit successfully. Source refusal, revision conflict,
invalid arguments, insufficient authority or storage failure exit unsuccessfully.
Apply retains accepted and refused source outcomes in `nonplan_refund.recover`
finance audit. A failed transaction commits none of its monetary/document effects.

## Monetary boundaries

For missing historical Cash, a confirmed exact source appends the complete
`Cr Cash`. Only provably retained original Wallet funding can be debited; spent,
held, repayment-origin or uncertain amounts remain `Equity:Adjustments` suspense.
Later Wallet credits cannot replenish the original source.

Source inspection verifies every sequenced entry before its selected boundary,
including the canonical bytes of intervening Wallet activity, using the caller's
database connection. It is bounded to 20,000 entries per customer ledger. A larger
history is `walletActivityReviewLimit`: a fresh confirmed refund still records
complete Cash with review suspense, and historical Wallet restitution refuses.
This limit is not proof that the original funding was spent or absent. Retain
the original evidence for explicit source investigation; splitting the refund
or changing its identity cannot establish a stronger source bound.

For a verified legacy `Dr Wallet / Cr Cash` entry, preserve its ID, key, hash,
booking date and note pointer. Compute the justified original Wallet take-back
immediately before that entry's sequenced position, including outstanding holds
at that historical point. Only the proved excess debit is restored with
`Dr Adjustments / Cr Wallet`. That correction never posts Cash again. A correct
legacy cash entry needs only the immutable allocation and audit, not another
monetary entry.

Unknown or reversed effects, mismatched hashes/keys/owners, incompatible source
links and ambiguous historical confirmations are explicit source-review refusals.
Original debt documents, CreditLine, receivables, quota and entitlements remain
governed by their original operations. This command does not authorize repayment
or forgive debt. Historical wrong-direction chargebacks remain #842.

The customer and finance `cashEffect` includes the complete Cash amount, proven
Wallet amount, suspense, any historical Wallet restitution, source/cash/correction
entry identities and hashes, receipt, review reasons and actual Cash booking day.
Its metadata commit time can be later than the original money-out. Reports count
that Cash once on its accounting day; restitution is not another provider refund.

A confirmed return before fulfillment fences the unperformed grant, including a
partial return. The paid order remains in reconciliation for explicit review and
is excluded from automatic fulfillment; the system does not invent a reduced
purchase or grant credits after their payment was returned.
