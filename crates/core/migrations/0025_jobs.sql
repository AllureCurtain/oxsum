-- The periodic-work table (issue #166, roadmap P8-1). One row per scheduled run:
-- the in-process interval loops (hold sweeper, webhook delivery) move onto it,
-- and reconciliation, monthly statement generation and retention cleanup arrive
-- with the layer. `webhook_deliveries` keeps its own queue — it is a user-visible
-- surface, not an internal job.
--
-- `FOR UPDATE SKIP LOCKED` claiming plus the lease columns make the queue
-- multi-instance safe without a separate scheduler: a worker whose claim lapses
-- is assumed dead, and the next claim takes the row over.

CREATE TABLE oxsum.jobs (
    job_id      uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The periodic kind the run belongs to (`sweep-holds`, `deliver-webhooks`,
    -- `reconcile`, `statements`, `retention`): the dispatch key.
    kind        text        NOT NULL,
    -- When the run is due; the claim orders on it.
    run_at      timestamptz NOT NULL,
    status      text        NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'running', 'done', 'dead')),
    -- Attempts already made against this run.
    attempts    int         NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    -- The lease: set when a claim takes the row, fenced on completion.
    claimed_at  timestamptz,
    last_error  text,
    -- Extra input one run carries; the periodic kinds need none today.
    payload     jsonb       NOT NULL DEFAULT '{}',
    created_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);

-- A periodic kind is a singleton while a run is due or in flight — enqueueing
-- the next occurrence while one waits would run the pass twice. `done`/`dead`
-- rows carry history only, so the index admits them freely.
CREATE UNIQUE INDEX jobs_periodic_singleton ON oxsum.jobs (kind)
    WHERE status IN ('pending', 'running');

-- The claim's due scan.
CREATE INDEX jobs_due ON oxsum.jobs (run_at) WHERE status = 'pending';
