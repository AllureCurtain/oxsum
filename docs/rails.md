# The rail honesty matrix

One row per deposit rail: what environment it has actually been exercised in, and what "verified"
means there. oxsum never reports a rail as "working" past what has been proven — the distinction
between *code shipped* and *money actually moved* is the difference between a demo and a payment
rail.

## Legend

| Level | Meaning |
| --- | --- |
| **unit-tested** | the request/response/wire shape is exercised in tests, against a stub or an in-code fake — no external call leaves the process |
| **integration-tested** | the full `pending → confirmed → credited` walk runs in a process-shared suite against a stub service — the deposit row, the ledger credit and the webhook handling all real, only the PSP is mocked |
| **sandbox-verified** | the rail has completed at least one real end-to-end round trip against the provider's own sandbox / testnet / hosted test environment — a real (test) credential was used, real webhook deliveries arrived |
| **live-verified** | the rail has moved real money in a deployment — the strongest claim oxsum makes, and only the deployer can certify it |

## The matrix

| Rail | Path | Level | What was actually run | Notes |
| --- | --- | --- | --- | --- |
| **Manual** (`rail='manual'`) | `POST /api/v1/topups` | **live-verified** | every development seed, every test org, every CI run | no PSP — the operator's own books are the rail; this is what "receive a real payment" means when no external rail exists |
| **Redemption** (`rail='redemption'`) | `POST /api/v1/redemptions` | **live-verified** | every seed run mints and redeems a batch against the real ledger | no PSP — a minted code *is* the instrument, the verifier is a hash compare |
| **Stripe** (`rail='stripe'`) | `POST /api/v1/topups/stripe` + `POST /api/v1/webhooks/stripe` | **integration-tested** | the `pending → confirmed → credited` walk runs against a stubbed Stripe (`crates/server/tests/stripe.rs`); no `sk_test_*` charge has been run end-to-end | a deployment's own Stripe sandbox must do the live check — see "for deployers" below; superseded in practice by the Hyperswitch rail below |
| **Airwallex** (`rail='airwallex'`) | `POST /api/v1/topups/airwallex` + `POST /api/v1/webhooks/airwallex` | **integration-tested** | the stubbed-wire suite (`crates/server/tests/airwallex.rs`) runs the deposit walk and signature check against documented Airwallex request/response fixtures; no Airwallex sandbox account has been exercised | a deployment with `OXSUM_AIRWALLEX_*` set should run one real sandbox payment before offering the rail; the Hyperswitch path below is the recommended alternative |
| **Hyperswitch switch** (`rail='hyperswitch'`) | `POST /api/v1/topups/hyperswitch` + `POST /api/v1/webhooks/hyperswitch` | **sandbox-verified** | a self-hosted switch sidecar (`infra/hyperswitch/`) plus a Stripe test connector ran a real payment `pay_…` → `payment_succeeded` webhook → **25 cr credited** on first delivery attempt; a cancelled payment expired cleanly (issue #320); a reversal issued `POST /refunds` and the `refund_*`/`dispute_*` webhooks walked `meta` correctly (issue #360) | this is the rail a deployment is expected to use — it fronts Stripe, Airwallex and ~60 other connectors with one webhook vocabulary; the manual tests above were run against a *locally self-hosted* switch, not Juspay's hosted sandbox |
| **x402** (`rail='x402'`) | `POST /api/v1/topups/x402` | **unit-tested** | the `RailVerifier` seam is exercised two ways: `Ed25519RailVerifier` (the test-scheme stand-in, `OXSUM_X402_VERIFYING_KEY`) signs and verifies a canonical body in unit tests; `X402FacilitatorVerifier` (the real impl, `OXSUM_X402_FACILITATOR_*`) delegates `verify`/`settle` to an x402-rs sidecar over HTTP and is covered by the same stub-based tests | **no real chain settlement has been run** — the facilitator-delegated leg is implemented and tested, but a real EIP-3009 transfer on a testnet is the deployment's own certification; see "for deployers" below |

## What "verified" does not cover

- **Refunds on rails that do not offer them.** Manual, redemption, Stripe, Airwallex and x402 deposits can all be `reversed` in the ledger — but only `hyperswitch` currently issues a real `POST /refunds` call to the provider. Every other rail's `refundStatus` is `null`, meaning "no rail refund was attempted", not "the rail refused one".
- **The deposit reversal path itself.** `POST /api/v1/admin/deposits/{id}/reverse` is integration-tested on every rail — the ledger half is identical across them. What differs is whether a *provider-side* refund also happens.
- **Chargeback / dispute webhooks.** Only `hyperswitch` registers `dispute_opened` (issue #360); the other rails do not surface disputes because their test surfaces did not reach that event. A deployment running a real Stripe/Airwallex rail would need to map its provider's dispute event onto the same `deposit.disputed` webhook before that signal exists.

## For deployers

A rail is ready to offer to your users when its matrix row reads **sandbox-verified** or better and
you have run at least one real payment through it on your own infrastructure. The suites ship
`crates/server/tests/{rail}.rs` as the harness — running them against real test credentials
(`sk_test_…`, an Airwallex sandbox account, a funded x402 testnet wallet) is a one-line change away;
the contract is written, only the credential is missing.

| Rail | What you need to run a real check |
| --- | --- |
| Stripe | `sk_test_…` + a webhook endpoint (`cloudflared`/`ngrok` if local) pointing at `/api/v1/webhooks/stripe`; test card `4242 4242 4242 4242` |
| Airwallex | an Airwallex sandbox account + `OXSUM_AIRWALLEX_*` env vars + a public webhook URL |
| Hyperswitch | `infra/hyperswitch/` compose up + a Stripe connector configured in Control Center + a public URL for `OXSUM_PUBLIC_URL` |
| x402 | a funded wallet on a supported testnet (`base-sepolia` is free via faucet) + `ghcr.io/x402-rs/x402-facilitator` running + the `OXSUM_X402_FACILITATOR_*` set |

## Honest summary

- **Money you can receive today without trusting a stub:** manual, redemption, and Hyperswitch
  (with its sandbox already verified on the wire).
- **Money that needs a real credential before you claim it:** Stripe, Airwallex, x402 — all three
  are code-complete and wire-tested; all three need a sandbox run on your infrastructure before
  you tell anyone they work.
- **Money you cannot receive here:** anything not on this list — no PayPal rail, no bank transfer,
  no other connector exists yet. The `deposits` seam is the place a new one lands, but none is
  pending.
