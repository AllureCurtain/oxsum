# Recover an exact historical non-plan chargeback

This workflow is under implementation in #842. Do not use it before migration
0119 and the command are integrated and validated. It covers one proven old
`Dr Cash / Cr CreditLine` loss, preserving the original entries and terminal
resolution. It does not call a PSP or change existing debt, monthly statements,
quota or entitlement.

The old writer clamped its amount B to drawn CreditLine even when the confirmed
loss L was larger. For the proven case `0 < B <= L`, the appended Cash credit is
B + L: B cancels the false receipt, and L records the actual loss. Only verified
retained original purchased Wallet may fund L. All other debit, including the
preserved CreditLine benefit B, stays source-linked Adjustments review suspense.
The correction is not a second provider cash loss. Later top-ups cannot replace
spent original source.

## Exact evidence

Stop and drain older financial writers before deployment and retain a verified
backup. Select the exact dispute and retained authenticated dispute receipt.
The command verifies the customer/order/merchant/version, case/payment/amount/
currency, immutable original fulfillment and funding, terminal `lost` state and
original actor/audit, derived original entry/key/hash and positive settled USD
legs. The original audit must bind the wrong amount B and actual contested loss
L. Current balances or a terminal state alone are insufficient evidence.

Missing, altered, foreign or reversed sources, contradictory audit, unsupported
prior returns or source boundaries, and orphan entries beside `opened`,
`evidence_due`, `won` or `closed` refuse. This workflow does not guess a historical
loss, change an outcome, or turn an unproven source bound into customer debt.

## Preview and apply

The planned exact command is:

```bash
cargo run -p oxsum-core --example recover_nonplan_chargeback -- \
  --dispute DISPUTE_UUID --receipt RECEIPT_UUID \
  --session-file /private/path/business-session \
  --reason "Reviewed retained historical chargeback evidence" \
  --key recovery-operation-key
```

Supply explicit `DATABASE_URL` and a private file containing a currently valid
personal owner/finance session in Business context, including second factor.
No dotenv discovery, migration, wallet creation, batch scan or provider I/O is
performed. Preview rolls back without ledger, document, effect or audit writes.
Inspect both Cash components, original and proposed postings, source hashes,
prior usage, review reasons and returned SHA-256 `sourceRevision`.

Apply the same exact request with `--apply --expected-revision SOURCE_REVISION`.
The command rechecks live authority and the complete source under business,
organization, source, dispute and append locks. Changed source refuses before
money. Exact replay binds actor, target, receipt, reason, key and revision. The
correction, immutable effect and `nonplan_chargeback.recover` audit commit
atomically. Refused apply attempts retain explicit outcomes. The old entry,
terminal outcome, original audit and debt history remain intact.

Reports continue to distinguish gross receipts and refunds from chargebacks.
The dispute's `cashCorrection` names the original wrong entry, correction, both
Cash components, retained credit benefit, original Wallet allocation and review
suspense. Null means no retained correction, not that the old entry was correct.
The read projection does not independently reverify ledger or provider evidence.
An exact earlier resolution request still replays the current corrected `lost`
standing; it does not transition the dispute back to the earlier outcome.
