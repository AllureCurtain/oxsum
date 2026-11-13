# Operator runbook: Solana top-up exceptions and refunds

How an operator finds a Solana deposit that needs a refund, walks the
pull-payment lifecycle, and records the decision. Since issue #1142 the rail
names its unsettlable outcomes instead of leaving them `pending` — the
exception queue is the work list — and a refund is still a **deliberately
manual** business decision plus a key-signing act: the server holds no spend
key, the multisig pays out-of-band, and the endpoints only record what was
decided and what was paid. There is never an auto-refund to the payment
source.

## The exception queue

`GET /api/v1/admin/solana/exceptions?status=&limit=&cursor=` lists every
deposit the rail marked, newest first; the dashboard's
**Admin → Solana exceptions** section renders it with a status filter and
inline refund actions. The statuses:

- `underpaid` — a located transfer honored the intent but delivered under the
  asked amount. No credit ran; the shortfall sits in `payTo`.
- `overpaid` — the transfer over-delivered. The opened amount **did** credit
  (`entryId` is set — the money-in record stands); the surplus in `payTo` is
  the figure a refund returns.
- `late` — the paying transaction's on-chain `blockTime` is at/after the
  intent's `expiresAt`. No credit ran.
- `flagged` — held for review by pre-credit screening (issue #1144): the
  payer hit a sanctions source and the row parked before the ledger credited;
  `meta.screening` records `{verdict, source, reason, address, at}` and
  `facilitator_tx_hash` names the held transaction. Review is a compliance
  call — refund on-chain or clear it, never an auto-send.
- `refund_pending` — a refund was opened; the row is mid-lifecycle.
- `refunded` — the payout signature is recorded; the deposit is closed.

Each row carries the figures a decision needs without a chain lookup: `asset`
(the whitelist key the intent locked — `sol` or an SPL key like `usdc` — the
denomination every delivered/expected figure counts in and the refund
repays), `amountMinor`/`receivedMinor` (opened vs. delivered, in credits),
`delivered`/`expected` (the same figures in the locked asset's base units —
lamports for `sol`, the mint's own base units for an SPL asset),
`signature` (the paying transaction), `paymentRef` (the intent's marker), and
the `refund` record (`requestedBy`/`requestedAt`/`reason`, `addressedAt`,
`approvedBy`/`approvedAt`, `completedAt`) once the lifecycle is walking.

The customer's own view — `GET /api/v1/topups/solana/deposits`, rendered on
the Topup page — shows the same rows scoped to their organization, which is
where they learn their deposit needs attention and where the refund-address
form lives.

## The refund lifecycle

The refund is a pull payment in the BTCPay shape — four steps, each recorded
on `meta.refund`, each audited (`solana.refund.request` / `.approve` /
`.complete`):

1. **Open** — `POST /api/v1/admin/solana/{depositId}/refund {"reason"}` on an
   `underpaid`/`overpaid`/`late`/`flagged` deposit marks it `refund_pending`.
   Re-requesting an open refund replays the standing row; a non-refundable or
   `refunded` deposit is `409`.
2. **Address** — the customer supplies where the money goes:
   `POST /api/v1/topups/solana/{depositId}/refund-address {"address"}` on
   their own surface, validated as a base58 32-byte public key. The rail
   never derives the payer — the customer's paste is the destination.
3. **Approve** — `POST /api/v1/admin/solana/{depositId}/refund-approve`
   records the review verdict and **freezes the address** (a post-approval
   edit could redirect the payout the multisig is about to sign). Approving
   with no address yet is `409`.
4. **Pay + record** — the multisig sends the refund out-of-band **in the
   deposit's locked asset**: a plain `SystemProgram.transfer` of lamports for
   `sol`, or an SPL `TransferChecked` of `delivered − expected` (or the agreed
   refund figure) base units into the customer's ATA for `meta.splToken`
   otherwise — always from `payTo` to the recorded address, never reusing the
   deposit's `reference`. Then
   `POST /api/v1/admin/solana/{depositId}/refund-complete {"signature"}`
   records the payout's signature and closes the row `refunded`. A replayed
   complete with the same signature answers the standing row; a contradicting
   one is `409` — the audit-protecting refusal.

No ledger entry ever posts on this path: an `underpaid`/`late`/`flagged`
deposit never credited, so the refund simply returns what sits in `payTo`; an
`overpaid` one's `entry_id` still proves the opened credit while the surplus
returns on-chain.

## Deciding which shape the refund takes

- **Underpayment** — a business decision, never automatic: refund the
  shortfall on-chain through the lifecycle, or — when the customer meant well
  — grant the delivered figure as a manual receipt
  (`POST /api/v1/admin/organizations/{id}/manual-receipts`, `payment_ref`
  naming the original deposit) and leave the row `underpaid` with a note in
  the reason. Requires the business owner's sign-off, not a unilateral
  credit.
- **Overpayment** — routine: refund the surplus (`delivered − expected` base
  units of the locked asset) through the lifecycle; the customer's opened
  credit already stands.
- **Late** — refund through the lifecycle, or credit manually when the
  lateness was not the customer's fault.
- **Cross-organization** (a signature that credited one org and was refused
  in another — the `deposits_solana_tx_hash` double-spend refusal) —
  escalate: the second org's customer paid into a deposit the first org
  already consumed. That is always a real refund owed, and it wants an
  incident note on both deposits.

In every case the approving operator is named in `meta.refund` and the audit
log, and the payout signature on the closed row is what the books and the
chain reconcile against.

## Reading a row directly

When the queue's answer is not enough, the row itself is the record:

```sql
SELECT deposit_id, organization_id, status, payment_ref,
       amount_minor, received_minor, facilitator_tx_hash, meta
FROM oxsum.deposits
WHERE rail = 'solana' AND deposit_id = '<id>';
```

`meta` carries `signature`, `asset`, `splToken`, `rateMicro`, `assetDecimals`,
`expiresAt`, `delivered`, `expected`, `receivedCredits`, `blockTime`,
`exception`, `refundAddress`, `refundSignature` and the `refund` record —
every figure the queue row serializes, with `asset`/`splToken`/`assetDecimals`
naming which whitelist asset and decimals the intent locked.

## Monitoring and alerts (issue #1145)

The rail's health lands on `GET /metrics` beside the process metrics. Scrape
it; alert on these:

| Metric | Shape | Recommended alert |
| --- | --- | --- |
| `solana_rpc_errors_total{call,endpoint}` | Counter, per call (`find_reference`/`validate_transfer`/`mint_decimals`/`mint_owner`) and endpoint **host** — never the API-key-carrying URL | Rate over 5 min sustained > 0 on *every* configured endpoint — the rail is blind: deposits stop crediting. A single endpoint erroring is informational; `solana_rpc_failovers_total` shows traffic routing around it. |
| `solana_rpc_failovers_total{from,to}` | Counter per endpoint hop | Sudden sustained rate — one provider is being routed around constantly; check its status page and quota. |
| `solana_rpc_endpoint_healthy{endpoint}` | Gauge, 1 serving / 0 inside the ~30s failure cooldown | `== 0` on every configured endpoint for more than a minute — all providers are failing; the deposits poll is running dry. |
| `solana_deposits_paid_uncredited` | Gauge — `confirmed` deposits (chain-verified, ledger credit never landed) | `> 0` for more than a few minutes — a credit write is failing after verification; page an operator, then check the `solana-deposits` job's warn lines. |
| `solana_deposits_paid_uncredited_oldest_seconds` | Gauge — age of the oldest `confirmed` row | `> 900` (15 min) — the same alert's staleness figure; an uncredited deposit this old is a customer complaint waiting. |
| `solana_exception_queue_depth{status}` | Gauge per deposit status (`confirmed`, `reversed`, `expired`, and the #1142 statuses — `underpaid`/`overpaid`/`late`/`flagged`/`refund_pending`/`refunded` — at zero until that issue lands) | Backlog growth on an exception status > a tolerable N — pick N per deployment volume; `flagged` `> 0` always wants eyes. |
| `solana_screening_hits_total{result}` | Counter, `result="flagged"` — the series exists from boot; #1144's screening check increments it | `> 0` — a payer address hit a sanctions list; the deposit needs operator review, never an automatic refund. |
| `solana_rpc_endpoint_healthy` + `solana_rpc_errors_total` together | — | All endpoints unhealthy AND rising errors = the rail cannot verify transfers at all; intents still open but nothing confirms. |

Tuning notes:

- The cooldown is deliberately short (~30s after 2 consecutive failures): a
  flapping provider alternates `healthy`/`unhealthy` rather than stalling
  calls, and the failover counter — not the error counter alone — shows the
  pattern.
- `solana_deposits_paid_uncredited` should almost always read 0: `confirmed`
  is a momentary state inside one transaction, so a persistent nonzero means
  a crash between writes, not normal operation.
- Endpoint labels carry host only — a Helius API key in the URL can never
  reach a metric, a log line or a `Debug` print.

## What this runbook does not cover

Price-oracle lock timing, the wallet adapter that removes the manual
reference step, and hot-wallet key custody / consolidation / withdrawal —
the follow-ups tracked on #1117, not this rail's current scope. Key custody —
receiving `payTo` into a Squads v4 multisig vault and who signs a spend — is
the operator-side setup in `docs/self-hosted-solana-multisig.md` (issue
#1143). Dual SOL+USDC deposits shipped with #1140 — USDT joined as an opt-in
whitelist key behind `OXSUM_SOLANA_USDT_ENABLED` (#1151): an intent's
`meta.asset` names the asset its refund must move, and a stablecoin's
`rateMicro` is the fixed 1_000_000 parity.
