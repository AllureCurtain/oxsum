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

An entry alongside `opened`/`evidence_due`, or alongside `won`/`closed`, is a
possible retained interrupted money phase. A lost dispute with missing or
different entry/audit evidence also requires review. The endpoint refuses these
combinations without posting cash, changing terminal state or replacing audit
history. A partial or unsupported entry shape is not sufficient evidence for an
automatic retry.

Compare these retained facts with the provider's actual outcome. Preserve the
original bytes and prepare an issue-specific append-only correction with explicit
source amounts and independent review. This release provides no generic apply
command for a conflicting dispute history. Do not hand-edit ledger tables,
disable guards, create a replacement payment/dispute, or choose another outcome
merely to make the endpoint succeed.

## Remaining financial boundary

Plan losses retain complete cash credits against original receivables, refund
payable or explicit suspense. The legacy non-plan allocation still debits Cash
and credits CreditLine, capped at current drawn debt. That is the known #829
cash-direction/source-allocation defect, not a certified chargeback policy.
Partial event amounts and adjacent non-plan refund recovery also remain in that
financial follow-up. It must finish before #777 exposes statement repayment.
