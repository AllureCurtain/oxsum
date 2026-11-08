# Funded and free admission (#849)

Status: implementation under validation. This evidence does not certify a release
candidate, provider, PSP, browser journey or external adoption.

## Baseline

Production code at `1e25fbaef2cda34984674a37a82e06e302fef0e1` was unchanged from
merged #843 (`df4ad4f`). The baseline commit added four gateway regressions; the
execution moved only their module declaration below the shared fixture macro.
The explicit local PostgreSQL fixture ran four tests (seven unrelated tests were
filtered out), and all four failed as expected:

| Regression | Observed | Required |
| --- | --- | --- |
| Manual suspension with full quota | 200; reached serving | 403 before upstream |
| Expired mandate with full quota | 200 | 403 `MANDATE_EXPIRED` |
| Second full-quota stream at concurrency 1 | 200 | 429 `TOO_MANY_HOLDS` |
| Explicit zero-priced call on empty wallet | 500 | 200 with zero-cost proof |

Command: `cargo nextest run -p oxsum-server --test coordinated -E 'test(funded_admission::)' --no-fail-fast`.
Build 39.52 s; test execution 1.672 s; exit 100. Execution log retained locally at
`/tmp/oxsum-849-baseline.log`. These are reproduced defects, not passing evidence.

## Required repair checks

The focused suite additionally covers current key revoke/expiry/scope changes,
key/member/team and mandate monetary caps, cache-hit restrictions, partial quota
rollback, atomic watch-write failure and legacy-watch insertion races, concurrency races/double counting,
independent identical operations and explicit idempotent replay, zero-wallet
recovery with failure/retry, one-connection money/quota/free recovery, distinct-key
small-pool bursts, controlled lock order, both winners of streaming cancellation
versus recovery, and plan-dunning overage-only behavior.

Results, exact runtime SHA, final focused/combined regression gates and review
will be recorded after execution. Customer readiness follows this repair and the
separate cancellation boundary issue #854.
