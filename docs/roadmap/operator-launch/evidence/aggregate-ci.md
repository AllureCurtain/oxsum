# Aggregate integration test execution (#1003)

Baseline main: `2ee706019a02f24b4b8ed4b949b8c5254b27a178`.
CI [42819010276](https://github.com/AllureCurtain/oxsum/actions/runs/42819010276)
failed all three shards with `operator didn't match any binary names` before
running tests. The old extras shell loop found no generative/seed/jobs binaries
and performed no shared-suite execution.

The repaired shard filter excludes only core generative/seed/jobs and server
jobs module prefixes in their aggregate binaries. The real nextest JSON
inventories were compared test by test: all 2,230 selected ordinary tests remain
selected, all 26 process-shared tests are excluded, and existing ignored cases
retain their state. Core/server library tests with similar names remain selected.

The new archived-binary runner checks every selection before running any suite.
Absent/ambiguous executables, empty/wrong-prefix inventories, missing DATABASE_URL,
failed subprocesses and zero/incomplete runs fail. Six independent executable
fixture groups passed. Seven existing candidate-gate fixtures, pinned formatting,
diff checks and actionlint passed.

Actual compiled aggregate execution against local PostgreSQL passed:

| Crate/module | Passed |
| --- | ---: |
| core/generative (default 1,000 generated sequences) | 3 |
| core/seed | 2 |
| core/jobs | 12 |
| server/jobs | 9 |

The generative process reported best-effort database teardown failure; the
ordinary age-limited scratch sweep retains responsibility for that residue.
No live or fresh foreign database was removed. The other suite teardown logs
reported their scratch databases dropped.

No production Rust changed. The independent bundle-upgrade failure on main
remains in PR #975 / issue #1002, not hidden by the repaired selection. This
local focused execution is not full workspace, engine or candidate acceptance.
