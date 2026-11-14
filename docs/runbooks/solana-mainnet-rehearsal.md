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

The mainnet target state is the batch tracked under #1117. The merged rail
carries the multi-asset whitelist (`OXSUM_SOLANA_ASSETS`, #1140),
oracle-priced SOL intents via Pyth (#1139 — a `sol` intent locks the Hermes
quote's rate into its `meta` at open), the Solana Pay QR on the top-up page
(#1141), the exception queue plus the pull-payment refund flow (#1142 —
`underpaid` / `overpaid` / `late` / `flagged` / `refund_pending` / `refunded`),
the Squads v4 multisig custody pin (#1143), pre-credit sanctions screening on
the payer address (#1144), the authenticated endpoint pool with failover and
rail metrics (#1145), the dashboard's live status flow (#1148), the admin
reconciliation CSV + unrecorded-transfer scan (#1149), the wallet-adapter pay
button beside the QR (#1147 — a convenience, not a rail change), the optional
Helius webhook that accelerates verification (#1150 — the poll path below
already credits; the webhook only shortens the wait), and USDT behind its own
opt-in flag (#1151); `finalized` commitment, the 15 s `solana-deposits` poll
and the operator refund runbook stand from the devnet-hardened batch. Pending
companion work:

- #1152 — end-to-end deposits against a local test-validator

## Step 0 — offline prerequisites (do these first, by hand)

These are human prerequisites, not code — nothing in the repo provisions them.
Complete every line before configuring the deployment.

- [ ] A Squads v4 multisig vault (or equivalent) on mainnet-beta to act as
  `payTo`: 2-of-3 signers — operations lead, finance, one offline backup key.
  Record the vault address **and** the multisig account; the server holds
  **no** spend key (issue #1143: `OXSUM_SOLANA_MULTISIG_ACCOUNT` names the
  multisig account and the boot proves `payTo` is its `vault index 0` — a
  mismatch refuses startup; see docs/self-hosted-solana-multisig.md). If a
  multisig is not yet in place for the rehearsal,
  a dedicated single-signer address is acceptable **for the rehearsal only** —
  a mainnet-shaped endpoint then refuses to boot unless the deliberate
  `OXSUM_SOLANA_ALLOW_HOT_WALLET=1` opt-out is set (issue #1189; the boot
  logs a loud warn marking the accepted risk) — treat its
  key like the deployment's cash drawer, and migrate to the multisig before
  any customer traffic.
- [ ] An authenticated Solana RPC endpoint (Helius or Triton key) — never the
  public `api.mainnet-beta.solana.com` for a production gate. The rail pools
  `OXSUM_SOLANA_RPC_URLS` with failover (issue #1145): a second provider
  listed there is the rehearsal's own failover proof.
- [ ] A funded test wallet holding at most ~US$20 total — enough for ~4 small
  transfers plus fees. This wallet plays the customer. Phantom, Solflare or a
  CLI keypair are all fine; the rail does not care what sends the transfer,
  only that it carries the `reference` marker.
- [ ] A Chainalysis (or equivalent screening provider) API key, or a non-empty
  local SDN list file — `OXSUM_SOLANA_SANCTIONS_ENABLED=1` is REQUIRED on a
  mainnet-shaped endpoint (issue #1179: the boot refuses to start with the
  variable unset), so the rehearsal screens with at least the embedded OFAC
  SDN Solana list; `OXSUM_SOLANA_SANCTIONS_API_KEY` adds the free API's
  coverage and `OXSUM_SOLANA_SDN_LIST_PATH` a refreshed file.
- [ ] The operator token for the deployment and a `psql` line to the database
  (the Compose file's `postgres` service or your managed instance).
- [ ] A registered customer organization on the deployment — the rehearsal
  uses real auth; `/api/v1/topups/solana` is a session endpoint.

## Step 1 — configure and boot for mainnet

Set the rail's environment in `deploy/.env` (or the env source your install
uses). The rail is all-or-none: `OXSUM_SOLANA_RPC_URLS` (or
`OXSUM_SOLANA_RPC_URL`), `OXSUM_SOLANA_PAY_TO`, `OXSUM_SOLANA_RATE_MICRO`
together, or the endpoints answer 503.

```dotenv
# The authenticated endpoint pool, never the public cluster endpoint —
# comma-separated providers the client fails over across (issue #1145); a
# second provider listed here is the rehearsal's own failover proof.
OXSUM_SOLANA_RPC_URLS=https://mainnet.helius-rpc.com/?api-key=<key>

# The multisig vault every intent pays — and the custody pin that proves it:
# the boot derives the multisig's `vault index 0` PDA offline and refuses to
# start unless PAY_TO equals it (issue #1143). On a mainnet-shaped endpoint
# MULTISIG_ACCOUNT is required (issue #1189): unset, the boot refuses. The
# single-signer rehearsal variant is the only place
# OXSUM_SOLANA_ALLOW_HOT_WALLET=1 belongs — it boots under a loud warn that
# PAY_TO is an unverified hot wallet holding real funds.
OXSUM_SOLANA_PAY_TO=<vault base58 address>
OXSUM_SOLANA_MULTISIG_ACCOUNT=<multisig account base58>
# OXSUM_SOLANA_ALLOW_HOT_WALLET=1  # rehearsal-only: unpinned hot wallet

# Mainnet-grade finality. Finalized is the default; state it explicitly so a
# later reader cannot mistake the posture.
OXSUM_SOLANA_COMMITMENT=finalized

# USDC as the rehearsal asset, via the legacy single-mint form (the rail
# also carries a multi-asset whitelist since #1140 — one deployment can
# serve both SOL and USDC — but the legacy form is the right choice for a
# dollar-denominated rehearsal: its one asset locks the static rate, never
# an oracle).
OXSUM_SOLANA_SPL_TOKEN_MINT=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
OXSUM_SOLANA_DECIMALS=6
# 1 USDC = 1 USD of credit at 6 decimals: 1_000_000 minor-credits per whole
# unit. Under #1139 this figure is required by the legacy form — a `sol`
# whitelist intent on a mainnet deployment prices off the Pyth oracle at
# open instead.
OXSUM_SOLANA_RATE_MICRO=1000000

# The safety bounds — the whole point of the rehearsal cap.
# 5_000_000 minor-credits = US$5.00 at the minor-units convention (1 credit =
# 1_000_000 minor, priced $1/credit in this cut). One intent can never demand
# more than $5 on-chain.
OXSUM_SOLANA_MIN_MINOR=1000
OXSUM_SOLANA_MAX_MINOR=5000000
OXSUM_SOLANA_OPENS_PER_MINUTE=3

# The rail's 900 s default is already the 15-minute window a mainnet rehearsal
# wants; pinning it here keeps the expired-then-paid scenario staging intact
# whichever way a future default moves.
OXSUM_SOLANA_INTENT_TTL_SECS=900

# REQUIRED on a mainnet-shaped endpoint (issue #1179): the boot refuses to
# start with the variable unset. `1` screens every payer pre-credit — the
# embedded OFAC SDN Solana list always, plus the Chainalysis free API when
# SANCTIONS_API_KEY is set (an API key or a non-empty SDN_LIST_PATH file is
# the rehearsal's coverage; the embedded list alone is the floor). `0` is
# only legal because it is explicit — the boot warns loudly that screening is
# off on a mainnet endpoint, and an unscreened rehearsal is not a rehearsal.
OXSUM_SOLANA_SANCTIONS_ENABLED=1
OXSUM_SOLANA_SANCTIONS_API_KEY=<chainalysis key>
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
  — those mean an endpoint is unreachable; they also count on the rail's
  metrics (issue #1145: `solana_rpc_*` series keyed by endpoint host, never
  the URL's API key).

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

- The dashboard's **Solana** tab renders the intent as a Solana Pay
  transfer-request URL plus a QR (issue #1141): scan it with the test wallet
  and approve — the wallet fills recipient, amount and `reference` itself.
- Or build a `TransferChecked` SPL transfer by hand for `amountLamports` of
  the USDC mint to `payTo`'s ATA, with `reference` added as a read-only
  non-signer account key — `examples/solana-topup/pay.mjs` does exactly this
  against a live deployment, or your wallet's "custom accounts" flow. The
  manual `payTo`/amount/`reference` fields stay on the tab beneath the QR.

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
verify seam — until the rail names it (issue #1142).

- Confirm answers `403` while the row is `pending` and the shortfall not yet
  located; the verify walk that does locate it — the poll's or the confirm's
  own — marks the row `underpaid` on the exception queue (a confirm answers
  `409` at that moment; a later one `404`, the row no longer an open intent).
  `GET /api/v1/admin/solana/exceptions?status=underpaid` lists it,
  `meta.delivered`/`meta.receivedCredits` recording the shortfall — the money
  sits in `payTo` for the refund flow (step 6) to rehearse.

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

`status` = `overpaid`, `amount_minor` = 2 000 000, `received_minor` =
2 500 000. The wallet gained US$2.00, the vault gained US$2.50. The US$0.50
surplus is an overpaid exception to refund or grant (step 6) — issue #1142
marks the row `overpaid` (the credit stands: `entry_id` proves the opened
amount booked) and places it on `GET /api/v1/admin/solana/exceptions` and the
customer's own `GET /api/v1/topups/solana/deposits` list.

## Step 5 — expired then paid

**Action.** Open an intent and **do not pay it** until `meta.expiresAt` has
passed (the 900 s TTL gives room; the poll's sweep marks the row `expired`
once the deadline lapses unpaid). Then pay it anyway, carrying the reference.

Expected: the payment lands on-chain but never credits. Expiry is judged on
the paying transaction's `blockTime` against `meta.expiresAt` — a `blockTime`
at/after the deadline is refused even though the row sat unclaimed. Which
refusal surface the rehearsal sees depends on a race:

- The verify walk locates the transfer while the row is still `pending` —
  a payment sent right at the deadline, or a poll tick that ran late — and
  the row marks `late` on the exception queue (issue #1142): the operator
  lists it at `GET /api/v1/admin/solana/exceptions?status=late`, the customer
  at `GET /api/v1/topups/solana/deposits`, and a confirm on its `reference`
  answers `409` at that moment (`404` once the row has left `pending`).
- The 15 s sweep usually wins: the row flips `expired` first, the later
  payment finds no `pending` intent, and the transfer sits in `payTo`
  unrecorded — the same refund case step 6 rehearses, found by the
  reconciliation pass in step 8 rather than the queue.

**Verify:**

```sql
SELECT status, meta->>'expiresAt' AS expiry, meta->>'signature' AS signature
FROM oxsum.deposits WHERE rail='solana' AND payment_ref='<reference>';
```

`status` = `late` or `expired`; cross-check the signature's `blockTime` on
the chain: `getTransaction <signature>` — it postdates `expiresAt`. Money in
`payTo`, never credited — a genuine refund candidate, rehearsed next.

## Step 6 — refund the exception

The rail **never auto-refunds to the payment source** — a refund is a business
decision plus a key-signing act. The pull-payment model (issue #1142, the same
flow docs/operator-solana-refunds.md walks) formalizes it: operator initiates
→ customer supplies a refund address → operator approves → the multisig pays
→ the signature is recorded back → the deposit closes `refunded`.

**Action.** Pick the underpaid (step 3) or late-paid (step 5) deposit.

1. Decide: refund the asset on-chain (same mint, same delivered base units,
   to an address **the customer supplies**) or credit the account manually.
   The refund repays the asset the customer paid, never the credit figure.
2. Open the refund: `POST /api/v1/admin/solana/{depositId}/refund
   {"reason":"…"}` marks the row `refund_pending` (audited
   `solana.refund.request`).
3. The customer names the refund address on their own surface:
   `POST /api/v1/topups/solana/{depositId}/refund-address {"address":"…"}` —
   the exception list under the dashboard's top-up form carries the same
   form (issue #1148).
4. Approve: `POST /api/v1/admin/solana/{depositId}/refund-approve` freezes
   the address and clears the payout for signing (audited
   `solana.refund.approve`).
5. Send the refund from the vault through the multisig — a plain
   `TransferChecked` for the delivered figure, **never** reusing the deposit's
   `reference`. Record the refund signature.
6. Record it back: `POST /api/v1/admin/solana/{depositId}/refund-complete
   {"signature":"…"}` flips `refund_pending` → `refunded` (audited
   `solana.refund.complete`) — bookkeeping only; the server holds no spend
   key and posts no ledger entry. When the decision was to credit instead of
   return, post the manual receipt
   (`POST /api/v1/admin/organizations/{id}/manual-receipts`) with
   `payment_ref` naming the original deposit and the reason naming the refund
   signature.

**Verify:**

```sql
SELECT status, meta->>'signature' AS paid_sig, meta->>'refundSignature' AS refund_sig
FROM oxsum.deposits WHERE rail='solana' AND deposit_id='<id>';
```

`status` = `refunded`, `refund_sig` set, the audit trail names the approving
operator. `getTransaction <refund_sig>` shows the vault → customer transfer
of the delivered figure.

## Step 7 — screening a flagged payer

With `OXSUM_SOLANA_SANCTIONS_ENABLED=1` (issue #1144 — REQUIRED on a
mainnet-shaped endpoint since issue #1179: the boot refuses to start with
the variable unset, and an explicit `0` boots only under a loud warning) the
payer's debited signer is screened before the ledger credit books:
the local OFAC SDN Solana list always, plus the Chainalysis free API when
`OXSUM_SOLANA_SANCTIONS_API_KEY` is set. A hit does not credit and does not
refund — the deposit moves to `flagged` and into operator review at
`GET /api/v1/admin/deposits/solana/flagged`, a deny-until-cleared posture
(an external-API failure keeps the deposit `pending` and retries, never
silently opens the gate).

**Rehearsal.** Tripping the real lists needs a sanctioned address — do not
fund one. Rehearse the *shape*:

- Boot with `OXSUM_SOLANA_SANCTIONS_ENABLED=1` and pay a small intent from
  the test wallet: the screen runs on every payment — an unreachable
  Chainalysis API (block egress, or unset the key) leaves the deposit
  `pending` and retries rather than crediting unscreened; the local list
  alone still screens clean payers through.
- Watch the address the rail screens — the transaction's debited signer —
  against a scratch wallet's own payer; that is the address a real flag
  would name.
- The review path is the exception queue: a `flagged` row carries
  `meta.screening` `{verdict, source, reason, address, at}` for the reviewer
  and the same refund lifecycle applies — the operator may refund it
  on-chain or leave it parked; it is never auto-cleared into a credit.

**Verify:** for a simulated hit — a local `OXSUM_SOLANA_SDN_LIST_PATH` file
carrying the scratch payer's base58 address is the rehearsal's tripwire —
`status` = `flagged`, `meta.screening` names the list and the payer address;
`GET /api/v1/admin/deposits/solana/flagged` lists it; the row parks, never
credited.

## Step 8 — reconcile against the vault

Reconciliation is line-by-line: every `solana` deposit row should map to one
on-chain transfer into `payTo`, and every transfer into `payTo` should map to
a deposit row, an exception or a deliberate test artifact.

**Action.** The rail ships both halves (issue #1149):
`GET /api/v1/admin/solana/reconciliation/deposits.csv?from=&to=` exports the
deposit table's side as line-by-line CSV (the intent's `paymentRef`, the
paying `signature`, payer, locked `asset`/`rateMicro`, `delivered`/`expected`
in base units, status and the refund lifecycle), and
`POST /api/v1/admin/solana/reconciliation/scan` walks `payTo`'s bounded
on-chain signature history and reports each transfer that paid the vault but
matches no recorded deposit. The manual check underneath them is still
instructive:

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
  `overpaid`, `expired` with a signature, `refunded`, or an exception row —
  `underpaid`/`late`/`flagged`/`refund_pending`), to a refund
  signature out of the vault (a send, not a receive), or to an unrecorded
  item — which the scan reports as `matchedIntent: false` and which today
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
- **Every anomaly** lands on the exception queue (issue #1142):
  `GET /api/v1/admin/solana/exceptions` is the work list, the dashboard's
  **Admin → Solana exceptions** section renders it, and the customer's own
  `GET /api/v1/topups/solana/deposits` shows their rows. Escalate anything
  cross-organization (a signature that credited one org and was refused in
  another) — that is always a real refund owed, with an incident note on both
  deposits.

## Checklist before the first real dollar

- [ ] `OXSUM_SOLANA_MAX_MINOR` ≤ 5 000 000 verified in the running env — no
  intent can demand more than US$5.
- [ ] `payTo` is the vault, not a hot wallet — `OXSUM_SOLANA_MULTISIG_ACCOUNT`
  set, so the boot proved it (mainnet refuses to boot without it —
  `OXSUM_SOLANA_ALLOW_HOT_WALLET` is the rehearsal-only opt-out) — and the
  vault's ATA for the USDC mint exists and is primed.
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
