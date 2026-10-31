# oxsum-verify

Pure, I/O-free bill verification for [oxsum](https://github.com/AllureCurtain/oxsum), the verifiable AI credit wallet. This crate is the *proof math* the customer runs — it recomputes a settlement's content hash from its canonical entry bytes, links it to the ledger's Merkle head, and checks the signed price binding — all offline, with no server round-trip.

It is also the crate the `/verify` page compiles to WebAssembly: the same function body runs on the server and in the user's browser, so "the server says it verified" is never the only evidence.

## What it verifies

Given a `ProofBundle` — the entry source, an inclusion proof, the tree head as of the proof — plus the 64-hex content hash the user recorded with the bill:

- **`verify_bundle`** recomputes the hash from the entry source and links it to the head. A single flipped byte fails it.
- **`verify_bill`** composes the full verdict: inclusion, the charge recomputation (`verify_charge`), and the signed price binding (`verify_price_head`), reduced to the sentence a page prints.
- **`verify_signed_head`** / **`verify_consistency`** / **`verify_witnesses`** establish that the tree head itself is trustworthy — the operator's Ed25519 signed-note signature, and that a new head extends one the holder archived.
- **`verify_statement_anchor`** / **`verify_usage_report`** cover the statement and usage-report proofs the same ledger also carries.

## The pieces a verifier needs

- `ProofBundle` — what the billing endpoint hands the user (`GET /api/v1/entries/{entryId}/proof`).
- `Hash`, `TreeHead`, `ConsistencyProof` — re-exported from `doubleentry` so the verifier's types match the ledger's.
- `PriceLink` — the signed binding between a settlement's `(channel, model, version)` and the operator's catalog row.

```rust
use oxsum_verify::{verify_bill, Hash};

let bundle_json: &str = /* GET /api/v1/entries/{entryId}/proof, inner `data` */;
let recorded: Hash = Hash::parse_hex("…the 64-hex hash on the bill…")?;

let verdict = verify_bill(bundle_json, &recorded)?;
// verdict.verdict is "pass", "chargeMismatch" or "fail"
```

## Wasm

The crate compiles clean to `wasm32-unknown-unknown`. Building with `--features wasm` produces a `cdylib` whose `wasm.rs` bindings the browser page imports — the exports take and return JSON strings so no `serde-wasm-bindgen`/`tsify` dependency enters the chain. `scripts/build-verify-wasm.sh` drives this build inside the oxsum repo.

## Money precision

`SCALE` is `6` — one credit is `1_000_000` minor units, fine enough for per-token pricing. The entry encoding (and therefore the content hash) depends on it, so the writer and every verifier must agree; it lives here, not in any consumer.

## Status

`publish = true` so it can ship on its own (issue #468), but the crate is developed inside the oxsum workspace — the vendored `doubleentry` copy it depends on is published too, and the two move together. This README is for the consumer verifying a bill or embedding the check; contributor notes live in the [repo](https://github.com/AllureCurtain/oxsum).

License: MIT OR Apache-2.0.
