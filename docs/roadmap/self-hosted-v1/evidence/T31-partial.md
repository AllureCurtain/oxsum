# T31 partial — locally executed external-adjacent checks (2026-11-06)

Issue: [#622](https://github.com/AllureCurtain/oxsum/issues/622). Scope: the
T31 items that can run without third-party authorization — the candidate-gate
dispatch, a local soak, a browser-pixel journey, and the Airwallex demo
receive/refund chain — executed on the integrated candidate. Environment:
`local-isolated`, macOS arm64, PostgreSQL 17 in Docker (`oxsum-postgres-1`),
a throwaway `oxsum_soak` database seeded with `oxsum seed` (`OXSUM_DEMO_SEED=true`
opt-in), `target/debug/oxsum` built from `8d241dd` (T26 merge; the candidate SHA
under test was `45b2f40`/`c5f5070`).

These runs close the *locally executable* half of T31's work list. The items
that by contract need outside authorization remain external-pending and are
enumerated in `RELEASE-CHECKLIST.md` §External actions — nothing below claims
them.

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

## 2. Soak (A28 measured, development hardware)

`scripts/soak.sh` was run to completion against a live `oxsum` on the seeded
demo database. One documented variant: the script's `authorization: Bearer`
probe was changed to `cookie: oxsum_session=` because a consumption API key is
structurally forbidden from `GET /api/v1/balance` (`Personal` capability —
`crates/core/src/customer_authority.rs`). The drift probe's intent — "the
balance read still answers 200 and holds do not grow unboundedly" — is served
identically by a session credential; a `SOAK_AUTH=cookie|bearer` flag would
upstream this without changing the measurement.

- Driver: `SOAK_SECONDS=300 BASE=http://127.0.0.1:3010` (session credential),
  four reads (`/balance`, `/usage`, `/org`, `/holds`) at ~20 ops/s.
- Result (`artifacts/soak-2026-11-06.log`): **300 s, 3 648 requests, 0
  failures, p50 5 ms, p99 206 ms, `soak: clean`** — balance answered 200 at
  the end and the holds list stayed bounded, so no leaked reservation
  accumulated.
- What this does *not* measure: the D09 1,000-customer / 10,000-key /
  100-stream shape on declared 4 vCPU hardware. That remains the stated
  limitation in `RELEASE-CHECKLIST.md`; this run is the per-deployment sanity
  figure on a laptop, not the normalized target.

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

This upgrades A25's evidence from contract-level to a recorded pixel journey:
the console-adjacent customer surfaces render and the browser verifier
completes a genuine check end to end.

## 4. Airwallex demo receive/refund chain (A31 item 1, as far as the environment allows)

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
- **The in-repo half of the rail was exercised end to end.** The deposit row
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
  the demo leg above stops at provider-side capture; live needs authorized
  real-money credentials.
- Real-provider certified examples — needs provider credentials + paid-call
  authorization.
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
what it is specified to do and refused; the workaround used the provider's
own API over the system proxy for the provider-side step and the in-repo
webhook for the receive side. No gate was weakened to obtain these results.
