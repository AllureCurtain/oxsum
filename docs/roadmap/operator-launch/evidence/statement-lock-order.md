# Plan statement lifecycle lock ordering

Issue [#811](https://github.com/AllureCurtain/oxsum/issues/811). Implementation
commit `37655dc`, based on integrated main `de3bbca`. These are local PostgreSQL
development checks. No PSP, browser or final release-candidate verdict is
claimed.

## Correctness boundary

Staff review formerly locked a statement before updating its customer plan,
opposite close/refund's plan → journal → ledger → statement order. The overdue
worker selected statements `FOR UPDATE SKIP LOCKED` through an autocommit pool
query: those locks were gone before it opened each processing transaction.
It then changed statement before plan, used stale due/state information and
counted an already-suspended plan as newly suspended on every pass.

Review now reads the immutable business-scoped plan coordinate, locks the
plan, and re-reads the statement under its own lock. Dunning reads candidate
IDs in keyset pages, then takes plan → statement with `SKIP LOCKED` inside one
transaction. Current state, remaining debt, due time, plan state and grace
determine the committed action. Paid/void/extended candidates cannot cause a
stale suspension. Only actual transitions count against the pass budget.
Skipped rows release all transaction locks and remain eligible for retry.

The limit bounds changed statements, not scan volume. Keyset pages continue
past locked oldest rows so a fixed locked prefix cannot starve other customers.
An entirely busy backlog can therefore require several read/lock-attempt
pages in one pass. Already-suspended and inside-grace past-due rows are excluded
from candidate work.

## Controlled concurrency evidence

The new `plan_statement_locks` suite observes PostgreSQL's `pg_blocking_pids`
before testing row ownership. A short polling delay permits observation; an
arbitrary sleep does not decide which writer wins.

| Scenario | Assertion |
| --- | --- |
| Grace extension and statement void behind a held plan | Review waits without owning the statement; a `NOWAIT` statement lock succeeds |
| Verified refund stopped at the ledger append lock, then review | Refund owns plan/journal; review waits on plan without owning statement, both complete and one refund allocation remains |
| Oldest plan locked, page/work limit one | Next page's independent due statement progresses; skipped one succeeds on the next pass |
| Statement locked after plan is available | Sweep skips promptly and releases its plan lock; next pass completes |
| Payment or future due date pending under the plan lock | Old MVCC candidate cannot suspend; after commit the next pass leaves it alone |
| Two workers on one due statement | Combined counters report one transition and one suspension |

The refund/review case begins with historical bill 3,500,000, applied payment
2,500,000 and debt 1,000,000. A verified 500,000 refund leaves Cash 2,000,000,
journal billed 3,000,000 / paid 2,000,000 and unchanged debt. Grace review
preserves those statement amounts; document void retains its existing
document-only zeroing behavior. No retry or fabricated ledger posting hides a
deadlock.

For a baseline check, only production `plan_statements.rs` was temporarily
restored from `de3bbca`, with the new tests unchanged. Both selected tests
failed: review had already taken the statement (`55P03` on `NOWAIT`), and the
oldest locked plan blocked the overdue pass until its two-second guard
expired. The fixed source was restored immediately afterward. No baseline
source edit remains.

## Validation

With explicit local PostgreSQL `DATABASE_URL` and
`DOUBLEENTRY_DATABASE_URL`:

- Six new controlled tests passed, none skipped.
- The adjacent core selection (`plan_statement_locks`, `plan_statements`,
  `plan_refunds`, `journals`, `plan_refund_conservation`) passed 45 tests in
  11.079 seconds. One independent H1 test was still ignored on base `de3bbca`;
  #803 separately activates and repairs it.
- All three HTTP `plan_statements` checks passed.
- `cargo clippy --workspace --all-targets -- -D warnings` passed in 39.41
  seconds. Formatter and whitespace checks passed.
- An independent source review confirmed compatibility with close/refund and
  #809's subscription → plan → journal → ledger → statement order; no new
  blocker was found. Required PR CI and merge remain integration gates.

## Separate financial review boundary

This change fixes lock ownership and current-state processing. The existing
staff statement-review `void` action still cancels only the customer document;
it does not reverse the journal receivable or clear dunning. Grace review also
retains its existing restoration policy. Those combined multi-document and
multi-debt semantics are being assessed separately under N10 and must not be
described as a completed debt write-off workflow. Explicit repayment remains
#777; daily recognition remains N11.


After rebasing onto the merged #803 source `e35e557`, implementation at PR head
`dd7b7d8` passed the combined core and HTTP selection: **53 passed, zero failed,
zero skipped**, 14.012 seconds. H1–H6, all refund scenarios and the six controlled
lock cases executed. The subsequent rebase onto `45a833e` only integrated SDK
publication tooling and documentation; this implementation and its tests are
unchanged. No additional source correction was required.
