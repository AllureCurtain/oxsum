# Lazy cancellation admission (#854)

Status: contract, test-only baseline and implementation prepared on integrated
funded/free admission #849 / PR #892 (`7e2c6216`). No runtime validation is
claimed yet.

The lazy plan rollover previously returned `false` for both a cancelled plan and
a still-serving past-due window. Admission proceeded to draw the ended quota in
both cases, so the first request after a cancelled boundary could consume it
before the worker ran. Cancellation also left quota-period history marked open.

The prepared core regression exercises both overage policies without running the
worker first, requires no new draw or entitlement, then settles a pre-boundary
reservation twice and checks the retained period and once-only invoice close.
The production repair rechecks subscription state under the existing lifecycle
lock and closes quota history with cancellation. The past-due remainder path
remains active. The #849 borrowed admission API carries the state check in its
transaction. A gateway regression checks repeated insufficient-wallet refusal,
full quota/journal/operation/watch rollback and successful wallet-funded retry.
A controlled PostgreSQL trigger pauses the boundary owner while retaining the
lifecycle lock; both worker-first and request-first cases prove the other path
and an earlier draw's settlement wait through `pg_blocking_pids`, then assert
once-only closure, old-draw settlement/replay and wallet-only new-call charging.

Test-only baseline commit: `600c7dc1`, production unchanged from `f3802d5e`.
Execution results and exact final code revision will be added before PR review.
This is a core-only production change; gateway source is test-only and the
contract changes prose only. Focused plan/subscription and coordinated gateway
checks, changed-crate strict Clippy and formatting are the local gate. The
unchanged #849 cross-crate workspace evidence is reused.
