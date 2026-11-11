# Real Airwallex cloud-demo evidence

Observed 2026-11-11 on macOS arm64 against the real `api-demo.airwallex.com`
cloud-demo (sandbox) environment. Issue #741 (N15). This is a **partial**
certification: the order→intent leg ran end-to-end against the live provider,
but capture, webhook fulfillment and refund are **blocked at the provider side**
and are honestly reported as incomplete, not synthesized.

## Credential and account

The `OXSUM_AIRWALLEX_*` set in the repository `.env` is a valid cloud-demo
credential. `POST /api/v1/authentication/login` returns a live bearer token
(`tier:"S"`), and the bound account is `dfdf8e44-…` (masked). The account is
**hosted-page-only**: `POST …/payment_intents/{id}/confirm` answers
"This account is not enabled for native API access", so a raw API confirm is
refused by design — the card must be entered on Airwallex's hosted page.

No credential, token, client_secret or card number is recorded here.

## What ran (verified real)

Two deposits were opened through the server's airwallex rail
(`POST /api/v1/topups/airwallex`) and each produced a real PaymentIntent at
`api-demo.airwallex.com`, bound to the deposit id as `merchant_order_id`:

| Deposit | Airwallex intent | Provider status | Amount |
| --- | --- | --- | --- |
| `17500b67-93cb-4d94-9c52-2bcfd2c4d4ab` | `int_sgpvfdzq7ho1vp8fsls` | `REQUIRES_PAYMENT_METHOD` (live) | 1 USD |
| `68a51209-50b4-4e06-bd7c-eda5ab8bd667` | `int_sgpvdzck4ho1vrm6mzz` | `CANCELLED` (live, via `…/cancel`) | 1 USD |

Both states were read back from the provider (`GET …/payment_intents/{id}`), not
assumed. Each deposit is `pending` in `oxsum.deposits` with
`meta.airwallexIntentId` and `meta.airwallexCheckoutUrl` populated — the
server's intent→deposit binding and checkout-URL generation are real and correct.
The wallet balance stayed `0` because no capture ever completed. The second
intent's provider-side `CANCELLED` confirms a real mutating call reached the
provider (not just a read path).

So the **order → real-provider-intent** leg is certified: the server logs in to
Airwallex, creates a bound PaymentIntent, persists the deposit and returns the
hosted-page URL + client_secret.

## What is blocked (provider-side, not synthesized)

### Capture — hosted checkout page renders blank

`https://checkout-demo.airwallex.com/?intent_id=int_…` loads but its SPA root
stays empty for the full observed window (45 s+). The page embeds anti-bot
iframes (zh-CN reCAPTCHA and Tencent captcha), issues no further network
fetches and logs no console errors. The standalone `?intent_id=` form and the
drop-in element page both render blank the same way. This is an
environment/captcha-gated render of Airwallex's own hosted page — the sandbox
card (`4035 5010 0000 0008`) was never reachable, so no capture was attempted
and none is claimed. (`pay.sandbox.airwallex.com` payment-link pages do render,
so this is specific to the `checkout-demo` intent page, not a general
browser failure.)

### Webhook fulfillment — no endpoint reachable, zero events

`oxsum.merchant_event_inbox` holds **0** rows, including for the
provider-confirmed `CANCELLED` intent after ~3 minutes — so no
`payment_intent.*` webhook was ever delivered to the public URL. The demo
account's webhook config endpoints (`/api/v1/webhooks*`) answer
"Insufficient permissions", meaning this token cannot register or inspect a
webhook endpoint. Any endpoint previously created in the Airwallex dashboard
points at a stale URL, not this run's ephemeral tunnel. Because no real signed
`payment_intent.succeeded` arrived, the `pending → confirmed → credited` walk
**did not run** and is not claimed. The unit-level webhook path is covered by
`crates/server/tests/server/airwallex.rs` (signature verify, atomic credit,
replay, cancel), but no provider-delivered event reached this deployment.

### Refund — not reached, and provider refund is not wired for this rail

No captured payment existed to refund. Separately, the deposit-reversal path
(`deposit_refund::recover` / `admin::ops::reverse_deposit`) is
**hyperswitch-only** today — for an airwallex deposit it would post the ledger
reversal but never call `api-demo …/refunds`. That is a product gap independent
of this environment; the refund leg could not be exercised regardless.

## Environment boundaries (mediated, not production)

- **Public URL**: FlClash's TUN + fake-ip (`198.18.0.0/16`) DNS hijack blocked
  `cloudflared` (QUIC blocked; HTTP/2 TLS handshake EOF to the argotunnel edge
  on both fake and real DoH-resolved IPs). The reachable public URL came from an
  SSH `localhost.run` reverse tunnel (`*.lhr.life`) — a temporary rotating
  domain, so this is a mediated public URL, not a stable one.
- **Merchant api_url**: the merchant path could not reach `api-demo` directly.
  First, fake-ip resolved it to `198.18.x`, which the `public_v4` destination
  policy correctly rejects; then, pinned to the real IP `34.120.179.146`,
  rustls hit the TLS-1.3 `CloseNotify` decryption failure from issue #374 —
  **on the shared `ProviderHttp` client, which has no TLS cap**. The dedicated
  `Airwallex` rail client carries `max_tls_version(TLS_1_2)` (#374) but
  `ProviderHttp` does not. To make the intent call complete, the merchant
  version's `api_url` was pointed at a local relay `http://127.0.0.1:8787` →
  `api-demo` with destination policy `internal`/`allowedIps:["127.0.0.1"]`.
  This proves the flow but is a workaround: **the #374 TLS-1.2 fix is not
  applied to `ProviderHttp`, so a merchant-scoped Airwallex call (intent
  create, refund) cannot reach `api-demo` directly on this build.**
- **Scratch DB setup**: the run used a fresh `oxsum_n15` database; registration
  was opened with `UPDATE oxsum.businesses SET registration_mode='open',
  initialized_at=now()` and the merchant bootstrapped from
  `OXSUM_AIRWALLEX_*` env (source `environment`). The served build reports
  `gitSha 0a5a7bae…`, `version 0.1.0`, `schema 0131_invoice_corrections`
  (running-source build, not the frozen `cc16bb8f` image — the amd64 image could
  not run the relay-on-localhost topology under emulation).
- **Scale**: one low-value (1 USD) intent, demo sandbox, not production, no real
  money moved.

## Verdict

Real, not fixture: the server genuinely authenticated to Airwallex and created
two provider-bound PaymentIntent + deposit pairs. Blocked, honestly: the hosted
checkout page will not render in this environment, no webhook endpoint delivered
a real signed event, and the provider refund path is unwired for this rail — so
the `pending → confirmed → credited` walk and the refund are **not certified
here**. The `#374` TLS-1.2 gap on `ProviderHttp` is a concrete defect surfaced
by this run.
