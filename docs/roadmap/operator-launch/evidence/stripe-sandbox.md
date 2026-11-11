# Real Stripe test-mode evidence

Observed 2026-11-11 against the real `api.stripe.com` test mode on the live
deployment `oxsum.785777.xyz` (the `oxsum` Docker container on the `82.*`
server). Issue #741 (N15). This is a **full capture→webhook→credited**
certification for the Stripe deposit rail — the leg that the Airwallex sandbox
(`airwallex-sandbox.md`) could not complete because its hosted page is
captcha-gated.

## Credential and endpoint

The Stripe test-mode account is `acct_1UTw4r1WfAYEIQ1B` (livemode `false`). The
rail is wired with `OXSUM_STRIPE_SECRET_KEY` (a `sk_test_…` key) and
`OXSUM_STRIPE_WEBHOOK_SECRET`; `OXSUM_PUBLIC_URL` supplies the success/cancel
return origin. A webhook endpoint `we_1UaWo21WfAYEIQ1BislYKRen` points at
`https://oxsum.785777.xyz/api/v1/webhooks/stripe`, API version
`2025-10-29.clover` (v1 snapshot payload), subscribed to
`checkout.session.completed` + `checkout.session.expired`.

Before the two secrets were set the container mounted no stripe rail and
`POST /api/v1/webhooks/stripe` answered **404**; after they were set (container
recreated on the **same image digest** `sha256:124a8b8a…`, `--pull never`, no
binary change) the same POST answers **401** for an unsigned body — the rail is
mounted and `Stripe-Signature` verification is active.

No secret, key, token or card number is recorded here.

## What ran (verified real)

1. Registered + logged in a fresh personal account over the public URL
   (`POST /api/v1/auth/register`, `…/login`).
2. `POST /api/v1/topups/stripe` `{idempotencyKey, amountMinor:5000000}` →
   `200` with a **real Stripe Checkout URL**
   `https://checkout.stripe.com/c/pay/cs_test_a04YoYz2…` and
   `depositId 771eddea-c5da-470f-962d-a20da4252792`. The deposit row was created
   `pending`, `rail=stripe`, `amount_minor=5000000`,
   `payment_ref=<idempotencyKey>`.
3. Completed the hosted Stripe Checkout with the standard test card
   `4242 4242 4242 4242` (any future expiry / CVC / US ZIP) — Stripe redirected
   back to `https://oxsum.785777.xyz/app/topup?session=success`.
4. Stripe emitted `checkout.session.completed`
   (`evt_1UaWrF1WfAYEIQ1BGsnmb1Qe`, `payment_status=paid`,
   `data.object.id=cs_test_a04YoYz2…`), plus `payment_intent.succeeded`,
   `charge.succeeded`, `payment_intent.created`, `charge.updated` — all read
   back from `GET /v1/events`, not assumed.
5. oxsum verified the signed webhook and credited the deposit.

Post-state read back from Postgres (`oxsum.deposits`):

| field | value |
| --- | --- |
| `status` | `credited` |
| `amount_minor` | `5000000` |
| `received_minor` | `5000000` |
| `entry_id` | `36cda362-e292-5fbf-98ef-8490ea6d65c7` (ledger entry written) |

And the wallet balance over the API (`GET /api/v1/balance`):
`availableMinor = 10000000` = the 5,000,000 signup bonus + the 5,000,000 Stripe
topup. The credit is a real ledger entry (`entry_id`), not a UI assertion.

So the full **order → hosted capture → signed webhook → credited ledger entry**
walk is certified end-to-end on the Stripe rail.

## What is not exercised (honest scope)

- **Refund / reversal.** `deposit_refund::recover` deliberately returns
  `UnknownRefund` for `MerchantRail::Stripe` (`deposit_refund.rs`), a deferred
  scope recorded in `docs/decisions.md` (the N1041 change wired refund recovery
  for Airwallex, not Stripe). The Stripe `merchant_events` arm likewise maps
  only `checkout.session.completed`/`expired`; `charge.refunded` /
  `charge.dispute.created` would normalize to `Unsupported`, so the endpoint
  was trimmed to the two events the rail actually consumes. The refund leg is
  therefore **not certified** and is a known product gap, not a defect of this
  run.
- **`checkout.session.expired`** was subscribed and reaches the same verify
  path; no expiring session was left to lapse in this run, so only the
  `completed`→`credited` transition is live-verified.

## Environment boundaries

- **Binary**: the served image is the same `latest`-tagged digest that has been
  running since 2026-11-01 (`sha256:124a8b8a…`). It **predates** the merged
  #1040/#1043 changes, but the Stripe rail needs neither (the #1040 TLS cap is
  Airwallex-destination-scoped; #1043 is the Airwallex refund arm). Only env was
  added — `oxsum.env` gained `OXSUM_STRIPE_SECRET_KEY` +
  `OXSUM_STRIPE_WEBHOOK_SECRET`; the container was recreated with the identical
  digest and identical flags (`--network host`, `--env-file`,
  `--restart unless-stopped`). This is an env change, **not** a candidate
  promotion; the production binary still awaits the normal verified-candidate
  gate.
- **Public URL**: `oxsum.785777.xyz` is served through the `ragent`
  cloudflared tunnel on the `82.*` host (`→ 127.0.0.1:3001`). Stripe reached it
  directly over the public internet — no local relay, no fake-ip mediation,
  unlike the Airwallex run.
- **Test mode, not production**: `livemode=false`, `4242…` test card, no real
  money moved. Amount display on Stripe's page rendered the minor-unit intent
  as `US$50,000.00` (Stripe shows the raw minor→major conversion of 5,000,000
  cents); that is a display artifact of the demo amount, not an accounting
  error — `received_minor=5000000` is what credited.

## Verdict

Real, not fixture, and **complete for the capture leg**: the server minted a
real Stripe Checkout session, a real hosted payment cleared it, Stripe signed
and delivered `checkout.session.completed`, oxsum verified the signature and
credited `5,000,000` minor units to the wallet via a real ledger entry. The
Stripe deposit rail's order→capture→webhook→credit path is certified on test
mode. The refund leg remains a deliberate deferred gap (Stripe `recover` is
`UnknownRefund` by design today).
