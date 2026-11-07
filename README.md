# oxsum

[![CI](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml/badge.svg)](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml)
[![Live demo](https://img.shields.io/badge/demo-oxsum.785777.xyz-blue)](https://oxsum.785777.xyz)

A verifiable AI credit wallet, written in Rust. Users top up credits; before each streaming LLM call, oxsum freezes an upper-bound estimate, then settles against the actual token usage reported by the upstream provider and refunds whatever was not spent. Every transaction is recorded in an append-only Merkle log: users receive an inclusion proof for each bill and can verify in their own browser — without trusting the server — that their billing history has not been tampered with.

What makes it different from existing gateways: each balance check and freeze is atomic on a double-entry ledger, settlement is idempotent, and every write carries a proof. Concurrent requests cannot overdraw, retries cannot double-charge, and every entry can be independently verified. The ledger core is [doubleentry](https://github.com/hupe1980/doubleentry) (MIT/Apache-2.0), vendored into `crates/doubleentry`.

![The flagship flow — top up, billed turns on the Requests page, the ledger on Bills, and a verification on /verify](docs/assets/demo.gif)

## Quick start

```bash
git clone https://github.com/AllureCurtain/oxsum.git && cd oxsum
cp .env.example .env            # OXSUM_SIGNUP=open if you want to register through the API
docker compose up -d            # local PostgreSQL
cargo run -p oxsum-server
```

Then visit <http://localhost:3000/healthz>. The database schema is created at startup. To try the API, set `OXSUM_SIGNUP=open` and register:

```bash
curl -X POST localhost:3000/api/v1/auth/register \
  -H 'content-type: application/json' \
  -d '{"email":"you@example.com","password":"correct horse battery"}'
# the response's data.apiKey.secret is your Bearer credential, shown once
```

The API reference is in [docs/api.md](docs/api.md).

## Live demo

**[oxsum.785777.xyz](https://oxsum.785777.xyz)** is a running deployment against a real upstream (DeepSeek). Register an account — new signups get 5 demo credits — or log in as `admin@oxsum.local` / `admin-password` to see the seeded world: 800 credits of balance, 100+ settled turns in the ledger, and a signed head you can verify on `/verify`.

Every tenant head is cosigned by an independent witness — a second oxsum process with its own key and database — so `/api/v1/log/checkpoint` serves a third-party `cosignature/v1` alongside the operator's signature, and the bill page reports "Witnessed by N." The same check is a product, not only a page: the [`verify-oxsum-bill`](web/verify-sdk) package verifies a bill in any browser, and its `<verify-bill>`, `<verify-statement>` and `<verify-usage-report>` elements drop the whole proof card onto a host page with one `<script>` tag. The package is **not yet published to npm** (`npm view verify-oxsum-bill` answers 404); build it from `web/verify-sdk`, or run the repository's [`examples/verify-consumer`](examples/verify-consumer) against a locally packed tarball — `bash scripts/check-verify-consumer.sh` is the reproducible path. The wire protocol both speak is [docs/transparency-log-v1.md](docs/transparency-log-v1.md).

## Demo

`demo/demo.py` runs an isolated local flow through the official OpenAI Python SDK: deployment-authorized owner consent, an invited customer, a synthetic manual receipt, explicit customer sale terms, one streamed stub response, and a verified settlement. It uses a unique disposable PostgreSQL database and **fails if it cannot create one**. It never uses the provisioning database for application data, reads no `.env`, and contacts no paid provider or PSP.

Prerequisites: Python 3.10+, Rust, Node 20.16+, npm, PostgreSQL 17 with a `CREATEDB` role, the `wasm32-unknown-unknown` Rust target and the matching `wasm-bindgen` CLI (see [development requirements](docs/development.md#requirements)). The command builds the server and verifier; Cargo's configured shared target directory is respected.

```bash
python3 -m venv /tmp/oxsum-demo-venv
/tmp/oxsum-demo-venv/bin/pip install -r demo/requirements.txt
docker compose up -d            # local PostgreSQL, if needed
/tmp/oxsum-demo-venv/bin/python demo/demo.py
```

The provisioning connection defaults to `postgres://postgres:postgres@127.0.0.1:5432/postgres`. Set `DATABASE_URL` in the process for another local cluster; it is used only to create/drop this run's random database. The application receives only its new database URL and generated keys, with all inherited `OXSUM_*` configuration removed. No database privilege failure permits a fallback or a channel-table reset.

The deterministic turn reports 23 input and 11 output tokens at one minor unit per token: the charge is exactly **34**, the available balance is **99,999,966**, and no reservation remains. The retained hash, pre-turn head/key, signed supply/sale bindings, consistency proof and new head go into the printed evidence directory (`--output <new-directory>` chooses it). The key is retained from this local fixture; it is not an independently attested operator identity.

After stopping the server and dropping its own database, the script installs the locally packed `verify-oxsum-bill` tarball in a clean directory outside the repository and runs the maintained [offline consumer](examples/verify-consumer). Bill, signature and history checks must pass; altered content/hash/key/archived-head checks must fail. A required failure exits nonzero. The tarball, checksums, sanitized evidence and verification log remain available. This proves an immediate entry, not a closed-period report, independent witness, real payment or real-provider certification.

The dashboard surfaces the same flow: a top-up on **Top up** (manual, Stripe, Airwallex, Hyperswitch or a redeem code — the Hyperswitch tab mounts the switch's Unified Checkout in place), billed turns on **Requests** and aggregated on **Usage**, the ledger entries with their proof bundles on **Bills**, and the verification itself on **`/verify`**:

| | |
| --- | --- |
| ![Top up through the Hyperswitch checkout](docs/assets/topup-hyperswitch.png) | ![Bills — the ledger entries with verify/proof](docs/assets/bills.png) |
| ![Usage — billed turns aggregated per key](docs/assets/usage.png) | ![Verifying a bill on /verify](docs/assets/verify.png) |

## Docker

The release image is a multi-stage build of the whole workspace: the `web/` React frontend is built and embedded into the binary (served at `/app`), and the one server-rendered page (`/verify`) rides along in the same crate set. It runs the `oxsum` binary against an external PostgreSQL — no database is bundled:

```bash
docker build -t oxsum .
docker run --rm -p 3000:3000 \
  -e DATABASE_URL=postgresql://user:pass@dbhost/oxsum \
  -e OXSUM_SIGNUP=open \
  oxsum
```

`DATABASE_URL` is the only required variable; the rest are the `OXSUM_*` settings [docs/development.md](docs/development.md) documents (`.env` is never baked into the image — pass them with `-e`). The schema is migrated at startup, and the dashboard's stylesheet is embedded in the binary. Then visit <http://localhost:3000/healthz>.

## Status

Under active development; the self-hosted release program (T00–T30) is implemented and integrated, with scoped acceptance evidence and declared capacity limits — see [docs/roadmap/self-hosted-v1/ACCEPTANCE-RESULTS.md](docs/roadmap/self-hosted-v1/ACCEPTANCE-RESULTS.md). On top of the core (top-up, hold/settle, inclusion proofs, tamper detection, the OpenAI/Anthropic-compatible gateway, `/app` dashboard, browser `/verify`), the release adds: one business per deployment with many customer accounts and a staff console at `/app/business`, a customer portal with plans/billing/self-service, versioned sale books and paid plans with journals + statements + receivables, payment orders across Stripe/Hyperswitch/Airwallex/x402 rails with refunds and disputes, promotional lots, financial-facts reporting with reconciliation, v2 evidence bundles customers can verify offline, runnable developer examples, and the compose-based self-hosted install/upgrade/restore path in [docs/self-hosted-install.md](docs/self-hosted-install.md). Release limitations include D09 throughput/overhead below target and limited browser workflow coverage; merchant/provider certification, independent pilots and publication authorization also remain pending — the exact list lives in [docs/roadmap/self-hosted-v1/RELEASE-CHECKLIST.md](docs/roadmap/self-hosted-v1/RELEASE-CHECKLIST.md). No production-ready label is claimed yet.

## Documentation

- The pitch — what it is, who it is for, and why a verifiable bill matters: docs/product-page.md
- Product specification: docs/product.md
- API conventions: docs/api.md
- User guide: docs/user-guide.md
- Verifying a bill (for the customer): docs/verify-your-bill.md
- What each proof covers (the surface map): docs/proofs.md
- What each payment rail has actually been exercised to: docs/rails.md
- Supported integrations and what the server refuses: docs/integrations.md and docs/unsupported.md
- Self-hosted install/upgrade/restore: docs/self-hosted-install.md and docs/self-hosted-upgrade.md
- Release candidate → promotion runbook: docs/self-hosted-release.md
- Development guide: docs/development.md
- Architecture: docs/architecture.md
- Technical decisions: docs/decisions.md
- Security reports: SECURITY.md — operator support: SUPPORT.md

## License

MIT OR Apache-2.0. `crates/doubleentry` retains the original author's copyright notices; see the LICENSE files in that directory.
