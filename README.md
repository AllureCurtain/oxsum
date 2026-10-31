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

Every tenant head is cosigned by an independent witness — a second oxsum process with its own key and database — so `/api/v1/log/checkpoint` serves a third-party `cosignature/v1` alongside the operator's signature, and the bill page reports "Witnessed by N." The same check is a product, not only a page: the [`verify-oxsum-bill`](web/verify-sdk) package verifies a bill in any browser, and its `<verify-bill>` element drops the whole proof card onto a host page with one `<script>` tag. The wire protocol both speak is [docs/transparency-log-v1.md](docs/transparency-log-v1.md).

## Demo

`demo/demo.py` runs the whole flow end to end with the official OpenAI Python SDK pointed at oxsum — no real provider, no API key. The script starts a scripted mock upstream and an oxsum server itself, registers a demo user, tops up 100 credits, runs one billed chat turn, and prints the settlement entry's billing record and its inclusion proof:

```bash
pip install -r demo/requirements.txt
docker compose up -d            # PostgreSQL, if it is not already running
python3 demo/demo.py
```

Abridged transcript (ids and hashes differ per run):

```
== 1. Register and top up ==
   registered demo-…@example.com (organization 'demo'); the API key is shown once
   balance before top-up: 0 minor units
   topped up 100000000 minor units (100 credits)
   top-up entry: 7dcf20b9-…
   content hash (keep for verification): 45b88ddd…
   proof bundle: entry 7dcf20b9-…, tree head size 1, root 6c5b0d8b…

== 2. One billed chat turn through the OpenAI SDK ==
   balance before the turn: 100000000 minor units
   upstream said: "Holds are oxsum's way of saying 'reserved'."
   x-oxsum-request-id: fca1ed85-…

== 3. The turn's settlement entry and its proof ==
   hold key req-fca1ed85-…:hold -> settlement entry c055f9d6-…
   billing record: {"request": "fca1ed85-…", "channel": "demo-…", "model": "demo-chat",
     "priceVersion": 1, "kind": "usage", "inputTokens": 23, "outputTokens": 11,
     "inputPrice": 1000000, "outputPrice": 1000000, "charged": 34, "freeze": 118}
   proof bundle: tree head size 3, root b0e9ac48…
   balance after the turn: 99999966 minor units (charged 34)
```

Both proof bundles are checkable on the `/verify` page against their content hashes (the check runs on the server). The demo prefers its own `oxsum_demo` database (created when the `DATABASE_URL` role may create databases) and otherwise uses `DATABASE_URL` directly.

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

Under active development. The multi-tenant wallet core (top-up, hold/settle, inclusion proofs, tamper detection), organization-scoped API key authentication, the OpenAI- and Anthropic-compatible gateway (freeze before the call, settle against upstream usage), the React dashboard at `/app`, the server-rendered `/verify` page (which runs the verifier in the browser when the wasm bundle is present), monthly statements with dunning, and the multi-rail deposit abstraction are implemented and covered by the test suite. The P1–P8 roadmap, the Q1–Q4 verifiability phase and the next-phase gap plan are closed; the remaining work is tracked as issues.

## Documentation

- The pitch — what it is, who it is for, and why a verifiable bill matters: docs/product-page.md
- Product specification: docs/product.md
- API conventions: docs/api.md
- User guide: docs/user-guide.md
- Verifying a bill (for the customer): docs/verify-your-bill.md
- What each proof covers (the surface map): docs/proofs.md
- What each payment rail has actually been exercised to: docs/rails.md
- Development guide: docs/development.md
- Architecture: docs/architecture.md
- Technical decisions: docs/decisions.md

## License

MIT OR Apache-2.0. `crates/doubleentry` retains the original author's copyright notices; see the LICENSE files in that directory.
