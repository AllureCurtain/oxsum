# Bounded provider usage recovery acceptance (#1236)

Engineering acceptance uses a local PostgreSQL instance and synthetic provider
fixtures. No live OpenAI credential, stored completion or supplier bill was used.

## Focused regression evidence

- `recovery-core-tests5.log`: 13/13, including pool-one settlement, discount/cap,
  unknown/contradictory/cumulative components, original configuration, stale
  claims, deadline/late evidence, sealed dates, post-ledger rollback, concurrent
  claims/finalizers and subscription rollover (old usage18/new reservation70).
- `recovery-http-tests4.log`: 6/6, including exactly one POST/replay, 429,
  pre-dispatch persistence failure, pending response without a zero charge,
  19-second synchronous heartbeat, cancellation in post-call moderation and
  cross-process durable WebSocket snapshot reconciliation.
- `recovery-queue-funding-tests.log`: 40/40, including existing subscription
  funding suites, 13 recovery tests, bounded correlated read-only GET errors,
  and Business queue authority/redaction after diagnostic deletion.
- Strict core/server Clippy, pinned format check, 16 contract/locale checks,
  generated API types and TypeScript/Vite production build passed.

The first workspace pass stopped after two `sale_heads` fixture timing failures
(`no active sale price` at immediate current-time resolution). No production code
was changed to mask these: all three sale-head tests passed on focused retry.
The complete no-fail-fast workspace rerun is recorded in the PR checks.
Raw local logs are retained in `/tmp/oxsum-billing-program/` for this execution;
required GitHub CI and independent review are linked from the PR.

## Browser acceptance

Ego Chromium used the real React portal and rebuilt server against a synthetic
`oxsum_correction_browser` database. The customer explicitly accepted existing
provider policy version1/digest before calls; historical legacy acceptance stayed
readable. A missing-usage response without lookup closed at zero, released212
minor units, retained unknown provider quantities and exported a readable receipt.
See [closed receipt](closed-receipt.png).

For a reproducible deferred path, a temporary **test-database-only trigger**
marked the local mock attempt lookup-capable. The gateway response omitted the
charged header and the receipt showed pending with unknown input/output. A
fixture next-lookup delay allowed UI inspection, then the fixture deadline was
moved into the past and the real background job was scheduled. No production
configuration permits this local GET capability. The receipt refreshed to closed,
waived both unresolved sides, released216 minor units, and the live indicator
became idle without navigation. The triggers were removed. See [pending
receipt](pending-receipt.png) and [automatic closure on mobile](deferred-closed-mobile.png).

Business Exceptions showed the durable closed-unknown queue and the operation
and customer identities needed for receipt lookup. Both this queue and the
customer receipt fit a390px viewport with document width390px. See [operator
queue](operator-queue.png) and [mobile queue](operator-mobile.png). Readable
receipt export was downloaded and inspected for unknown quantities/waiver and
no secrets.

## Independent review and external limits

`/root/review_financial_foundation` approved after reviewing the final code and
actual focused logs. It identified financial, cancellation and UI lifecycle
issues; all material findings were fixed and verified. Two minor prose findings
were corrected. Review is separate from required CI, not a substitute for it.

Actual canonical OpenAI stored-completion authorization and usage availability
remain external certification under #740. Two independent operator pilots remain
#745. These fixtures establish engineering behavior, not actual provider token
truth, complete customer delivery or production readiness. No deployment or
candidate promotion was performed.
