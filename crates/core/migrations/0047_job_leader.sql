-- Job leader election (issue #281): on a multi-instance deployment every
-- replica runs every periodic pass — `FOR UPDATE SKIP LOCKED` makes the queue
-- safe (one row, one claimant) but not efficient, because each replica still
-- claims and executes its own copy of a periodic run. A leader-required kind
-- should run on exactly one instance per tick; the others yield.
--
-- Postgres is already the deployment's single point of truth, so leadership
-- rides a session-scoped advisory lock keyed on the kind's name — no new
-- component, and a crashed holder's lock releases with its connection so the
-- next instance's next claim takes over.

ALTER TABLE oxsum.jobs
    ADD COLUMN requires_leader boolean NOT NULL DEFAULT false;

-- Backfill the rows a pre-migration enqueue already wrote: the flag is a
-- property of the kind, so the known periodic kinds take it here. New rows
-- inherit it from `requires_leader(kind)` inside `Db::enqueue_job`.
UPDATE oxsum.jobs SET requires_leader = true
    WHERE kind IN (
        'sweep-holds', 'witness-checkpoints', 'statements', 'retention',
        'sync-upstream-prices', 'reconcile'
    );
