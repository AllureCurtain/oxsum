# Operator runbook: Solana top-up refunds

How an operator finds a Solana deposit that needs a manual refund, decides
whether to refund it, and records the decision. This runbook is deliberately
**manual** — the rail does not auto-refund, because a refund is a business
decision (was the underpayment a customer mistake or a bad quote?) plus a
key-signing act the service should not automate.

## The four shapes a refund problem takes

Every anomaly surfaces on one `oxsum.deposits` row (`rail = 'solana'`) or in
the `solana-deposits` job's log line. Read the row first:

```sql
SELECT deposit_id, organization_id, status, payment_ref,
       amount_minor, received_minor, facilitator_tx_hash, meta
FROM oxsum.deposits
WHERE rail = 'solana' AND deposit_id = '<id>';
```

- `amount_minor` is the credit the intent opened for; `received_minor` is what
  the chain actually delivered converted back at the locked rate.
- `facilitator_tx_hash` is the paying transaction's signature; `meta` carries
  `signature`, `solLamports`, `rateMicro`, `assetDecimals`, `expiresAt`,
  `delivered` and `receivedCredits`.

### 1. Expired-but-paid — the customer paid after `expiresAt`

The payment's on-chain `blockTime` is at/after `meta.expiresAt`, so the rail
refused to credit (a payment before the TTL credits even when confirmed
late — this is only the genuinely-late case). The deposit stays `pending`
until the poll's expiry sweep marks it `expired`; the money sits in `payTo`.

**Find it:**

```sql
SELECT deposit_id, payment_ref, meta->>'expiresAt' AS expiry,
       meta->>'signature' AS signature
FROM oxsum.deposits
WHERE rail = 'solana' AND status = 'expired'
  AND meta ? 'signature';
```

An `expired` row that still carries a `signature` in meta — or one whose
`getTransaction` blockTime postdates `expiresAt` — is money in `payTo` the
customer was never credited for. Decide whether to refund on-chain or to
credit the account manually (a manual receipt, see below) when the lateness
was not the customer's fault.

### 2. Underpayment — the transfer was short

`delivered`/`received_minor` < the asked `solLamports`/`amount_minor`. The
rail refused the intent; the partial payment is in `payTo`.

**Find it:**

```sql
SELECT deposit_id, meta->>'solLamports' AS asked,
       meta->>'delivered' AS delivered, meta->>'signature' AS signature
FROM oxsum.deposits
WHERE rail = 'solana'
  AND (meta->>'delivered')::bigint < (meta->>'solLamports')::bigint;
```

Either refund the shortfall on-chain, or — when the customer meant well —
open a manual receipt for the delivered figure and treat the gap as a
write-off the business accepts.

### 3. Overpayment — the transfer over-delivered

`delivered`/`received_minor` > `amount_minor`. The rail credited the *opened*
amount, not the surplus — the surplus stays in `payTo` and is the figure to
refund or to grant as extra credit.

**Find it:**

```sql
SELECT deposit_id, amount_minor, received_minor, meta->>'signature' AS signature
FROM oxsum.deposits
WHERE rail = 'solana' AND status = 'credited'
  AND received_minor > amount_minor;
```

The surplus in `payTo` is `(delivered − solLamports)` asset units. Refund it
on-chain, or grant it as a manual top-up if the business chooses to honor the
overpayment.

### 4. Refused — the transaction did not honor the intent

Wrong recipient, wrong mint, missing marker, a failed transaction, or a
signature that already credited another deposit (the cross-organization
double-spend the global `deposits_solana_tx_hash` index refuses with `403`).
These land as `solana deposit credit failed` warnings in the `solana-deposits`
job log and stay `pending` until expiry.

**Find it:** the job's warn lines carry `deposit_id` + the refusal reason;
the row's `meta.settleRefusal`/`signature` names what the chain saw.

## Recording the refund

A refund is two separate, deliberately unlinked acts — keep them honest:

1. **The on-chain return.** The operator (or a delegated signer) sends the
   refund from `payTo` back to the customer's wallet — a plain
   `SystemProgram.transfer` / SPL `TransferChecked` for the figure being
   returned. When `payTo` is a Squads v4 multisig vault
   (`docs/self-hosted-solana-multisig.md`) this is a multisig operation: a
   signer proposes the spend, the threshold approves it — the server never
   signs. Record that transaction's signature: it is the refund's proof.
   Never reuse the deposit's `reference` — the refund is a new transfer.

2. **The ledger note.** On the deployment, post a **manual receipt** (`POST
   /api/v1/admin/organizations/{organizationId}/manual-receipts`) *only* when
   the decision is to credit the customer's account rather than return the
   funds — for example an underpayment the business accepts, or an
   expired-but-paid deposit the customer should keep. Give the manual receipt
   a `payment_ref` that names the original deposit (`solana-refund:<signature>`
   or the deposit id) and a `reason` that records the refund's on-chain
   signature, so the ledger line and the chain transaction point at each
   other. When the decision is to return funds on-chain with no credit, no
   ledger write is needed — the deposit simply stays `expired`/`refused`.

## Who approves

- **Expired-paid and refused** — a support/operator decision: refund the
  customer to be safe, or hold for the customer to re-pay with a fresh
  intent. Document the call on the deposit row's audit note.
- **Underpayment** — a business decision: the merchant accepts the short
  figure (manual receipt) or refunds it. Requires the business owner's sign
  -off, never a unilateral credit.
- **Overpayment** — routine: refund the surplus on-chain; no credit decision
  needed unless the business chooses to grant the overage.
- **Cross-organization** (a signature that credited one org and was refused
  in another) — escalate: the second org's customer paid into a deposit the
  first org already consumed. That is always a real refund owed to the second
  customer, and it wants an incident note on both deposits.

In every case the approving actor is named in the deposit's audit trail and
the refund's on-chain signature is recorded on the deposit row (`meta`), so
the books and the chain reconcile.

## What this runbook does not cover

Price-oracle selection and lock timing, dual SOL+USDC support, and the wallet
adapter that removes the manual reference step — the follow-ups tracked on
#1117, not this rail's current scope. Key custody — receiving `payTo` into a
Squads v4 multisig vault and who signs a spend — is the operator-side setup
in `docs/self-hosted-solana-multisig.md` (issue #1143).
