# Funded and free admission (#849)

Status: development gates passed; protected PR integration pending. This evidence does not certify a release
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

The implementation is rebased onto shared borrowed ledger reads #856 / PR #860
(`4fc5581a`). Its duplicate transaction-read helper was removed; canonical entry
verification, hold decoding, calendars and receipt reads use that integrated
implementation. The branch is also rebased onto payment migration 0115 in
PR #868 (`f3802d5e`), and migration 0116 is now registered immediately after it.
The independent wallet-open DDL repair #867 is integrated in PR #870
(`f89fcb65`); this branch is rebased on that revision for its admission runs.
This dependency reconciliation is source work and does not constitute executed
#849 validation.

The first executed repair suite ran 23 cases: 21 passed and two failed. The
recovery receipt omitted the zero-use input/output lines required by the existing
settlement parser; restoring the original recorded rates fixed that case in a
narrow rerun. The remaining quota-plus-cache case returned a cache hit, then its
settlement transaction rolled back with `description exceeds 512 characters`.
Issue #873 owns that independent bounded evidence-capacity repair, integrated
in PR #890 (`134a4aae`) before this branch's final validation. Logs: `/tmp/oxsum-849-focused.log` and
`/tmp/oxsum-849-two-boundaries.log`. These intermediate results are not a passing
gate or release acceptance.

After that repair, all 23 funded-admission cases passed at `c5f69cf2` (seven
unrelated cases filtered), then the combined core/server admission, wallet-key,
mandate, team, organization, subscription, plan, cache, gateway, request and
sweep selection passed 248 tests (one existing opt-in skipped). Logs:
`/tmp/oxsum-849-final-focused.log`, `/tmp/oxsum-849-combined.log`. The branch was
then rebased onto integrated main `134a4aae`; runtime source under core/server,
the engine and verifier was confirmed identical. The final cross-crate run at
`8dcef5957e3b7a8e2d2e4bb33d4a27749f2a0fe8` passed all 2,070 tests (six existing
opt-in skips, 326.855 seconds). `cargo clippy -p oxsum-core -p oxsum-server
--all-targets -- -D warnings` passed in 4m28s. Four frontend contract groups,
code generation and the production frontend build passed; generated schema had
no drift. Formatting/diff checks and two independent source reviews passed.
Logs: `/tmp/oxsum-849-workspace.log`, `/tmp/oxsum-849-clippy.log`,
`/tmp/oxsum-849-frontend.log`. Per the revised test scope, this one full run is
justified by the core/server transactional boundary and migration; documentation
or equivalent-source rebases do not trigger another full run. Customer readiness follows this repair and the
separate cancellation boundary #854, team deletion #859 and evidence budgeting
#875 issues. The independent source-publication race #891 is being repaired
before this PR merges; no result here claims that concurrent path is fixed.

Source review also identified an existing team-deletion lock inversion: admission
locks membership before team; deletion owns the team before its foreign-key
action updates memberships. Issue #859 tracks a focused fix and controlled
runtime evidence after #849/#854, before customer readiness. It is not covered by
the current implementation or represented as an executed regression.
