-- The async task relay's own table (issue #503): one row per submitted task —
-- the billing-side record of a long-running upstream job, polled by the
-- `poll-tasks` periodic kind and settled on the terminal state. The ledger
-- side needs nothing new: a task's hold is an ordinary `req-<id>:hold` the
-- same sweeper watches, and its settle the same `req-<id>:settle`.

CREATE TABLE oxsum.tasks (
    -- oxsum's own id: the one the client polls and cancels by, and the request
    -- id the hold/settle pair derives from. A uuid rather than the upstream's
    -- id so the client's handle is ours and a foreign key cannot probe another
    -- organization's tasks.
    task_id          uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id  uuid         NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    -- The API key that submitted, for attribution — the usage record's key.
    key_id           uuid         REFERENCES oxsum.api_keys (key_id) ON DELETE RESTRICT,
    -- The channel that accepted the submission: where the poll pass asks.
    channel_id       uuid         NOT NULL REFERENCES oxsum.channels (channel_id) ON DELETE RESTRICT,
    -- Upstream's own id for the job, returned by `POST {baseUrl}/tasks`.
    external_task_id text         NOT NULL,
    kind             text         NOT NULL
                     CHECK (kind IN ('image', 'video', 'batch', 'custom')),
    state            text         NOT NULL DEFAULT 'submitted'
                     CHECK (state IN ('submitted', 'running', 'succeeded', 'failed', 'cancelled')),
    -- The billing side: the hold key the freeze lives under and what the
    -- terminal settle charged (NULL while open).
    hold_key         text         NOT NULL,
    freeze_minor     bigint       NOT NULL CHECK (freeze_minor >= 0),
    charged_minor    bigint       CHECK (charged_minor IS NULL OR charged_minor >= 0),
    model            text         NOT NULL,
    -- Upstream's answer body on success and the reported failure otherwise;
    -- the result the poll returns to the caller.
    result_json      jsonb,
    error_json       jsonb,
    -- The settled usage report upstream's terminal answer carried (its
    -- `usage` object verbatim); NULL while open or when it reported none.
    usage_json       jsonb,
    submitted_at     timestamptz  NOT NULL DEFAULT now(),
    -- When the state went terminal — succeeded, failed or cancelled.
    completed_at     timestamptz
);

-- The poll pass's working set: the still-open tasks. The terminal states are
-- excluded so the index stays small — a task spends its life open and its
-- history closed.
CREATE INDEX tasks_open ON oxsum.tasks (state) WHERE state IN ('submitted', 'running');

-- One channel's tasks, for the admin listing and for a channel's own cleanup.
CREATE INDEX tasks_channel ON oxsum.tasks (channel_id, submitted_at DESC);

-- The organization's own list, newest first — the dashboard's tasks page.
CREATE INDEX tasks_org ON oxsum.tasks (organization_id, submitted_at DESC);
