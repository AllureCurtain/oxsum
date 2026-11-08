# Review retained dispute-resolution effects

Issue #826 makes a new dispute resolution one transaction. The current business
and personal-session authority, exact customer wallet, ledger/document effects,
selected state and immutable finance audit commit or roll back together. It does
not automatically compensate older interrupted transactions.

## Deploy without old writers

Stop admission and drain the old server and all workers before replacing them.
Do not run old and new dispute writers concurrently: an old binary can still
commit its separate middle money phase without the new decision lock. Take and
verify the normal backup, then follow the self-hosted upgrade runbook. No new
migration is required for this transaction fix.

## Retry an unchanged request

Reuse the original idempotency key, target, resolution and optional reason. The
reason compares after surrounding whitespace is removed. Live authority and the
customer wallet are checked on every call, including replay. A changed actor,
target, resolution or reason conflicts under a reused key. An unchanged prior
request returns the current dispute standing; a later valid transition does not
cause an old `evidenceDue` retry to move the dispute backward.

A different terminal outcome returns `409`. An unused key requesting the current
terminal outcome returns its standing row without writing another audit or cash
entry. It does not reserve that unused key for a future operation.

## Investigate an explicit source-review conflict

Retain the business, organization, merchant/version, order, dispute and original
event IDs, requested outcome and request key. Use read-only access to inspect:

1. The immutable order identity/amount and authenticated provider event.
2. The dispute's current state, `recharge_entry_id`, `resolved_by` and `audit_ref`.
3. The sequenced entry under `dispute-recharge/<dispute-id>` in that organization's
   ledger, including its exact derived ID, key, currency, layer and postings.
4. The `dispute.resolve` audit's target, actor, outcome, reason and allocations.
5. For plan orders, original journal/statement sources, refunds, void payable,
   and any historical usage-correction source map.
6. For non-plan orders, immutable fulfillment, original `payment-order:<id>`
   entry/hash, later Wallet debits/reservations, prior actual Cash returns and
   any reversal of either the funding entry or the chargeback itself.

An entry alongside `opened`/`evidence_due`, or alongside `won`/`closed`, is a
possible retained interrupted money phase. A lost dispute with missing or
different entry/audit evidence also requires review. The endpoint refuses these
combinations without posting cash, changing terminal state or replacing audit
history. A partial or unsupported entry shape is not sufficient evidence for an
automatic retry.

Compare these retained facts with the provider's actual outcome. Preserve the
original bytes and prepare an issue-specific append-only correction with explicit
source amounts and independent review. There is no generic apply command for conflicting dispute history. The
[exact historical non-plan workflow](nonplan-chargeback-recovery.md) supports
only its independently proved old terminal loss shape after migration 0119. Do not hand-edit ledger tables,
disable guards, create a replacement payment/dispute, or choose another outcome
merely to make the endpoint succeed.

## Remaining financial boundary

Plan losses retain complete cash credits against original receivables, refund
payable or explicit suspense. New non-plan losses (#840) credit the complete
Cash loss. They debit only proven retained original Wallet funding and place the
rest in `Equity:Adjustments` suspense. Source proof includes the fulfilled order,
exact original entry/id/key/hash and positive settled USD legs, plus all later
settled Wallet debits, current pending reservations and prior actual case cash.
Later top-ups never replenish the source. Any prior refund history excludes a
Wallet take-back until that separate writer's recovery is implemented.

The immutable `dispute.resolve` audit names `walletTakenMinor`,
`unallocatedMinor`, `requiresReview`, `source` and `cancelledPromoLotId`. The source
policy is `nonplan-cash-v1`; `source.reviewReasons` explains unproven fulfillment,
entry, hash, reversal, allocation, prior refunds/chargebacks or funding no longer
retained. Credit-repayment funds remain suspense: reopening an old statement is
not authorized by a generic loss. Non-plan case intake requires an explicit ID,
amount and matching fiat currency. Partial fiat loss must convert exactly to
integer credit minor units. Missing or contradictory old accepted events remain
review-required, with their original bytes preserved.

Historical Dr Cash / Cr CreditLine chargebacks are not accepted as correct cash
evidence. #842 provides exact preview/apply for proven terminal losses: original
false receipt B plus authenticated loss L requires appended Cash credit B + L.
The old credit benefit and documents remain unchanged for review. Unsupported
history still refuses. #841's complete refund cash/recovery is integrated; #829
remains open until historical boundaries and acceptance are accounted for before
#777 exposes statement repayment. Stop and drain old
writers when deploying; mixed-version writers do not honor the new source lock.

Finance `fundingMinor` remains a gross receipt metric and `refundMinor` follows
its documented refund sources; neither is a net-Cash-after-chargebacks balance.
Review the chargeback's retained Cash entry and source audit directly. Review
suspense is not recognized revenue or a reopened receivable.

A paid order whose non-plan loss preceded fulfillment remains visible in payment
reconciliation for review. It is excluded from the automatic fulfillment queue,
so its deliberate grant refusal does not block unrelated paid orders.
