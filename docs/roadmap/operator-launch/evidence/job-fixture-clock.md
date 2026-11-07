# Job fixture clock authority (#770)

A full collection-authority workspace run executed 1,814 tests and failed only
`faults::a_failed_job_backs_off_then_dies_and_the_kind_recovers` at first claim.
The fixture enqueued a host-current instant while PostgreSQL determines whether
it is due. A direct isolated probe sent 200 host timestamps to PostgreSQL:
199 were still in its future. The faults suite subsequently passed unchanged.
The run was retained as failed rather than calling a retry a full green result.

Anchor the fixture's enqueue time and backoff comparison to PostgreSQL
`clock_timestamp()`. Keep production scheduling, the due-time predicate,
backoff, five-attempt limit and dead-state assertions unchanged. No sleeps or
looser accounting/performance checks were introduced.

Executed 2026-11-08 on a branch from `0314cbd`:

- fmt, diff check and strict workspace Clippy passed.
- PostgreSQL-backed jobs/faults selection: 38 passed, no skips.
- The originally failing case: five additional consecutive one-test runs passed;
  the other 16 faults cases were explicitly filtered in each repetition.

Logs: ignored `local-notes/operator-launch/clock-*.log`. This focused fixture
repair does not substitute for the final combined candidate's full CI. No
provider/payment attempt or expense occurred.
