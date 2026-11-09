# Team deletion and admission (#859)

Status: source repair and controlled regressions prepared; no runtime validation
claimed yet. Integration follows #854 before customer readiness.

Team deletion previously locked the team first, then its `ON DELETE SET NULL`
action updated memberships. Monetary and gateway admission lock the member
before reading the team's budget, allowing opposite row ownership. Deletion
now takes the existing organization writer lock first, matching member
reassignment and conflicting with admission's earlier organization key-share
lock. No budget calculation, historical posting or membership is removed.

The regression module exercises legacy monetary holds, atomic wallet admission
and fully quota-funded admission for each interleaving:

- A deletion paused inside a targeted row trigger already owns the organization
  and team. Admission demonstrably waits through `pg_blocking_pids`, then reads
  the detached member and succeeds even though its former team's cap rejects
  the same request before deletion.
- Admission paused at its member read owns the parent share. Deletion must wait
  there before acquiring the team, and the reservation survives the later
  deletion. A subsequent reservation uses the committed detached roster.
- Both winners of member reassignment are controlled at the same parent/row
  boundary. Earlier reservations remain; later admissions see the target team's
  cap, including existing monetary spend even for a quota-funded request.

Assertions retain member seats, check exact reserved and available money, release
or sweep all accepted holds, preserve earlier charges, and verify quota returns
to zero. All interleavings are over real PostgreSQL locks, without scheduler-only
sleep races. Runtime results, source revision and logs will be recorded before
PR publication. Production changes only `crates/core/src/teams.rs`; focused
team/key-budget/coordinated tests and changed-crate strict Clippy are sufficient,
with no repeat of #849's workspace run.
