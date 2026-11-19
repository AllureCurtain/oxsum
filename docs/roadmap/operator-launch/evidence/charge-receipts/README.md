# Readable charge receipt acceptance (#1233)

Date: 2026-11-20 (Asia/Shanghai). Local synthetic seeded data only; no live
provider, payment or production deployment.

- PostgreSQL 17 focused integration: five new receipt tests actually created
  and dropped scratch databases. Covered analytics/usage/operation loss,
  historical hold metadata, ambiguous provider IDs, sibling and foreign
  credentials, staff lookup/proof and promo-expiry movements. A signer plus
  supplier rates verifies that the Business proof omits wholesale binding.
- `cargo nextest run --workspace`: 2,591 passed, 6 runner-filtered skips;
  elapsed 483.627 seconds. This is local regression, not release certification.
- Strict Clippy for core/server, pinned formatter, contract/codegen and React
  production build passed.
- Independent agent review approved after fixes for usage provenance,
  historical hold ceiling, promo burn, same-ID refresh, Business discovery,
  proof redaction, empty ledgers and unrelated wallet initialization.
- Ego browser inspected customer Requests and Business Customers with seeded
  historical ledger-only history. Lookup showed original 20,077-minor ceiling,
  2,286-minor charge and 17,791-minor unused ceiling. Readable text and staff
  proof downloads succeeded. Both customer prefill and exported staff proof
  verified through `/verify` (inclusion and recorded arithmetic).
- At 390 CSS pixels the Business receipt document width was 390; wider tables
  remained in keyboard-focusable scroll regions. Screenshots accompany this
  file. Browser runtime used the server verifier fallback; this does not claim
  an independently trusted head or provider usage truth.

The initial test invocation ran before PostgreSQL became available and was
excluded: testkit returned early when scratch creation failed. The new receipt
acceptance tests require a reachable database, preventing that false evidence.
