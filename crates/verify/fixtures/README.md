# Description capacity fixtures (#873)

These are synthetic receipts written through `Wallet::usage_receipt` and read
through `Wallet::receipt_proof` against a fresh local PostgreSQL test database.
The generator was the same `crates/core/tests/settlement_capacity.rs` test,
with two temporary file writes removed after capture. Neither fixture belongs
to a live deployment. The stored content hash is retained independently of the
bundle during verification; the supplied unsigned tree head is inclusion
evidence, not an independently trusted operator identity.

`description-composed.json` carries a v14 settlement with 200-byte model,
requested and upstream names, quota and cache provenance, mandate hash,
guardrail quotation with escaping/Unicode, experiment and sale digest.
Its description is 3,114 Unicode scalar values and 3,174 UTF-8 bytes. The
sale/mandate fields are synthetic commitments for encoding validation, not
evidence of an independently verified sale or signed mandate.

`description-max.json` carries exactly 16,384 four-byte Unicode scalar values,
65,536 UTF-8 bytes before transport escaping. It is a metadata receipt, not a
usage settlement. Native and actual WASM tests verify both fixtures, reject
modified descriptions under the retained hash and reject one scalar above the
limit. Existing short golden fixtures are preserved unchanged.
