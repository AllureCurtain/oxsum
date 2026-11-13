# Runbook: Solana mainnet small-amount rehearsal

A real-money rehearsal of the `solana` top-up rail on Solana **mainnet-beta**,
capped at **≤ US$5 per transfer**. The point is to prove the deployed rail
against live chain conditions — authenticated RPC, real block times, a real
multisig vault — before opening it to customers. Every step states the
action, the expected result, and how to verify it. Nothing here credits a
wallet the operator did not intend to credit; every scenario that is supposed
to refuse is rehearsed to refusal, not assumed.

The rail is documented in `docs/operator-solana-refunds.md` (the refund half),
`docs/decisions.md` 2026-11-13 (why mainnet looks the way it does) and
`crates/server/openapi.yaml` (`POST /api/v1/topups/solana`,
`POST /api/v1/topups/solana/confirm`). This runbook assumes the reader has an
installed deployment (`docs/self-hosted-install.md`) and an operator token
(`OXSUM_ADMIN_TOKEN`).

> **Read this before spending anything.** Mainnet money is not recoverable
> by the software. The cap, the asset bounds and the intent rate limit below
> are the rehearsal's safety rails — set them, verify they booted, then spend.

## Status of companion features

The mainnet target state is the batch tracked under #1117. Features that have
not merged yet are marked **(pending #NNNN)** where a step's expected outcome
depends on them; the step still runs — the expected result names both the
merged and the target behavior. As of this writing the merged rail is the
devnet-hardened one: single asset (SOL or one configured SPL mint),
`OXSUM_SOLANA_RATE_MICRO` static pricing, `finalized` commitment, the 15 s
`solana-deposits` poll and the manual refund runbook. Pending companion work:

- #1139 — oracle-priced SOL intents via Pyth (locked at open)
- #1140 — multi-asset deposits (SOL + USDC on one deployment)
- #1141 — Solana Pay transfer-request URL + QR on the top-up page
- #1142 — exception queue + pull-payment refund flow (`underpaid` / `overpaid` /
  `late` / `flagged` / `refund_pending` / `refunded` statuses)
- #1143 — `payTo` as a Squads v4 multisig vault
- #1144 — pre-credit sanctions screening on the payer address
- #1145 — authenticated RPC endpoints with failover + rail metrics
- #1149 — admin reconciliation scan + daily CSV

## Step 0 — offline prerequisites (do these first, by hand)

These are human prerequisites, not code — nothing in the repo provisions them.
Complete every line before configuring the deployment.

- [ ] A Squads v4 multisig vault (or equivalent) on mainnet-beta to act as
  `payTo`: 2-of-3 signers — operations lead, finance, one offline backup key.
  Record the vault address; the server holds **no** spend key (pending #1143:
  the boot validates `payTo` as far as it can; until then the address is
  operator-verified only). If a multisig is not yet in place for the rehearsal,
  a dedicated single-signer address is acceptable **for the rehearsal only** —
  treat its key like the deployment's cash drawer, and migrate to the multisig
  before any customer traffic.
- [ ] An authenticated Solana RPC endpoint (Helius or Triton key) — never the
  public `api.mainnet-beta.solana.com` for a production gate (pending #1145:
  multi-endpoint failover; until then one authenticated endpoint is the
  minimum).
- [ ] A funded test wallet holding at most ~US$20 total — enough for ~4 small
  transfers plus fees. This wallet plays the customer. Phantom, Solflare or a
  CLI keypair are all fine; the rail does not care what sends the transfer,
  only that it carries the `reference` marker.
