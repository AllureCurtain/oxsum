# Current wallet reopen and active settlement (#867)

Status: baseline failure reproduced; focused, engine and combined workspace
runtime checks, strict Clippy, formatting and independent source review passed.
Normal PR integration pending.

## Observed failure

Main `4fc5581a` failed [CI run 42525666821](https://github.com/AllureCurtain/oxsum/actions/runs/42525666821)
in `gateway::a_client_that_hangs_up_at_the_terminator_still_gets_its_usage_bill`.
At 21:44:08.033 UTC the shard-3 PostgreSQL log reported backend 2292 waiting for
`RowExclusiveLock` on relation 713606 while backend 2280 reapplied reference DDL
and waited for `AccessExclusiveLock` on relation 713671 held by 2292. PostgreSQL
killed the settlement's `INSERT INTO entries`. The test's separate `Tenants`
registry first opens its wallet during receipt polling, reproducing the same
independent-process cold-open path available in production.

The same inversion recurred on main `cbabfafd` in
[CI run 42528717370](https://github.com/AllureCurtain/oxsum/actions/runs/42528717370):
`a_client_that_hangs_up_at_a_lingering_terminator_still_gets_its_usage_bill`
failed the same settlement assertion. The shard-2 PostgreSQL log at
22:21:50.343 UTC shows backend 2703's `INSERT INTO entries` killed while it
and DDL backend 2704 waited on each other's relation locks. These repeated
failures require a production concurrency repair, not a longer test watchdog.

## Repair and executed checks

The engine's full canonical schema revision is checked under migration
serialization and committed atomically with DDL. Current reopens preserve the
existing trigger objects. Missing or changed revisions still upgrade. Wallet
account rules and lazy migrations remain active.

Executed tests cover an independent one-connection pool reopening while an
atomic settlement owns ledger relation locks, rollback/retry and conserved
balances, retained receipt proof, unchanged current trigger identity, old
metadata, changed revisions, foreign ledger identity, and an injected final
stamp failure that rolls back DDL and revision together. Cancellation is checked
only after the database reports a real DDL lock wait; the pool remains open
while an independent connection proves migration ownership was released.

The existing gateway watchdog and financial assertions are unchanged.

| Check | Actual result |
| --- | --- |
| Production baseline `cbabfafd`, test-only cold reopen fixture | One executed failure in 1.149 s: `55P03` because the re-open's DDL waits for the settlement's held relation locks; `/tmp/oxsum-867-baseline.log` |
| Wallet suite and the three previously failing gateway scenarios | 37/37 passed in 10.949 s; `/tmp/oxsum-867-focused.log` |
| Engine with `postgres,sqlite,witness` features | 519/519 passed, one existing ignored test; includes PostgreSQL/deferred/SQLite conformance and the new migration checks; `/tmp/oxsum-867-engine.log` |
| Engine documentation examples with the same features | 29 passed, four existing ignored examples; `/tmp/oxsum-867-engine-doc.log` |
| Combined workspace at `3a037b50325d8941c1393eed2d0b6f6148e1bd1d`, rebased on refund PR #868 / `f3802d5e` | 2,042/2,042 passed in 283.599 s; six existing optional tests skipped; `/tmp/oxsum-867-workspace.log` |
| Strict lint and formatting | `cargo clippy --workspace --all-targets -- -D warnings` passed in 4m 30s; `scripts/fmt-check.sh` and `git diff --check` passed |
| Independent source review | Root and financial reviewers passed the migration transaction, ownership, revision and cancellation boundaries |

Commands used explicit local PostgreSQL URLs for both `DATABASE_URL` and
`DOUBLEENTRY_DATABASE_URL`. The engine run enables the PostgreSQL, SQLite and
witness paths; optional Iceberg integration was not enabled. Exact engine code
was unchanged by the refund rebase. The final follow-up commit only records
these results and updates integration status. These are development checks; no release
candidate, deployment or mixed-version upgrade acceptance is implied.
