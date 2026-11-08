# Sanitized operator diagnostics (#819)

N17.a adds one operator-only aggregate read and an explicit private-file support
collector. It closes the diagnostic software unit after review and integration;
N17/J16 upgrade, drain, restore, rollback and old-proof/new-call acceptance remain
separate work. No candidate or external provider/PSP is exercised here.

## Implemented boundary

- `GET /api/v1/admin/diagnostics` reports eight parsed configuration flags and
  23 provider/merchant/recovery counts. One repeatable-read, read-only snapshot
  checks exact schema first; a five-second statement timeout and six-second
  endpoint deadline bound the read. Nonready storage has null counts.
- Stored provider/customer identities, URLs, secrets, amounts, payloads and error
  messages are never selected for output. The migration name is syntax checked.
  The endpoint uses the existing operator-token guard, never session authority.
- The Python standard-library collector accepts only the documented fields and
  types; it discards unexpected payloads/error bodies, disables redirects and
  ambient proxies, and sends the operator token only to the protected path.
  Explicit private regular files supply the token and optional literal env
  presence. It creates a new `0600` report; no full logs are collected.
- Presence/counts do not certify provider connectivity, key decryptability,
  mail delivery, PSP acceptance, ledger conservation or artifact provenance.
  Public probes and the protected snapshot are separate reads and can straddle
  a restart. See [operator procedure](../../../operator-diagnostics.md).

## Executed development evidence

On the task worktree based on `45a833e`:

- `cargo nextest run -p oxsum-core --test diagnostics -p oxsum-server --test operator_diagnostics`:
  six passed initially; seven after the source-review refinements, including the
  build-override unit check. A real PostgreSQL fixture populates every count with live, due,
  future, disabled and terminal rows, including both refund paths. Read-only
  snapshots preserve job rows and do not serialize private fixture data.
  HTTP tests cover the operator guard, session/key rejection, config flags,
  unmigrated/mismatched/unavailable DB states and malformed migration names.
- `python3 scripts/test-operator-diagnostics.py`: ten passed, using actual
  loopback HTTP listeners. They cover private inputs/output, no shell execution,
  public-only collection, exact token destination, 307 no-follow, unknown/invalid
  fields, oversized/error/deeply nested bodies, null counts and rejected plaintext remote URLs.
- Strict workspace Clippy, pinned formatting, four OpenAPI authority tests,
  generated TypeScript and the production frontend build passed. The existing
  Vite bundle-size advisory remains; this feature adds no frontend UI.

The complete workspace run on implementation commit `61f6d98` passed **1,905
tests**, with six existing opt-in skips and no failures (255 seconds). Independent
reviews by the coordinating and payment agents passed after tightening build-SHA
filtering, recovery-candidate semantics (fresh operations/metering excluded), and
malformed-JSON handling. The final bounded refinements passed seven focused core/HTTP/unit tests, ten
collector checks, four contract checks and strict workspace Clippy; the complete run is attributed to its actual source,
not represented as a second full run of the later refinement. The report fixture is synthetic and the PostgreSQL data
is task-owned; neither is a running candidate deployment or a restore rehearsal.
