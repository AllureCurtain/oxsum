# oxsum

A verifiable AI credit wallet, written in Rust. Users top up credits; before each streaming LLM call, oxsum freezes an upper-bound estimate, then settles against the actual token usage reported by the upstream provider and refunds whatever was not spent. Every transaction is recorded in an append-only Merkle log: users receive an inclusion proof for each bill and can verify in their own browser — without trusting the server — that their billing history has not been tampered with.

What makes it different from existing gateways: each balance check and freeze is atomic on a double-entry ledger, settlement is idempotent, and every write carries a proof. Concurrent requests cannot overdraw, retries cannot double-charge, and every entry can be independently verified. The ledger core is [doubleentry](https://github.com/hupe1980/doubleentry) (MIT/Apache-2.0), vendored into `crates/doubleentry`.

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

Both proof bundles are checkable on the `/verify` page against their content hashes — no trust in the server required. The demo prefers its own `oxsum_demo` database (created when the `DATABASE_URL` role may create databases) and otherwise uses `DATABASE_URL` directly. Recording this flow as a GIF is a manual step: record the terminal running `python3 demo/demo.py`.

## Docker

The release image is a multi-stage build of the whole workspace, including the Leptos dashboard (server-side rendering plus the WASM browser side). It runs the `oxsum` binary against an external PostgreSQL — no database is bundled:

```bash
docker build -t oxsum .
docker run --rm -p 3000:3000 \
  -e DATABASE_URL=postgresql://user:pass@dbhost/oxsum \
  -e OXSUM_SIGNUP=open \
  oxsum
```

`DATABASE_URL` is the only required variable; the rest are the `OXSUM_*` settings [docs/development.md](docs/development.md) documents (`.env` is never baked into the image — pass them with `-e`). The schema is migrated at startup, and the dashboard reads its files from the image's `/app/site`. Then visit <http://localhost:3000/healthz>.

## Status

Under active development. The multi-tenant wallet core (top-up, hold/settle, inclusion proofs, tamper detection), organization-scoped API key authentication, the OpenAI-compatible gateway (freeze before the call, settle against upstream usage), and the web dashboard (login, balance, keys, holds, transaction log, bill verification) are tested and working. The Leptos chat page is next; see TODO.md for the plan.

## Documentation

- Product specification: docs/product.md
- API conventions: docs/api.md
- User guide: docs/user-guide.md
- Development guide: docs/development.md
- Architecture: docs/architecture.md
- Technical decisions: docs/decisions.md

## License

MIT OR Apache-2.0. `crates/doubleentry` retains the original author's copyright notices; see the LICENSE files in that directory.
