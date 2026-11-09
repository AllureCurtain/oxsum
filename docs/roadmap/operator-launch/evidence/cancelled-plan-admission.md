# Lazy cancellation admission (#854)

Status: development checks and independent source review passed; protected PR
integration pending. This is not candidate, browser or external acceptance.
The repair is based on integrated funded/free admission #849 / PR #892
(`7e2c6216`) and scoped validation policy #894 (`397d07b0`).

The lazy plan rollover previously returned `false` for both a cancelled plan and
a still-serving past-due window. Admission proceeded to draw the ended quota in
both cases, so the first request after a cancelled boundary could consume it
before the worker ran. Cancellation also left quota-period history marked open.

The core regression exercises both overage policies without running the
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

The executed test-only baseline is `37b235962b35737918f982340df9739317fcbca3`,
with production unchanged from main `397d07b0`. The focused core regression
failed as expected: the first request drew 1,000,000 quota minor instead of zero.
Command: `cargo nextest run -p oxsum-core --test plans -E
'test(lazy_boundary_cancellation_never_spends_the_ended_quota)' --no-fail-fast`.
One test failed; 12 unrelated tests were filtered. Execution took 1.226 seconds.
Log: `/tmp/oxsum-854-baseline.log`. The earlier baseline `600c7dc1` was source
preparation only and was not executed.

At `090f0124178314f6fafe4eda7ddd3a78d2275172`, the repaired branch passed all
74 selected tests in 16.365 seconds with no skips:

```sh
cargo nextest run -p oxsum-core -p oxsum-server \
  --test plans --test subscriptions --test coordinated --no-fail-fast
cargo clippy -p oxsum-core -p oxsum-server --all-targets -- -D warnings
```

The selection covers both core and HTTP plan/subscription suites and coordinated
gateway checks. Changed-crate strict Clippy passed in 3m03s. Pinned formatting,
diff checks and independent source review passed. Four OpenAPI contract groups
and code generation passed; the generated schema changes only the matching
description comment, with no type or frontend runtime changes. Logs:
`/tmp/oxsum-854-focused.log`, `/tmp/oxsum-854-clippy.log`,
`/tmp/oxsum-854-contract.log`. Both runs used explicit local PostgreSQL fixtures;
the suite created and dropped its own scratch database. Initial test compilation
exposed two optional fixture-ID unwraps, fixed before any repair runtime result.

This is a core-only production change; gateway source is test-only and the
contract changes prose only. Focused plan/subscription and coordinated gateway
checks, changed-crate strict Clippy and formatting are the local gate. The
unchanged #849 cross-crate workspace evidence is reused.
