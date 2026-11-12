# oxsum

[![CI](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml/badge.svg)](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[中文](README.zh-CN.md)

**Self-hosted, verifiable billing for AI APIs: reserve-then-settle credits, a double-entry ledger, and customer-checkable Merkle proofs. Rust + PostgreSQL, OpenAI-compatible gateway.**

Oxsum is usage-based billing for an AI-service business that runs the software itself. One Business owns its suppliers, customer sale terms, and collection. Personal and team customer accounts own their balances, API keys, and bills. Operators use the built-in portal at `/app`. Application and Agent SaaS developers can embed the gateway and scoped metering APIs in their own product. The Rust server and the React UI ship in one binary; PostgreSQL 17 holds the data. This deployment model takes no platform fee.

Before an admitted model call, oxsum reserves the applicable upper bound, then settles the recorded usage against the pinned terms and releases unused credit. Purchased funds, promotional credit, plan quota, and approved credit lines keep their separate accounting rules. Each settlement is appended to a Merkle transparency log. A customer who retains the entry hash — and, for the stronger checks, an earlier log head and a signing key they chose to trust — can check that bill on `/verify` or with the [`verify-oxsum-bill`](web/verify-sdk) package. The check has a [documented scope](docs/proofs.md). It does not establish that an upstream provider's token count was truthful.

**Status.** The workspace version is 0.1.0, and there is no GitHub Release yet. Operator onboarding still has gaps tracked in [#725](https://github.com/AllureCurtain/oxsum/issues/725). `verify-oxsum-bill` is not on npm (`npm view verify-oxsum-bill` returned 404 on 2026-11-12); build it from [`web/verify-sdk`](web/verify-sdk). [oxsum.785777.xyz](https://oxsum.785777.xyz) is not a supported trial: on 2026-11-12 it redirected to `/app` and returned HTML, and `GET /api/v1/version` did not return a version document. That probe did not identify the commit, balances, credentials, or provider access. Pictures in this file are the 2026-10-24 dashboard, not the current UI. A green pull-request CI run is the fast tier, not a full release-candidate pass ([candidate gates](docs/self-hosted-release.md)).

## Run it locally

Rust stable at or above the workspace MSRV 1.98, and Docker with Compose. `.env.example` already points `DATABASE_URL` at the database `compose.yaml` creates. The server loads `.env` when the file is present.

### The process is up

```bash
git clone https://github.com/AllureCurtain/oxsum.git
cd oxsum
cp .env.example .env
docker compose up -d
cargo run -p oxsum-server
```

The first `cargo run` compiles the workspace. Then check [health](http://127.0.0.1:3000/healthz) and [readiness](http://127.0.0.1:3000/readyz). Readiness answers only when PostgreSQL is reachable and migrations have applied. Stopping here leaves a database with no Business owner.

### Become the owner

Uncomment `OXSUM_ADMIN_TOKEN` in `.env`, set it to the output of `openssl rand -hex 24` (at least 16 characters), and restart the server. Mint a one-time setup invitation:

```bash
curl -s -X POST http://127.0.0.1:3000/api/v1/admin/business/setup-tokens \
  -H "authorization: Bearer $OXSUM_ADMIN_TOKEN" \
  -H "content-type: application/json" \
  -d '{"idempotencyKey":"setup-1","ownerEmail":"owner@example.com"}'
```

The response shows `token` once. Open [http://127.0.0.1:3000/app/setup](http://127.0.0.1:3000/app/setup), accept with that email and a password of at least 12 characters, then sign in at [http://127.0.0.1:3000/app](http://127.0.0.1:3000/app) and choose Business. Keep the deployment token out of the customer browser. An image deployment uses [`deploy/`](deploy/docker-compose.core.yml) and the [installation runbook](docs/self-hosted-install.md) instead of `cargo run`. The runbook's setup call is the same endpoint.

`OXSUM_SECRET_KEY` (`openssl rand -base64 32`) is required before the database stores a provider or payment credential. `OXSUM_HEAD_SIGNING_KEY` is optional; without it the signed-log routes answer 503 and the wallet still serves.

Customer registration stays invite-only until the owner opens it. `OXSUM_SIGNUP` does not override that saved policy. Registration grants no purchased funds and no staff authority. A running process, a configured channel, or a merchant row is not evidence of a successful model call or payment. Use the documented setup APIs where this revision has no browser editor ([#725](https://github.com/AllureCurtain/oxsum/issues/725)).

`docker build -t oxsum .` builds a local image containing the binary, the React app, and the verifier assets. The image still needs an external PostgreSQL and deployment configuration. `.env` is excluded from the image. The supported install path pins an immutable image reference; there is no floating published tag to pull.

## What a proof establishes

A retained content hash lets `verify_bundle` check that the entry bytes still hash to that value and that an inclusion proof reaches the supplied log head. For settlement descriptions the verifier knows, a separate charge check recomputes the recorded amount from the usage and rates inside the entry. Signed price and sale bindings, a consistency proof against a head the reader archived earlier, a usage-report or evidence-bundle check, and witness cosignatures each answer their own question. `/verify` runs the check in the browser when the WASM assets are installed, and falls back to a server-rendered form when they are not.

The same check does not establish that the provider actually returned those tokens, that a key supplied beside the bundle is an independently trusted operator identity, that a witness is organizationally independent, or that an unanchored report lists every settlement. A second oxsum process can cosign heads when `OXSUM_WITNESS_URLS` is set; that demonstrates the protocol. Comparing heads the reader kept can expose a fork. The full map is [docs/proofs.md](docs/proofs.md). The customer walkthrough is [docs/verify-your-bill.md](docs/verify-your-bill.md). The wire format is [docs/transparency-log-v1.md](docs/transparency-log-v1.md).

`verify-oxsum-bill` (not yet on npm) exposes the same checks to Node and the browser, including `<verify-bill>`, `<verify-statement>`, and `<verify-usage-report>`. Build it from [`web/verify-sdk`](web/verify-sdk), or run [`examples/verify-consumer`](examples/verify-consumer) against a locally packed tarball:

```bash
bash scripts/check-verify-consumer.sh
```

## Features

- **LLM usage-based billing.** Reserve an upper bound, settle recorded usage, and release unused credit. Unknown usage, estimation, and capped settlement stay explicit outcomes. Amounts are integers in minor units (1 credit = 1,000,000).
- **OpenAI-compatible gateway.** `POST /v1/chat/completions` and `POST /v1/responses`, plus Anthropic Messages, Gemini, embeddings, and rerank on the surfaces documented in [docs/integrations.md](docs/integrations.md). Point an official SDK at this deployment's `/v1`.
- **Double-entry ledger and Merkle log.** The ledger engine is [doubleentry](https://github.com/hupe1980/doubleentry) (MIT OR Apache-2.0), vendored in `crates/doubleentry`. oxsum adds the wallet, sale terms, and proof bundles on top.
- **Business portal and customer accounts.** The React app is served at `/app`, in English and 中文, and release builds embed it in the binary. Legacy `/dashboard`, auth, and `/admin` paths redirect there. `/verify` stays server-rendered. With the WASM assets installed, the browser runs the verifier that shipped with that server; otherwise the form checks on the server.
- **Collection rails.** Stripe, Airwallex, and Hyperswitch checkouts, redemption codes, and staff-recorded receipts. `POST /api/v1/topups` refuses customer self-credit. A checkout response is not payment success; signed provider events and purpose-specific fulfillment establish what an order funded. The [rail matrix](docs/rails.md) separates local fixtures from provider and live-money evidence.
- **Embeddable metering.** Owner-issued credentials with explicit scopes can reserve and settle events that never passed through the gateway. The embedded application lifecycle is part of [#725](https://github.com/AllureCurtain/oxsum/issues/725); a scoped API existing in this tree is not a claim that every planned endpoint is already accepted.

## Architecture

```
LLM clients and other API callers
        │
        ▼
one Rust process (axum)
  /v1      gateway relay
  /api/v1  billing, business, and customer API
  /app     React portal (embedded in a release build)
  /verify  server-rendered page; WASM check when the assets are installed
        │
        ▼
oxsum-core  ──relay──►  upstream model provider
        │
        ▼
doubleentry     double-entry bookkeeping and the Merkle log
        │
        ▼
PostgreSQL 17   one ledger schema per organization, plus shared oxsum tables
```

Domain logic lives in `crates/core`. The gateway does protocol and orchestration. `crates/verify` is the pure checker, shared with the browser and kept wasm32-clean. More detail is in [docs/architecture.md](docs/architecture.md).

## Try the isolated demo

`demo/demo.py` is the maintained local flow. It creates a unique disposable PostgreSQL database and fails if it cannot. It does not read a checkout `.env`, does not store application data in the provisioning database, and does not contact a paid provider or payment service. The run covers deployment-authorized owner consent, an invited customer, a synthetic manual receipt, explicit customer sale terms, one streamed stub response through the official OpenAI Python SDK, and an offline check of the packed verifier after the server has stopped and its database has been dropped.

Prerequisites: Python 3.10+, Rust, Node 20.16+, npm, PostgreSQL 17 with a `CREATEDB` role, the `wasm32-unknown-unknown` target, and the matching `wasm-bindgen` CLI ([development requirements](docs/development.md#requirements)). The command builds the server and the verifier. Cargo's configured target directory is respected.

```bash
python3 -m venv /tmp/oxsum-demo-venv
/tmp/oxsum-demo-venv/bin/pip install -r demo/requirements.txt
docker compose up -d
/tmp/oxsum-demo-venv/bin/python demo/demo.py
```

The provisioning connection defaults to `postgres://postgres:postgres@127.0.0.1:5432/postgres`. Set `DATABASE_URL` in the process environment for another local cluster; it is used only to create and drop this run's database. The application receives a new database URL and generated keys, with inherited `OXSUM_*` configuration removed.

The deterministic turn reports 23 input and 11 output tokens at one minor unit per token. The charge is exactly 34 minor units, the available balance is 99,999,966, and no reservation remains. The retained hash, the pre-turn head and key, the signed supply and sale bindings, the consistency proof, and the new head are written to the evidence directory (`--output` chooses a new directory). The key comes from this local fixture. It is not an independently attested operator identity.

Bill, signature, and history checks must pass. Altered content, hash, key, and archived-head checks must fail. This is an immediate entry on a local stub. It is not a closed-period report, an independent witness, a real payment, or a real-provider certification.

Issue [#728](https://github.com/AllureCurtain/oxsum/issues/728) replaced an older script that assumed a different registration and top-up model. The script in this tree is that replacement. Use this file's own instructions; do not treat an older transcript as acceptance evidence for a later commit.

## Earlier dashboard pictures

These captures are from 2026-10-24 ([#362](https://github.com/AllureCurtain/oxsum/issues/362)): a sequence of real pages, top-up through Requests, Bills, and `/verify`. The sidebar is the earlier flat navigation. The current portal groups Main, Configure, and Account, and adds pages such as Orders, Plans, Billing, Evidence, and Business. The opening frame of `demo.gif` and the stills below were checked against that older chrome. Later frames of the animation were not replayed one by one. They illustrate that revision. They are not the current UI, and [oxsum.785777.xyz](https://oxsum.785777.xyz) is not a stand-in for them or for a release candidate.

![Earlier dashboard: top up, requests, bills, and verify](docs/assets/demo.gif)

| Funding | Evidence |
| --- | --- |
| ![Earlier Hyperswitch top-up screen](docs/assets/topup-hyperswitch.png) | ![Earlier Bills screen](docs/assets/bills.png) |

## Documentation

Start at the [documentation index](docs/README.md).

- [Product overview](docs/product-page.md), [behavior](docs/product.md), and [user guide](docs/user-guide.md)
- [API](docs/api.md), [integrations](docs/integrations.md), and [unsupported operations](docs/unsupported.md)
- [Verify a bill](docs/verify-your-bill.md), [proof scope](docs/proofs.md), and [payment evidence](docs/rails.md)
- [Install](docs/self-hosted-install.md), [upgrade and restore](docs/self-hosted-upgrade.md), and [candidate and release gates](docs/self-hosted-release.md)
- [Development](docs/development.md), [architecture](docs/architecture.md), and [decisions](docs/decisions.md)
- [Security](SECURITY.md) and [support](SUPPORT.md)

## FAQ

**Is oxsum a hosted wallet?** No. You run the binary and PostgreSQL. This deployment model takes no platform fee.

**What does reserve-then-settle mean?** An admitted call reserves an upper bound in integer minor units (1 credit = 1,000,000). Settlement records the usage the deployment wrote down, charges that amount, and releases the rest.

**What does a Merkle proof establish?** That the retained entry bytes still hash to the expected value and sit in the supplied log head, plus the charge, price, and sale checks [docs/proofs.md](docs/proofs.md) names. It does not establish that the provider's token count was truthful.

**Does it replace LiteLLM or new-api?** Project decisions describe an overlap with those self-hosted proxies. The recorded aim is ledger-backed settlement a customer can check, not a claim that oxsum replaces them.

**Can I `npm install verify-oxsum-bill`?** Not as of 2026-11-12 (`npm view` returned 404). Build it from `web/verify-sdk`, or run `bash scripts/check-verify-consumer.sh`.

**Is there a release or a public demo to click?** There is no GitHub Release yet. The pictures below are the 2026-10-24 dashboard. oxsum.785777.xyz answered on 2026-11-12 and is not a supported trial.

**Why is some UI still not React?** The dashboard is the React app at `/app`. `/verify` and `/bill/{token}` stay server-rendered so a bill can be read without the dashboard bundle and without JavaScript. Details are in [docs/README.md](docs/README.md).

**Where do I start as an operator?** Bring the process up, then become the owner, both in [Run it locally](#run-it-locally). Portal gaps are [#725](https://github.com/AllureCurtain/oxsum/issues/725).

## Where it sits

Oxsum is the billing system an operator runs next to their own model providers. Project decisions describe the gateway as overlapping self-hosted proxies such as LiteLLM and new-api, and describe the aim as ledger-backed settlement a customer can check within [docs/proofs.md](docs/proofs.md), rather than a best-effort spend log. Those notes do not claim oxsum replaces those proxies. Provider sandbox certification, reference capacity, independent pilots, and publication remain separate gates from a local fixture ([rail matrix](docs/rails.md), [release checklist](docs/roadmap/self-hosted-v1/RELEASE-CHECKLIST.md)).

## License

MIT OR Apache-2.0. The texts are [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). The workspace manifest records the same SPDX expression. `crates/doubleentry` keeps the original author's copyright notices; see the LICENSE files in that directory.
