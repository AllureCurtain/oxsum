# T31 partial — locally executed external-adjacent checks (2026-11-06)

Issue: [#622](https://github.com/AllureCurtain/oxsum/issues/622). Scope: the
T31 items that can run without third-party authorization — the candidate-gate
dispatch, a local soak, a browser-pixel journey, and a manually prepared Airwallex
webhook/ledger-reversal experiment — executed on the integrated candidate. Environment:
`local-isolated`, macOS arm64, PostgreSQL 17 in Docker (`oxsum-postgres-1`),
a throwaway `oxsum_soak` database seeded with `oxsum seed` (`OXSUM_DEMO_SEED=true`
opt-in), `target/debug/oxsum` built from `8d241dd` (T26 merge; the candidate SHA
under test was `45b2f40`/`c5f5070`).

This is historical evidence with restricted scope. It does not close all
locally executable acceptance: [closeout-capacity.md](closeout-capacity.md),
[closeout-browser.md](closeout-browser.md) and
[closeout-candidate.md](closeout-candidate.md) supersede its A28/A25/A30
claims. The payment experiment below manually seeds the provider reference
and locally signs its event; no provider capture or refund is demonstrated.

## 1. Candidate-gate dispatch (A30 executed half)

- `gh workflow run release-candidate.yml -f sha=45b2f40e5967249bdd48b19598e0b3b0c9371aee`
  → run `42229255357`. Jobs: `resolve`, `fmt`, `clippy`, `test (lib+doc)`,
  `frontend`, `candidate image (no push)` all succeeded; `wasm`, `verify
  consumer`, `developer examples` **failed** on the same root cause — the
  step that `curl`s `wasm-bindgen-cli` from
  `github.com/rustwasm/wasm-bindgen/releases/download/v0.2.129/…` got a 404,
  because the `rustwasm` org's move to `wasm-bindgen/wasm-bindgen` left
  `v0.2.129` without that release asset.
- **Fix landed**: PR [#677](https://github.com/AllureCurtain/oxsum/pull/677)
  (`fix/wasm-bindgen-install`, merged `c5f5070`) replaces the GitHub-release
  download with `cargo install wasm-bindgen-cli --version $version --locked`
  in all three steps — the lockfile pin is kept (no verifier-toolchain drift),
  only the source moves to crates.io, whose published versions are never
  removed. Decision recorded in `docs/decisions.md`.
- Re-dispatch on `c5f5070` tracks the fix's first green run; the manifest job
  is `skipped` when a gate job fails, which is the designed short-circuit.

## 2. Historical read-only soak (superseded for A28)

The 2026-11-06 log records 3,648 successful reads of `/balance`, `/usage`,
`/org` and `/holds` over 300 seconds on a seeded local deployment. The old
script labeled the minimum and maximum as p50 and p99; those percentile
claims are invalid. An HTTP 200 balance read and bounded hold list do not
prove accounting conservation, and the workload created no streaming charges.
Do not use this log as D09 capacity or zero-drift acceptance.

[The closeout measurement](closeout-capacity.md) replaces it with real HTTP
streaming calls at the normalized account/key/concurrency shape and exact
per-wallet, operation, usage and hold reconciliation.

## 3. Browser journey (A25 pixel evidence)

Driven through `ego-browser` against the same live server (`/app` served from
the binary's embedded `web/dist`, built fresh with `npm run build`). Screenshots
in `artifacts/`:

| File | Surface | Result |
| --- | --- | --- |
| `01-overview.png` | login → `/app` overview, `Acme Demo` org | rail + live billing status render |
| `02-billing.png` | `/app/billing` plan-and-credit summary | correct empty state (no plan → no bills) |
| `03-verify.png` | `/verify` bill-verification page | form + "runs in this browser" copy served |
| `04-verify-example.png` | "Try an example bill" fills the public fixture | proof bundle + content hash populated |
| `05-verify-result.png` | click Verify | **"The bill verifies."** — content hash recomputes, inclusion proof links to the tree head, charge recomputes from usage+rates, price bound to the signed checkpoint |

These five screenshots prove only those rendered states and verification of
the public example fixture. They do not prove billed customer evidence,
checkout, refunds, staff permissions, mobile or keyboard workflows. The
broader scoped journey is in [closeout-browser.md](closeout-browser.md).

## 4. Airwallex demo intent and simulated receive/ledger-reversal experiment

`.env` carries a real Airwallex *demo* credential set
(`api-demo.airwallex.com` / `checkout-demo.airwallex.com`, `x-client-id` +
`x-api-key`). What ran and what blocked it:

- **Outbound call refused by design.** `POST /api/v1/topups/airwallex` reached
  `merchant_checkout::create`, which uses `ProviderHttp` — deliberately
  `.no_proxy()` with a checked DNS answer set (`crates/integrations/src/
  provider_http.rs`). The local FlClash/clash-verge fake-IP DNS resolves
  `*.airwallex.com` into the `198.18.0.0/15` benchmark reservation, which
  `ProviderNetwork::Public` rejects — so the intent-create leg 503'd
  *correctly*: the SSRF guard refused a destination that is not a public
  address. This is T06's feature working, not a defect. (`merchant_versions`
  are trigger-immutable, so no DB workaround was attempted or should be.)
- **Provider side is real.** The same credentials, called with `curl` (which
  honors the system proxy path), authenticated (`authentication/login` →
  scoped token) and created a genuine demo PaymentIntent
  `int_sgpvzc9hjhnwshepheb` ($500.00 USD, `REQUIRES_PAYMENT_METHOD`). The
  demo account does not have native-API `confirm` enabled — a real charge
  would need the hosted `checkout-demo` page, which the fake-IP DNS also
  diverts.
- **The manually prepared receive/ledger-reversal path ran.** The deposit row
  (`pending`) from the failed create was completed by inserting the real
  intent id into `meta.airwallexIntentId` + `merchant_payment_refs` —
  simulating exactly the state a successful `create` would have left — then:

  | Step | Result |
  | --- | --- |
  | `POST /api/v1/webhooks/airwallex` `payment_intent.succeeded`, signed `hex(HMAC-SHA256(ts+body))` under `OXSUM_AIRWALLEX_WEBHOOK_SECRET` | **200** — `deposits.status=credited`, wallet `+50 000 000` minor, ledger entry `2bcb5ab2` |
  | Same webhook replayed | **200**, idempotent — deposit unchanged (confirm-guard + unique `airwallex:<intent>` ledger key) |
  | `POST /api/v1/admin/deposits/{id}/reverse` (admin token) | **200** — `status=reversed`, reversal entry `27091187`, wallet `-50 000 000` (net zero conserved, entries immutable) |

- **Coverage statement.** This exercises verification → fulfillment → credit
  → operator reverse → ledger conservation on a live server against the real
  provider's credential/account shape. What it does not cover: Airwallex's own
  hosted-checkout capture and a real refund call *to* the provider — both need
  the public internet path the local proxy DNS does not offer, and a merchant
  with live-refund permission. Those stay on the external list.

## 5. What remains external-pending (unchanged)

- Merchant sandbox **plus low-value live** payment/refund certification —
  the demo leg above never performs provider-side capture; live needs authorized
  real-money credentials.
- Remaining real-provider certified examples — one later DeepSeek
  nonstreaming call is recorded in [T31-followup.md](T31-followup.md); other
  protocols/scenarios still need credentials and paid-call authorization.
- Two independent pilot operators — there is no authorized outside operator;
  the contract forbids simulating one. `ONBOARDING-DRYRUN.md` is the document
  a pilot would follow.
- Publication/deployment — a separate recorded G5 decision per chosen SHA.
- npm publication of `verify-oxsum-bill` — no npm account exists yet; the
  packed-tarball path remains the verified distribution.

## Standing note on environment

All four runs used the developer machine's own network, which carries a
fake-IP DNS layer (FlClash/clash-verge). Where that layer rewrote a provider
hostname into a non-public reservation, the product's destination policy did
what it is specified to do and refused; the provider API over the system
proxy created an intent only. The later
receive path was manually seeded and locally signed; it cannot be substituted
for PSP certification. The destination guard itself was not weakened.
