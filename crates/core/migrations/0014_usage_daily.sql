-- Daily usage rollup (roadmap P3-3, issue #126): one row per
-- (tenant, day, key, channel, model) — the table the usage dashboard reads so it
-- does not scan `usage_records` row by row.
--
-- `day` is the settlement entry's `booking_date` — the ledger's date, not the
-- instant the usage row was written — so the aggregate answers the same day a
-- statement period would count the turn under. `key_id` rides along so the
-- members' scope rule (own keys plus the unattributed shared rows) filters
-- losslessly; `NULLS NOT DISTINCT` keeps unattributed usage aggregating under one
-- row.
--
-- The rollup is maintained transactionally by `record_usage`: the upsert runs in
-- the same transaction as the usage insert, and only when the insert actually
-- landed — a replayed write aggregates nothing twice.

CREATE TABLE oxsum.usage_daily (
    tenant_id        text    NOT NULL,
    day              date    NOT NULL,
    key_id           uuid,
    channel          text    NOT NULL,
    model            text    NOT NULL,
    turns            bigint  NOT NULL DEFAULT 0 CHECK (turns >= 0),
    input_tokens     bigint  NOT NULL DEFAULT 0,
    output_tokens    bigint  NOT NULL DEFAULT 0,
    cached_tokens    bigint  NOT NULL DEFAULT 0,
    reasoning_tokens bigint  NOT NULL DEFAULT 0,
    charged_minor    bigint  NOT NULL DEFAULT 0
);

-- The rollup key keeps `key_id` nullable so unattributed usage (session settles)
-- aggregates under one row too — a NULLS NOT DISTINCT index, since plain unique
-- indexes treat NULLs as distinct.
CREATE UNIQUE INDEX usage_daily_key ON oxsum.usage_daily
    (tenant_id, day, channel, model, key_id) NULLS NOT DISTINCT;

CREATE INDEX usage_daily_tenant_day ON oxsum.usage_daily (tenant_id, day);

-- Backfill: every usage row already recorded is re-aggregated from its entry's
-- booking date. Ledgers live one schema per tenant, so the join is written per
-- tenant that has rows.
DO $$
DECLARE
    tenant text;
BEGIN
    FOR tenant IN SELECT DISTINCT tenant_id FROM oxsum.usage_records LOOP
        -- A usage row may name a tenant whose ledger schema was never created
        -- (or was since dropped): skip it rather than fail the migration.
        IF to_regclass('ledger_' || tenant || '.entries') IS NULL THEN
            CONTINUE;
        END IF;
        EXECUTE format(
            'INSERT INTO oxsum.usage_daily
                 (tenant_id, day, key_id, channel, model, turns,
                  input_tokens, output_tokens, cached_tokens, reasoning_tokens,
                  charged_minor)
             SELECT u.tenant_id, e.booking_date, u.key_id, u.channel, u.model,
                    count(*), sum(u.input_tokens), sum(u.output_tokens),
                    sum(u.cached_tokens), sum(u.reasoning_tokens),
                    sum(u.charged_minor)
             FROM oxsum.usage_records u
             JOIN %I.entries e ON e.entry_id = u.entry_id
             WHERE u.tenant_id = $1
             GROUP BY u.tenant_id, e.booking_date, u.key_id, u.channel, u.model',
            'ledger_' || tenant
        )
        USING tenant;
    END LOOP;
END $$;
