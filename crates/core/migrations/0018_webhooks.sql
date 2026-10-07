-- Outbound webhooks (issue #144, roadmap P5-2): an organization registers an
-- endpoint, and every settled turn enqueues one signed delivery per enabled
-- subscribed endpoint — inside `record_usage`'s transaction, so a settlement and
-- its notification can never diverge, and the usage row's `ON CONFLICT` guard
-- means a replay enqueues nothing twice.
--
-- `webhook_endpoints` holds the signing secret sealed under OXSUM_SECRET_KEY,
-- the same protection a channel's upstream credential gets; only `secret_last4`
-- is ever read back out.
CREATE TABLE oxsum.webhook_endpoints (
    endpoint_id     uuid        PRIMARY KEY,
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE CASCADE,
    url             text        NOT NULL,
    secret_sealed   text        NOT NULL,
    secret_last4    text        NOT NULL,
    events          text[]      NOT NULL,
    enabled         boolean     NOT NULL DEFAULT TRUE,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX webhook_endpoints_organization ON oxsum.webhook_endpoints (organization_id);

-- One row per endpoint per event: the delivery queue and its attempt history.
-- `next_attempt_at` is the worker's due marker; `status` moves pending →
-- sending (a worker's atomic claim) → delivered on a 2xx, or back to pending on
-- a retry, or failed when the attempt budget runs out. `claimed_at` stamps the
-- claim, so a row whose worker died mid-flight is reclaimable once its lease
-- passes.
CREATE TABLE oxsum.webhook_deliveries (
    delivery_id     uuid        PRIMARY KEY,
    endpoint_id     uuid        NOT NULL REFERENCES oxsum.webhook_endpoints (endpoint_id) ON DELETE CASCADE,
    organization_id uuid        NOT NULL,
    event_type      text        NOT NULL,
    payload         jsonb       NOT NULL,
    status          text        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sending', 'delivered', 'failed')),
    attempts        integer     NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    claimed_at      timestamptz,
    response_status integer,
    last_error      text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    delivered_at    timestamptz
);

-- The worker's read: what is due, and nothing else.
CREATE INDEX webhook_deliveries_due
    ON oxsum.webhook_deliveries (next_attempt_at)
    WHERE status = 'pending';
