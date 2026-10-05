-- The normalized usage record of every settled turn (issue #102, roadmap P1-1).
--
-- The ledger entry is the bill and stays the source of truth for money; this table holds what an
-- immutable entry description deliberately does not: the full usage dimensions pricing looks at,
-- the caller's attribution (`end_user`, `tags`, `service_tier`), and `usage_details.provider_raw`
-- (upstream's usage object verbatim). All three are mutable-store data — attribution is
-- pseudonymized when an organization is deleted, and `provider_raw` is nulled after the retention
-- window because it may carry prompt fragments; neither could live inside a hashed ledger entry.
--
-- One row per settled turn, written by the path whose settlement landed (the turn, or the sweeper
-- for a timed-out hold). `request_id` is the primary key: a second writer can only be the same
-- turn replaying and is ignored (`ON CONFLICT DO NOTHING`), so the row is idempotent like the
-- settlement it describes.

CREATE TABLE oxsum.usage_records (
    -- From `x-oxsum-request-id`: what the caller quotes when asking about the bill.
    request_id              text        PRIMARY KEY,
    -- The organization whose ledger settled the turn, as its ledger tenant id.
    tenant_id               text        NOT NULL,
    -- The key that paid, where the hold attributed one.
    key_id                  uuid,
    -- What the turn was priced by, copied from the settlement record.
    model                   text        NOT NULL,
    channel                 text        NOT NULL,
    price_version           bigint      NOT NULL,
    -- The settlement kind (`usage`, `estimated`, `capped`, `swept`, ...): how the counts
    -- below were arrived at.
    kind                    text        NOT NULL,
    -- The settlement entry's id, derived from the hold's key before the write.
    entry_id                uuid        NOT NULL,
    -- Token-metered dimensions. `input_tokens` includes `cached_tokens`,
    -- `output_tokens` includes `reasoning_tokens` — the normalized invariants every
    -- adapter maps its protocol into.
    input_tokens            bigint      NOT NULL,
    output_tokens           bigint      NOT NULL,
    cached_tokens           bigint      NOT NULL DEFAULT 0,
    cache_write_5m_tokens   bigint      NOT NULL DEFAULT 0,
    cache_write_1h_tokens   bigint      NOT NULL DEFAULT 0,
    reasoning_tokens        bigint      NOT NULL DEFAULT 0,
    tool_calls              bigint      NOT NULL DEFAULT 0,
    image_input_tokens      bigint      NOT NULL DEFAULT 0,
    audio_input_tokens      bigint      NOT NULL DEFAULT 0,
    video_input_tokens      bigint      NOT NULL DEFAULT 0,
    image_output_tokens     bigint      NOT NULL DEFAULT 0,
    audio_output_tokens     bigint      NOT NULL DEFAULT 0,
    -- Context: the caller's service tier slot, and the event type (NULL for a native
    -- gateway turn; external metering events name theirs).
    service_tier            text,
    event_type              text,
    -- Caller-supplied attribution, pseudonymized on organization deletion.
    end_user                text,
    tags                    jsonb       NOT NULL DEFAULT '{}',
    -- Provider-specific extras including `provider_raw`, nulled after the retention
    -- window; the normalized columns above stay.
    usage_details           jsonb,
    -- What the settlement charged and what it had frozen, in minor units.
    charged_minor           bigint      NOT NULL,
    freeze_minor            bigint      NOT NULL,
    settled_at              timestamptz NOT NULL DEFAULT now()
);

-- The analytics and usage-API reads: one organization's turns over a window, and the
-- detail-filtered read of one end user's turns.
CREATE INDEX usage_records_tenant_time ON oxsum.usage_records (tenant_id, settled_at DESC);
CREATE INDEX usage_records_end_user ON oxsum.usage_records (tenant_id, end_user)
    WHERE end_user IS NOT NULL;
-- Tag-scoped filtering and grouping (rate limits and analytics take tag axes).
CREATE INDEX usage_records_tags ON oxsum.usage_records USING gin (tags);

-- The sweeper's watch rows carry the caller's attribution too, so a swept settlement
-- writes a usage row that says whose turn it was (issue #102).
ALTER TABLE oxsum.open_holds
    ADD COLUMN key_id       uuid,
    ADD COLUMN end_user     text,
    ADD COLUMN service_tier text,
    ADD COLUMN tags         jsonb NOT NULL DEFAULT '{}';
