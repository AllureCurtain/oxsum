# Legacy deposit reversal recovery evidence

Date: 2026-11-08. Child #763, N10/#736. Base `0314cbd3d5439adbc904a327a8e16704b344a0af`.

## Behavior verified

The ledger reversal and durable external operation commit together. Recovery
uses the original merchant/version, immutable body UUID, exact payment amount
and currency, and bounded destination-policy HTTP requests. A claim precedes
I/O; five attempts exhaust automatic work without claiming definite failure.
HTTP success establishes at most a matching pending handle. Only an exact signed
callback establishes a terminal outcome, and a late success cannot be downgraded.
Historical rows retain original deposit JSON and ledger references; old handles
are read-only and unbound operations cannot acquire new create authority.

## Executed checks

- `scripts/fmt-check.sh`, `git diff --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- Full PostgreSQL-backed workspace nextest: **1,813 passed, 6 opt-ins skipped**,
  228.525 seconds of test execution. The skips remain separate candidate gates.
- Focused tests exercise actual migration of pre-0100 data, immutable history,
  unmatched historical amounts/handles, lost response recovery, concurrent
  claims, five-attempt exhaustion, exact/mismatched signed events and reordered
  failed/succeeded events. Database credentials were explicitly loaded for
  these runs; a preliminary run without DATABASE_URL is not counted as evidence.
- `npm --prefix web run build` and `cargo build -p oxsum-server`: passed. The
  existing Vite chunk-size advisory remains; no limit was relaxed.
- `scripts/deposit-refund-smoke.py`: passed against the pinned v1.129.0 switch,
  using debug binary SHA-256
  `8c4d8ca4b79f809c66bf32905ebe441e80ed4eb6c89126227f9192336ef1fb9f`.

The smoke created deposit `fef3d11f-cc9c-4d69-b0bd-f8bbbd1e798e`, provider payment
`pay_bWIHqBPoXSwrLCoh5mwr` and refund `6c44865c-a374-4226-80ec-eac07fed71e2`.
One refund POST committed while its response was dropped; the pre-read and
recovery read found the exact same operation. One extra direct fixture read
verified the provider's final result (three refund GETs total). Repeated
reversal and duplicate signed events preserved entry
`55129329-6f17-5594-ba75-6ce58810bf84` and ending wallet balance zero.

Provider payment and full refund were USD 2.00 each. This compatibility deposit
API credits **200 credit-minor**, its historical units, not the purpose-scoped
order API's 1-credit/USD conversion. The evidence keeps these two figures
separate rather than relabeling a legacy balance. This repair does not migrate
legacy deposit pricing semantics.

## Retained evidence and limits

Ignored logs: `local-notes/operator-launch/deposit-{focused-database,upgrade-tests,edge-tests,final-fmt,final-clippy,final-workspace,final-build,frontend,final-switch}.log`.
The pinned sandbox retains `.runtime/smoke-e9f88a8e4902/result.json`; its task-only
listener/proxy/database were cleaned by the runner. The shared stack/volumes and
other databases were preserved.

Genuine review, protected integration and final exact-SHA candidate gates are
pending. Locally signed events and a dummy connector do not certify a PSP,
automatic callback delivery or browser checkout. Real-model attempts/spend: zero.
