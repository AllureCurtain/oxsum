-- Request analytics (issue #505): one row per gateway request, the
-- operational record a billing dispute or an SRE query actually reads —
-- "every request this key made to gpt-4o-mini yesterday 14:00–15:00, with
-- token counts and latency". `usage_records` is the settlement's accounting
-- fact; this table is the search surface over it, so it also logs the turns
-- admission refused before they could ever settle.
--
-- The row writes *after* the turn settles, off the settle path's success: a
-- logging failure is traced and dropped, never a failed request — which is
-- why it is a separate post-settlement insert, not a column on
-- `record_usage_in`'s transaction. `ON CONFLICT DO NOTHING` keeps a replay
-- (the sweeper's settle, a retried write) from standing a second row.
CREATE TABLE IF NOT EXISTS oxsum.request_log (
    -- `x-oxsum-request-id`: joins the row to the settlement entry and its
    -- proof bundle the way `usage_records.request_id` does.
    request_id          text        NOT NULL,
    -- The organization's ledger tenant id — the same key `usage_records`
    -- partitions on, kept beside `organization_id` so the ledger-scoped reads
    -- and the org-scoped member reads both stay index-only.
    tenant_id           text        NOT NULL,
    -- The org uuid the member endpoint scopes to: a member names their org,
    -- never a raw ledger tenant.
    organization_id     uuid        NOT NULL,
    -- The key that paid, where the hold attributed one.
    key_id              uuid,
    -- The model the caller asked for and the model that answered: equal until
    -- a cross-model failover rewrote the served name (issue #500).
    model_requested     text        NOT NULL,
    model_served        text        NOT NULL,
    channel             text        NOT NULL,
    -- `admitted` when the turn reached a hold, `refused` when admission turned
    -- it away — a refused turn still logs so the surface answers "which
    -- requests were turned away" without a ledger join.
    admission_outcome   text        NOT NULL,
    -- Milliseconds to the first upstream byte: NULL on a non-streamed turn
    -- that reads its body whole, and on a refusal that never reached upstream.
    ttft_ms             integer,
    -- Milliseconds admission→settle: the turn's whole wall clock.
    total_ms            integer,
    input_tokens        integer     NOT NULL DEFAULT 0,
    output_tokens       integer     NOT NULL DEFAULT 0,
    cached_tokens       integer     NOT NULL DEFAULT 0,
    charge_minor        bigint      NOT NULL DEFAULT 0,
    -- Upstream calls under the one hold: >1 means it failed over between
    -- channels, or a cross-model fallback served it.
    upstream_attempts   integer     NOT NULL DEFAULT 1,
    -- `settled` | `errored` | `refused` — mirrors `usage_records.status`.
    status              text        NOT NULL,
    -- The settlement kind for a non-settled turn, or the failure reason —
    -- the terminal cause the row shows without a second table.
    error_kind          text,
    -- The gateway's own answer status when it served one.
    http_status         integer,
    created_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (request_id, tenant_id)
);
-- The three orderings the endpoint filters on: a key's recent requests, a
-- model's recent requests, and the org's whole recent stream. Each is
-- tenant-scoped first so a member's read never leaves their own rows.
CREATE INDEX IF NOT EXISTS request_log_key_time
    ON oxsum.request_log (tenant_id, key_id, created_at DESC);
CREATE INDEX IF NOT EXISTS request_log_model_time
    ON oxsum.request_log (tenant_id, model_served, created_at DESC);
CREATE INDEX IF NOT EXISTS request_log_time
    ON oxsum.request_log (tenant_id, created_at DESC);
