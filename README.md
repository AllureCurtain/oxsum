# oxsum

A verifiable AI credit wallet, written in Rust. Users top up credits; before each streaming LLM call, oxsum freezes an upper-bound estimate, then settles against the actual token usage reported by the upstream provider and refunds whatever was not spent. Every transaction is recorded in an append-only Merkle log: users receive an inclusion proof for each bill and can verify in their own browser — without trusting the server — that their billing history has not been tampered with.

What makes it different from existing gateways: each balance check and freeze is atomic on a double-entry ledger, settlement is idempotent, and every write carries a proof. Concurrent requests cannot overdraw, retries cannot double-charge, and every entry can be independently verified. The ledger core is [doubleentry](https://github.com/hupe1980/doubleentry) (MIT/Apache-2.0), vendored into `crates/doubleentry`.

## Quick start

```bash
git clone https://github.com/AllureCurtain/oxsum.git && cd oxsum
cp .env.example .env            # adjust OXSUM_API_TOKEN as needed
docker compose up -d            # local PostgreSQL
cargo run -p oxsum-server
```

Then visit <http://localhost:3000/healthz>. The API reference is in [docs/api.md](docs/api.md).

## Status

Under active development. The multi-tenant wallet core (top-up, hold/settle, inclusion proofs, tamper detection) is tested and working. The OpenAI-compatible gateway and the web dashboard are next; see docs for the plan.

## Documentation

- Product specification: docs/product.md
- API conventions: docs/api.md
- User guide: docs/user-guide.md
- Development guide: docs/development.md
- Architecture: docs/architecture.md
- Technical decisions: docs/decisions.md

## License

MIT OR Apache-2.0. `crates/doubleentry` retains the original author's copyright notices; see the LICENSE files in that directory.
