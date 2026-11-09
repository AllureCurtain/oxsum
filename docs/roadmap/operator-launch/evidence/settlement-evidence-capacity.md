# Bounded composed settlement evidence (#873)

Status: implementation under validation. No candidate, publication, provider,
PSP, deployment or browser journey is certified here.

## Reproduced failure

Test-only commit `4dd4d561` adds a quota-funded cache regression to production
unchanged from main `2c2a739d`. An explicit local PostgreSQL run executed one test
and failed with a missing settlement, after a successful source turn and cache
hit. The triggering #849 diagnostic logged `description exceeds 512 characters`
from the rolled-back settlement transaction. This is an existing composed-field
capacity mismatch, not a fixture timeout.

Baseline log: `/tmp/oxsum-873-baseline.log`; build 65 seconds, execution 7.238
seconds, one failure and 15 unrelated cache cases filtered.

## Scope and compatibility

The only production behavior change is the vendored `Description` bound from
512 to 16,384 Unicode scalar values, at most 65,536 UTF-8 bytes before JSON
escaping. The same constructor validates serialization input and storage reads.
PostgreSQL and SQLite TEXT have no description-length CHECK. No DDL, canonical
encoding, settlement version or historical hash changes.

The quota/cache gateway test asserts retained subscription/source-entry fields,
actual cache-rate charge, unchanged wallet money, consumed quota, one upstream
call, a verified receipt and no remaining watch. Distinct request-operation
identity is the separate #849 repair and is not claimed by this unit.

The composed fixture contains 3,114 scalar values / 3,174 UTF-8 bytes, with
200-byte model/requested/upstream names, full subscription/cache/mandate/guardrail/
experiment/sale fields. The maximum fixture contains 16,384 four-byte scalars,
65,536 bytes. Actual PostgreSQL receipt writes/reads, SQLite reads, native and
WASM proofs cover exact preservation, tamper rejection, old short proofs,
control-character refusal and bound-plus-one rejection.

Older 512-character readers cannot read longer new records. Upgrade the native
and served/distributed WASM verifier with the writer; a schema-identical rollback
still requires a compatible reader or pre-upgrade restore. Arbitrary policy
count/reason budgeting remains #875 before customer readiness/acceptance. A
finite capacity increase does not make unbounded configured evidence safe.

## Executed validation

- Cache suite: 16/16 passed, including the standalone regression.
- PostgreSQL composed/maximum receipts: 2/2 passed. The first new fixture run
  used an overlong synthetic tenant identifier; it was corrected to the same
  32-hex UUID accepted by production, then both cases passed.
- Complete native engine with postgres/sqlite/witness: 521 passed, one existing
  opt-in ignored, in an isolated single-thread run (18.541 seconds). The first
  parallel run passed 519 and failed two wall-clock scale ratios; those passed
  2/2 in isolation, and the complete serial rerun passed without threshold,
  fixture or production changes. Optional Iceberg is not enabled.
- Engine doc tests: 29 passed, four existing ignored.
- Native verifier: 94/94 passed.
- Actual rebuilt WASM: ten groups passed across description capacity, retained
  heads and whole-period evidence. Includes new maximum/composed/tampered/overbound
  cases and the original short proof. Build uses the current source, not the
  committed placeholders; generated output retained locally under
  `/tmp/oxsum-873-wasm`.
- Frontend: four contract groups, code generation and production build passed.

Full workspace, strict Clippy, exact final source SHA and independent review
remain to be recorded before the focused PR.
