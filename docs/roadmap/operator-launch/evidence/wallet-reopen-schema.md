# Current wallet reopen and active settlement (#867)

Status: implementation and regression fixtures prepared; runtime validation pending.

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

## Repair and intended checks

The engine's full canonical schema revision is checked under migration
serialization and committed atomically with DDL. Current reopens preserve the
existing trigger objects. Missing or changed revisions still upgrade. Wallet
account rules and lazy migrations remain active.

Prepared tests cover an independent one-connection pool reopening while an
atomic settlement owns ledger relation locks, rollback/retry and conserved
balances, retained receipt proof, unchanged current trigger identity, old
metadata, changed revisions, foreign ledger identity, and an injected final
stamp failure that rolls back DDL and revision together. Cancellation is checked
only after the database reports a real DDL lock wait; the pool remains open
while an independent connection proves migration ownership was released.

The existing gateway watchdog and financial assertions are unchanged. Focused
runtime checks, all engine tests, cross-crate workspace checks, strict Clippy,
formatting and independent review must pass before integration. These are
development checks; no release candidate, deployment or mixed-version upgrade
acceptance is implied.