- [ ] Chainalysis (or equivalent screening provider) API key if rehearsing the
  screening path (pending #1144) — plus a local copy of the OFAC SDN
  Solana-address list. Skip for a rehearsal on the merged rail; the step marks
  the expected result either way.
- [ ] The operator token for the deployment and a `psql` line to the database
  (the Compose file's `postgres` service or your managed instance).
- [ ] A registered customer organization on the deployment — the rehearsal
  uses real auth; `/api/v1/topups/solana` is a session endpoint.

## Step 1 — configure and boot for mainnet

Set the rail's environment in `deploy/.env` (or the env source your install
uses). The rail is all-or-none: `OXSUM_SOLANA_RPC_URL`,
`OXSUM_SOLANA_PAY_TO`, `OXSUM_SOLANA_RATE_MICRO` together, or the endpoints
answer 503.

```dotenv
# The authenticated endpoint, never the public cluster endpoint.
OXSUM_SOLANA_RPC_URL=https://mainnet.helius-rpc.com/?api-key=<key>

# The multisig vault every intent pays.
OXSUM_SOLANA_PAY_TO=<vault base58 address>

# Mainnet-grade finality. Finalized is the default; state it explicitly so a
# later reader cannot mistake the posture.
OXSUM_SOLANA_COMMITMENT=finalized

# USDC as the rehearsal asset (pending #1140: one deployment carries both SOL
# and USDC; until then the rail prices exactly one configured mint, and USDC
# is the right choice for a dollar-denominated rehearsal — its rate is the
# fixed unit conversion, never an oracle).
OXSUM_SOLANA_SPL_TOKEN_MINT=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
OXSUM_SOLANA_DECIMALS=6
# 1 USDC = 1 USD of credit at 6 decimals: 1_000_000 minor-credits per whole
# unit. With #1139 merged this figure survives only as a devnet/test stand-in
# — a mainnet deployment that prices SOL must lock a live oracle per intent.
OXSUM_SOLANA_RATE_MICRO=1000000

# The safety bounds — the whole point of the rehearsal cap.
# 5_000_000 minor-credits = US$5.00 at the minor-units convention (1 credit =
# 1_000_000 minor, priced $1/credit in this cut). One intent can never demand
# more than $5 on-chain.
OXSUM_SOLANA_MIN_MINOR=1000
OXSUM_SOLANA_MAX_MINOR=5000000
OXSUM_SOLANA_OPENS_PER_MINUTE=3

# A mainnet rehearsal wants a longer payable window than devnet's 600 s so the
# expired-then-paid scenario can be staged deliberately. 900 s recommended.
OXSUM_SOLANA_INTENT_TTL_SECS=900
```

Boot and verify:

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.core.yml up -d oxsum
curl -s http://127.0.0.1:3000/readyz        # ready — the boot verified
```

Expected:

- `/readyz` answers `ready` — the boot read the USDC mint's on-chain decimals
  and matched `OXSUM_SOLANA_DECIMALS=6`. A mismatch is a startup refusal, not
  a running misprice.
- `GET /api/v1/topups/solana` unauthenticated is 401; a session call before
  the rail's set is complete is 503 (not relevant here — the set is complete).
- The server log shows no `solana find_reference RPC failed` warnings at boot
  — those mean the endpoint is unreachable (pending #1145: those errors count
  on `solana_rpc_errors_total`; today they are warn lines).

Verify the configured deposit address on-chain once, by hand:

```bash
# On your RPC endpoint — the vault should exist and, for USDC, its ATA for the
# mint should exist (prime it with a tiny amount if it does not, so the first
# deposit does not hit rent edge cases).
getAccountInfo <OXSUM_SOLANA_PAY_TO>
getTokenAccountsByOwner <OXSUM_SOLANA_PAY_TO> for mint EPjFWdd5...TDt1v
```

## Step 2 — happy path: open, pay, credit

**Action.** As the customer session, open an intent:

```bash
curl -s -X POST http://127.0.0.1:3000/api/v1/topups/solana \
  -H "cookie: <customer session>" -H "content-type: application/json" \
  -d '{"amountMinor": 2000000}'    # US$2.00
```

Expected: `200` with

```json
{"data":{"reference":"<base58>","payTo":"<vault>","amountLamports":2000000,
          "splToken":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
          "expiresAt":"<RFC3339>"}}
```

`amountLamports` is in the asset's base units — 2 000 000 base units of USDC =
US$2.00. The `reference` is the per-intent marker the paying transaction must
carry as a **read-only non-signer** account key.

**Pay it** from the test wallet (≤ $5 cap holds — this is $2):

- Pending #1141: scan the Solana Pay QR on the top-up page and approve.
- Until then: build a `TransferChecked` SPL transfer for `amountLamports` of
  the USDC mint to `payTo`'s ATA, with `reference` added as a read-only
  non-signer account key — `examples/solana-topup/pay.mjs` does exactly this
  against a live deployment, or your wallet's "custom accounts" flow.

**Confirm** (or just wait ≤ 15 s for the poll):

```bash
curl -s -X POST http://127.0.0.1:3000/api/v1/topups/solana/confirm \
  -H "cookie: <customer session>" -H "content-type: application/json" \
  -d '{"reference":"<the reference>"}'
```

Expected: `200` with a `Receipt` — the ledger credit. `404` means the poll
already credited it (the documented race — the money arrived; re-read the
deposit row). `403` means no honoring transfer has landed yet — wait for the
transaction to reach `finalized`, then retry.

**Verify:**

```sql
SELECT deposit_id, status, amount_minor, received_minor,
       facilitator_tx_hash, entry_id, meta
FROM oxsum.deposits
WHERE rail='solana' AND payment_ref='<reference>';
```

- `status` = `credited`; `entry_id` names the ledger entry.
- `facilitator_tx_hash` = the paying transaction's signature — the
  `solana:<signature>` credit key makes any replay a no-op.
- `received_minor` = `amount_minor` (exact pay); `meta.signature` matches
  `facilitator_tx_hash`; `meta.delivered` = 2 000 000; `meta.receivedCredits`
  = 2 000 000.
- The wallet's available balance rose by exactly the credit amount — check the
  customer-facing balance endpoint or the ledger line the receipt named.

Replay the confirm with the same `reference`: `404` — one intent credits once.

## Step 3 — underpayment

**Action.** Open a $2 intent, then pay **less than** `amountLamports` — for
example 1 500 000 base units (US$1.50).

Expected: the transfer finalizes on-chain but the deposit does **not** credit.
`validate_transfer` requires the delivered figure to meet the asked amount;
an underpayment is refused, indistinguishable from "not paid yet" at the
verify seam.

- Confirm answers `403` while the row is `pending`; after `expiresAt` lapses
  the poll's sweep marks it `expired`.
- Pending #1142: the deposit moves to `underpaid` on the exception queue with
  `meta.delivered`/`meta.receivedCredits` recording the shortfall, and the
  admin list can filter on the status. Until then the expected state is:
  stays `pending` until expiry, then `expired`; the shortfall is money sitting
  in `payTo` the refund runbook (step 6) rehearses.

**Verify:**

```sql
SELECT status, amount_minor, received_minor, meta->>'delivered' AS delivered,
       meta->>'signature' AS signature
FROM oxsum.deposits WHERE rail='solana' AND payment_ref='<reference>';
```

`received_minor` is null or below `amount_minor`; `meta.signature` (once the
rail has seen the transaction) names the paying signature for the refund step.
The vault's on-chain balance did rise — the money is real; the credit is not.
That asymmetry is the rehearsal's point.

## Step 4 — overpayment

**Action.** Open a $2 intent, pay **more** than `amountLamports` — 2 500 000
base units (US$2.50).

Expected: the deposit **credits at the opened amount** — never more. The
ledger books `amount_minor` = 2 000 000; `received_minor`/`meta.receivedCredits`
record the surplus (2 500 000) for a human. The rail never auto-patches.

**Verify:**

```sql
SELECT status, amount_minor, received_minor,
       meta->>'delivered' AS delivered, meta->>'receivedCredits' AS rc
FROM oxsum.deposits WHERE rail='solana' AND payment_ref='<reference>';
```

`status` = `credited`, `amount_minor` = 2 000 000, `received_minor` =
2 500 000. The wallet gained US$2.00, the vault gained US$2.50. The US$0.50
surplus is an overpaid exception to refund or grant (step 6); pending #1142
the row also carries an `overpaid` status and queue placement — today the
`received_minor > amount_minor` predicate on `GET /api/v1/admin/deposits`
(or the SQL above) is the review surface.

## Step 5 — expired then paid

**Action.** Open an intent and **do not pay it** until `meta.expiresAt` has
passed (the 900 s TTL gives room; the poll's sweep marks the row `expired`
once the deadline lapses). Then pay it anyway, carrying the reference.

Expected: the payment lands on-chain but never credits. Expiry is judged on
the paying transaction's `blockTime` against `meta.expiresAt` — a `blockTime`
at/after the deadline is refused even though the row sat unclaimed.

- Confirm answers `403` ("the paying transaction landed after the intent
  expired") while the row is still `pending`, or `404` once the sweep has
  expired it.
- Pending #1142: the row lands as `late` on the exception queue. Until then
  it is `expired` with `meta.signature` set — the find query in step 6's
  refund path is the same one the refund runbook names.

**Verify:**

```sql
SELECT status, meta->>'expiresAt' AS expiry, meta->>'signature' AS signature
FROM oxsum.deposits WHERE rail='solana' AND payment_ref='<reference>';
```

`status` = `expired`; cross-check the signature's `blockTime` on the chain:
`getTransaction <signature>` — it postdates `expiresAt`. Money in `payTo`,
never credited — a genuine refund candidate, rehearsed next.

## Step 6 — refund the exception

The rail **never auto-refunds to the payment source** — a refund is a business
decision plus a key-signing act. The pull-payment model (pending #1142)
formalizes it: operator initiates → customer supplies a refund address →
operator approves → the multisig pays → the signature is recorded back → the
deposit closes `refunded`. Until #1142 merges, the acts are the same two the
refund runbook (`docs/operator-solana-refunds.md`) names, done by hand.

**Action.** Pick the underpaid (step 3) or late-paid (step 5) deposit.

1. Decide: refund the asset on-chain (same mint, same delivered base units,
   to an address **the customer supplies**) or credit the account manually.
   The refund repays the asset the customer paid, never the credit figure.
2. The customer names the refund address — out of band, or through the
   `refund_pending` flow once #1142 lands (the deposit's meta carries the
   supplied address for review).
3. Send the refund from the vault through the multisig — a plain
   `TransferChecked` for the delivered figure, **never** reusing the deposit's
   `reference`. Record the refund signature.
4. Record it back: pending #1142, the approve call writes the signature and
   flips `refund_pending` → `refunded` with the audit fields. Until then,
   annotate the deposit row's `meta` (or the incident note) with the refund
   signature so the books and the chain reconcile — and, when the decision
   was to credit instead of return, post the manual receipt
   (`POST /api/v1/admin/organizations/{id}/manual-receipts`) with
   `payment_ref` naming the original deposit and the reason naming the refund
   signature.

**Verify:**

```sql
SELECT status, meta->>'signature' AS paid_sig, meta->>'refundSignature' AS refund_sig
FROM oxsum.deposits WHERE rail='solana' AND deposit_id='<id>';
```

Pending #1142: `status` = `refunded`, `refund_sig` set, the audit trail names
the approving operator. Until then: the refund signature exists on-chain from
the vault to the customer-named address, and the deposit row (or ops note)
points at it. `getTransaction <refund_sig>` shows the vault → customer
transfer of the delivered figure.

## Step 7 — screening a flagged payer (pending #1144)

On the target rail the payer's debited signer is screened before the ledger
credit books: Chainalysis plus a local OFAC SDN Solana list, both cached. A
hit does not credit and does not refund — the deposit moves to `flagged` and
into operator review, a deny-until-cleared posture (an external-API failure
keeps the deposit `pending` and retries, never silently opens the gate).

**Rehearsal.** Until #1144 merges there is nothing to trip: every paying
transfer validates to credit. Rehearse the *shape* anyway:

- Fund a second scratch wallet you control, pay a small intent from it, and
  trace which address the rail would screen — the transaction's debited
  signer. That is the address a real flag would name.
- Verify the operator review path the same way: a flagged deposit is an
  exception-queue row (pending #1142) whose review clears or holds. Today the
  stand-in is simply: any `pending` row with `meta.signature` set that is not
  crediting is a row a human must look at — the screening feature automates
  the flag, not the judgment.

**Verify** (post-#1144): `status` = `flagged`, `meta.screening` names the
list/provider and the payer address; the admin list filters on it; a clear
decision moves it through review to `credited` or leaves it closed.

## Step 8 — reconcile against the vault

Reconciliation is line-by-line: every `solana` deposit row should map to one
on-chain transfer into `payTo`, and every transfer into `payTo` should map to
a deposit row, an exception or a deliberate test artifact.

**Action.** Pending #1149 (the daily CSV + unrecorded-on-chain scan job), do
it by hand:

```bash
# Every signature touching the vault, newest first — the same RPC call the
# rail's findReference makes, on the payTo address itself.
curl -s <RPC_URL> -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,
  "method":"getSignaturesForAddress","params":["<payTo>",{"limit":100,"commitment":"finalized"}]}'
