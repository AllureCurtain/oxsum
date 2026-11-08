# Retained customer ledger trust

Issue #858, under N12/#738. Based on integrated `8f92546d`.

The Bills page previously displayed a successful consistency HTTP response as a
verified extension, without checking the supplied old root or executing the
verifier. Its unscoped archive retained only size/root. The replacement calls the
existing Rust/WASM verifier, pins the initial key and expected customer origin,
and stores the signed head with organization/site/ledger identity. It never
silently migrates an unidentified archive or replaces the original trust record.
Web Locks serialize first retention between tabs; a changed record requires an
explicit reload/inspection. Original inputs and checked proof can be downloaded.

Validation executed:

- Five behavioral tests: customer/site/ledger isolation, malformed/legacy storage,
  actual positive-verdict requirement, unavailable verifier, changed key/origin,
  rollback/fork, unchanged head, and empty-head signature-only semantics.
- Four actual-WASM test groups: valid signature and extension; altered old root,
  path, signature, archived root and operator key; genuine signed rollback/fork,
  foreign origin and empty-head checks. Engine-produced BLAKE3 Merkle fixtures
  and freshly generated Ed25519 signatures exercise the actual verifier.
- TypeScript and production Vite build passed. Existing large-chunk warning
  remains; generated deployment placeholders are not committed.

The initial fixture experiment used SHA-256 CT nodes; the actual engine verifier
correctly rejected it. The passing suite uses the repository's engine-generated
BLAKE3 fixtures. This is development evidence using the existing local WASM
artifact, not a claim that the final candidate or browser journey was exercised.
The five behavioral checks are included in frontend CI; the documented real-WASM
command fails rather than skipping when its artifact is absent or a placeholder.

Trust remains explicit: a server-fetched key and verifier do not independently
establish the operator's identity or honest write-time metering. An empty retained
head cannot prove an earlier history. Entry proofs, period bundle delivery and
final-candidate offline/browser acceptance remain outstanding under N12.
