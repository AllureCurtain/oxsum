# Payment rail evidence

This matrix distinguishes implemented adapters from exercised infrastructure.
A successful fixture establishes the tested state transitions and conservation;
it does not establish an actual payment, current credential access or readiness
of another deployment. Historical observations retain their original date and
scope. A new candidate needs its own acceptance evidence.

## Evidence tiers

| Tier | Meaning |
| --- | --- |
| Software fixture | Rust/HTTP/database tests with explicit local provider stubs and signed fixture events |
| Local switch fixture | An actual self-hosted switch with a dummy connector; signing or posting a fixture callback is not automatic PSP delivery |
| Provider sandbox | A real processor test account completes checkout, automatic authenticated callback, fulfillment and applicable refund/reconciliation |
| Live payment | An authorized payment moves real money through the configured deployment; testnet and seeded balances do not qualify |

A rail may have passed one operation while another remains unperformed. Record
source SHA, artifact identity, configuration version, operation IDs and exact
amounts for each observation; never record secret credentials or card data.

## Current software and historical evidence

| Rail | Software scope | Retained external evidence and limits |
| --- | --- | --- |
| Manual receipt | Staff/deployment-authorized receipt, unique reference, idempotent fulfillment and ledger correction; customer self-credit is retired | Local tests prove the software. The staff member's out-of-band receipt verification remains necessary; fixtures do not prove a bank transfer |
| Redemption | Minted instrument, hashed redemption secret, one-time credit and concurrency checks | Seed redemption is a local fixture, not a live-money certification. Distinguish promotional grants from purchased funds |
| Stripe (**active primary rail**) | Compatibility deposit checkout and purpose-specific order adapter, versioned signed intake; order refund adapter; compatibility deposit-refund recovery (`v1/refunds` + `refund.updated`/`charge.refund.updated` event settlement) | The deposit capture→webhook→credit leg is live-verified on test mode 2026-11-11 ([evidence](roadmap/operator-launch/evidence/stripe-sandbox.md)): a real Checkout session cleared, `checkout.session.completed` signed and credited. Deposit-refund recovery (#1047) is stub-certified — a reversed stripe deposit resolves `cs_…`→`pi_…`, mints a `re_…` under `Idempotency-Key`, and settles on a signed `refund.updated`; no live Stripe refund has been exercised. The order-refund adapter resolves the same `cs_…`→`pi_…` before issuing `v1/refunds` (stub-certified), so no arm passes a session id where Stripe expects the intent |
| Airwallex | Payment intent/checkout adapter and versioned signed intake; order refund adapter; compatibility deposit-refund recovery (`refunds/create` + `refund.settled`/`refund.failed` event settlement) | The 2026-10-25 notes (#377) recorded API/webhook work. The later [T31 follow-up](roadmap/self-hosted-v1/evidence/T31-followup.md) records credential login failure (403). Historical notes do not certify the current account, browser checkout or candidate. Transport note: `api-demo.airwallex.com`'s TLS 1.3 answers are unreadable by rustls, so `ProviderHttp` caps **that destination** at TLS 1.2 (#1040) — env-rail and merchant-scoped calls to it both work; `api.airwallex.com` keeps TLS 1.3. The deposit-refund recovery path (#1041) is stub-certified only — no live Airwallex refund has been exercised |
| Hyperswitch (**optional orchestration layer — deferred**) | Unified Checkout/payment and refund adapters, versioned callbacks, order fulfillment; distinct compatibility deposit reversal path | #320/#360 contain earlier manual observations. The old disposable switch data is lost. The fresh pinned local-switch work is [#757](https://github.com/AllureCurtain/oxsum/issues/757), using a dummy connector. Genuine PSP credentials, SDK browser completion and automatic callback certification must be recorded separately. **Deployment status**: not mounted on the live deployment — it needs a self-hosted six-service switch and its only distinct value is multi-PSP orchestration, so it is deferred until a second processor is actually needed (feasibility on the `82.*` host: disk fits after cleanup, the 3.6 GiB host RAM does not — see #1048) |
| x402 | V1 exact-scheme verification/settlement through a facilitator and compatibility deposit path | On 2026-10-25, Base Sepolia transaction [0xe25f9c…4038](https://sepolia.basescan.org/tx/0xe25f9c399dbb2f620d236246ac382b8d7cf9760bc6ec4d184442a7bff0cc4038) was recorded under #372. This is historical testnet evidence, not live money or current-candidate certification |

The current T31 record and the
[release checklist](roadmap/self-hosted-v1/RELEASE-CHECKLIST.md) take precedence
over an older blanket “verified” label. No genuine PSP or live-payment
certification has been performed by the November 8 operator-launch documentation
update. Model-provider certification is separate from payment certification.

## Refunds, disputes and migration boundaries

Purpose-specific orders use `/api/v1/orders` and the staff refund workflow.
Compatibility deposit endpoints use `/api/v1/topups/{rail}` and the deployment
admin reversal path. Their implementations and callback vocabularies differ:
a purpose-order refund adapter existing for Stripe or Airwallex does not mean
legacy deposits automatically issue the same refund.

The compatibility reversal books a local correction for supported deposits;
its Hyperswitch and Airwallex arms also attempt the external refund. A null
provider refund status means no external refund was attempted, not that money
was returned.
Unknown outcomes require reconciliation. The retry defects found during fresh
switch testing are tracked in [#760](https://github.com/AllureCurtain/oxsum/issues/760)
and [#763](https://github.com/AllureCurtain/oxsum/issues/763); consult the installed
revision before enabling recovery against a real processor.

Only event kinds explicitly normalized by the rail can drive a dispute or
refund transition. Do not infer a complete processor dispute lifecycle from a
successful payment callback. See [integration contracts](integrations.md) and
[OpenAPI](../crates/server/openapi.yaml) for the accepted vocabulary.

## Prepare a genuine sandbox acceptance

| Rail | Required input and exercise |
| --- | --- |
| Stripe | Processor test-account key, exact merchant identity/mode and signing secret; complete hosted checkout, automatic callback, duplicate handling and refund with the processor's test instructions |
| Airwallex | Usable sandbox client/key pair, matching signing secret and supported checkout integration; authenticate first, then exercise actual payment and callback rather than synthesizing success |
| Hyperswitch | Pinned switch/SDK, durable fresh merchant/profile/key records, HTTPS SDK backend and callback topology, and an actual PSP test connector credential. The `paypal_test` dummy connector is a local fixture |
| x402 | Supported testnet/facilitator, matching asset/recipient and funded test wallet; record the actual transaction and callback/settlement outcome as testnet evidence |

Configure merchant credentials through the supported owner or explicit bootstrap
interface; preserve versions needed by late callbacks. A configured merchant, a
reachable URL or a returned client secret is not completed checkout. The Rust
suites deliberately start local stubs: changing a credential does not turn them
into a safe real-provider harness. Use a dedicated isolated exercise, exact
amount bounds and the provider's test instruments. Live money and public
publication are outside the current execution authorization.
