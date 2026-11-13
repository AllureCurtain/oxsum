# Solana top-up example

A Node walkthrough (`topup.mjs`, Node ≥ 20.16) that exercises the `solana`
deposit rail end to end against a live deployment and asserts each step out
loud — intent → on-chain reference-marked transfer → confirm and poll credit.
It is the scripted counterpart of the `/app` top-up page's Solana tab: same
two endpoints, same Solana Pay recipe, driven from the command line instead
of a wallet.

Chain access needs `@solana/web3.js` 1.98.4 (the pinned version —
`npm install` here first). The 3.x line renames the APIs these scripts use
and is not a drop-in upgrade.

## The rail, in one paragraph

`POST /api/v1/topups/solana` opens a `pending` deposit whose `payment_ref`
is a freshly minted `reference` account key and answers
`{reference, payTo, amountLamports, splToken, expiresAt}` — the terms the
paying transaction must honor at the intent's locked
`OXSUM_SOLANA_RATE_MICRO`. The customer's wallet transfers at least
`amountLamports` of native SOL (or the configured SPL mint's base units) to
`payTo`, carrying `reference` as a **read-only non-signer** account — the
marker `getSignaturesForAddress` indexes. `POST /api/v1/topups/solana/confirm`
takes the `reference` (never the signature — the rail locates and validates
the transaction itself), or the 15-second `solana-deposits` poll settles the
same intents on its own. Credit lands once under `solana:<signature>`;
a replayed confirm on a credited deposit answers `404`.

## Prerequisites

- A live deployment with the rail configured — the all-or-none set:

  ```sh
  OXSUM_SOLANA_RPC_URL=http://127.0.0.1:8899   # the local validator below
  OXSUM_SOLANA_PAY_TO=<payTo public key>       # keys.mjs prints it
  OXSUM_SOLANA_RATE_MICRO=200000000            # 200 credits per whole SOL
  # OXSUM_RAILS=solana                         # optional explicit declaration
  ```

- A Solana JSON-RPC endpoint to pay through. The honest, reproducible choice
  for a development loop is a **local `solana-test-validator`** — Docker:

  ```sh
  docker run --rm -p 8899:8899 -p 8900:8900 \
    solanalabs/solana:stable solana-test-validator --reset
  ```

  The public devnet faucet answers HTTP 410 to `requestAirdrop` now, so the
  validator's unlimited airdrop is what makes the script self-contained.
  Against real devnet, set `SOLANA_RPC_URL=https://api.devnet.solana.com`
  and fund `payer.json` by hand at <https://faucet.solana.com>.

- A registered customer on the deployment (`email:password` for
  `OXSUM_SOLANA_PAYER`). A deployment is invite-only until its business
  owner selects open registration (`POST /api/v1/business/registration`) or
  invites the customer — the
  [`../consumption`](../consumption/README.md) walkthrough drives that whole
  setup if the deployment is fresh.

## Running it

```sh
cd examples/solana-topup
npm install

# 1. Mint the two throwaway keypairs and airdrop them (local validator).
node keys.mjs
# → PAY_TO=… PAYER=…   Point OXSUM_SOLANA_PAY_TO at PAY_TO and (re)start
#   the deployment so the rail picks it up.

# 2. Run the walkthrough — 1000 minor-credits by default.
OXSUM_BASE_URL=http://127.0.0.1:3000 \
OXSUM_SOLANA_PAYER=customer@example.com:correct-horse-battery \
node topup.mjs 1000
```

What it asserts, in order:

1. **Login and balance.** Personal session for the customer; reads the
   starting `availableMinor`.
2. **Open the intent.** `POST /api/v1/topups/solana` answers the terms; a
   native-SOL deployment answers `splToken: null`.
3. **Pay it on chain.** A `SystemProgram.transfer` of `amountLamports` to
   `payTo` with `reference` attached as a read-only non-signer key — the
   same shape a `@solana/pay` wallet produces.
4. **Settle now.** `POST /api/v1/topups/solana/confirm` with the
   `reference` credits the wallet by exactly `amountMinor`.
5. **Replay is a no-op.** The same confirm answers `404` and moves nothing —
   the deposit is `credited`, not `pending`.
6. **The poll covers the unattended path.** A second intent paid on chain
   but never confirmed is credited by the `solana-deposits` poll within its
   15-second cadence — the path the top-up page's balance poll leans on
   after its 60-second intent window lapses.

`pay.mjs` is the single-payment form for driving step 3 by hand (for
example with a wallet-generated reference): `node pay.mjs payer.json <payTo>
<reference> <lamports>`.

## Caveats worth knowing

- **Rent-exempt minimum.** A brand-new `payTo` account cannot receive a bare
  few-thousand-lamport deposit — the cluster refuses the transfer
  (`insufficient funds for rent`). `keys.mjs` airdrops 0.1 SOL to `payTo`
  for this reason; a real deployment's deposit address is already funded, so
  this is a scratch-loop concern only.
- **Intent expiry.** An unpaid `pending` intent lapses at `expiresAt`
  (`OXSUM_SOLANA_INTENT_TTL_SECS`, default 600 s). A payment that lands after
  expiry is not credited — it is money sent to `payTo` outside any deposit,
  recovered as an operator matter, not by this rail.
- **`confirmed` vs `finalized`.** The walkthrough uses `confirmed`, the
  rail's default. `OXSUM_SOLANA_COMMITMENT=finalized` is one flag for a
  high-value deployment; it only delays the same checks.
- **SPL/USDC intents.** The scripts cover the native-SOL path. An SPL intent
  (`OXSUM_SOLANA_SPL_TOKEN_MINT` set) needs a token transfer carrying the
  same `reference` marker; the rail validates it through
  `postTokenBalances` instead of lamport deltas.
- **Throwaway keys.** `payer.json`/`payto.json` hold raw secret keys for a
  scratch validator or devnet. Never point the scripts at mainnet, and never
  reuse the keypairs.

## Where the pieces live

| Piece | Path |
| --- | --- |
| Rail contract | `crates/server/openapi.yaml` — `/api/v1/topups/solana{,/confirm}` |
| Core rail | `crates/core/src/solana.rs` (`SolanaRailVerifier`, `ChainLookup`) |
| RPC client | `crates/integrations/src/solana.rs` (`SolanaRpcClient`) |
| Poll job | `solana-deposits` kind, `crates/core/src/jobs.rs` |
| UI counterpart | `web/src/pages/Topup.tsx` Solana tab |
| Design note | `docs/decisions.md` — 2026-11-13, issue #1117 |