```

```sql
SELECT deposit_id, payment_ref, status, amount_minor, received_minor,
       facilitator_tx_hash, meta->>'signature' AS signature
FROM oxsum.deposits WHERE rail='solana' ORDER BY created_at;
```

Expected: the two sets agree —

- Every `facilitator_tx_hash`/`meta.signature` appears in the on-chain list.
- Every on-chain transfer into `payTo` resolves to a deposit (`credited`,
  `expired` with a signature, `refunded`, or an exception row), to a refund
  signature out of the vault (a send, not a receive), or to an unrecorded
  item — which pending #1149's scan job reports automatically and which today
  means: find it, understand it, write it down.

A transfer with no matching deposit is money someone sent the vault that the
books never saw — investigate before dismissing it. A deposit whose signature
is not on-chain is a data bug, not a chain bug.

## If a credit looks wrong

Stop. Do not open another intent, do not re-pay, do not hand-patch the
deposits table.

- **A deposit credited but the wallet did not move** — the ledger entry
  `entry_id` names is the record; a missing or wrong `entry_id` on a
  `credited` row is a storage anomaly to investigate via
  `docs/operator-diagnostics.md`, not to fix in place.
- **A deposit that will not credit** — read its `meta` for the refusal:
  `signature` set means the rail saw the transaction and refused it
  (underpay, late, wrong mint, missing marker, or a signature that already
  credited another deposit — the cross-organization double-spend refusal).
  `signature` absent means the rail never located a transfer on the marker —
  the customer's paying transaction did not carry `reference` as a read-only
  non-signer key; the money is still in `payTo`, the deposit is a refund
  case.
- **A wrong credit** — `POST /api/v1/admin/deposits/{depositId}/reverse`
  reverses a `credited` deposit (ledger reversal, `reversed` status,
  `reversalEntryId`). It does not return chain funds — on Solana the money is
  already in the vault; the refund half (step 6) is a separate on-chain act.
  A reversal the wallet cannot cover (the customer already spent the credit)
  refuses `402 INSUFFICIENT_FUNDS` — that refusal is the ledger doing its job,
  not a reason to force it.
- **Every anomaly** lands on the exception queue once #1142 merges; until
  then the review surface is `GET /api/v1/admin/deposits` plus the SQL in
  `docs/operator-solana-refunds.md`. Escalate anything cross-organization (a
  signature that credited one org and was refused in another) — that is always
  a real refund owed, with an incident note on both deposits.

## Checklist before the first real dollar

- [ ] `OXSUM_SOLANA_MAX_MINOR` ≤ 5 000 000 verified in the running env — no
  intent can demand more than US$5.
- [ ] `payTo` is the vault, not a hot wallet; the vault's ATA for the USDC
  mint exists and is primed.
- [ ] The test wallet holds ~US$20 and nothing more; the operator knows which
  customer session it will pay as.
- [ ] `OXSUM_SOLANA_COMMITMENT=finalized` booted; the USDC mint's decimals
  check passed at startup.
- [ ] The `solana-deposits` job is scheduled (it is when the rail is
  configured — the 15 s pass log line appears once intents exist).
- [ ] The refund path is agreed: who supplies refund addresses, who signs at
  the multisig, where the refund signature is recorded.
- [ ] Recon works before money moves: `getSignaturesForAddress(payTo)`
  answers from the operator's host; the deposits SQL runs.
