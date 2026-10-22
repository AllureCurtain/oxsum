-- The per-customer usage day log (issue #337, the design in docs/decisions.md
-- 2026-10-22): one row per (tenant, endUser, UTC day) with usage, holding the
-- day's settlement count, the Merkle root over that day's settlement content
-- hashes in log order, and the day leaf the completeness proof commits.
--
-- The root is derived from the ledger and `usage_records` rather than appended
-- incrementally, so the row can never drift from the rows it claims: a rebuild
-- from the same settled rows produces the same root. A day with no settled
-- usage has no row; the report's contiguity check proves a day was not skipped
-- because every day with usage has one.
--
-- The leaf domain is `oxsum/usage-day/v1`; `leaf_hash = H(domain, day ||
-- count || rows_root)`, computed by the writer (a bytea here, not generated, so
-- the definition lives in one place per language).

CREATE TABLE IF NOT EXISTS oxsum.usage_day_log (
    tenant_id   text        NOT NULL,
    end_user    text        NOT NULL,
    day         date        NOT NULL,
    entry_count bigint      NOT NULL CHECK (entry_count > 0),
    rows_root   bytea       NOT NULL CHECK (octet_length(rows_root) = 32),
    leaf_hash   bytea       NOT NULL CHECK (octet_length(leaf_hash) = 32),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, end_user, day)
);

-- The anchor pass (stage 2) reads every customer's latest day, newest first.
CREATE INDEX IF NOT EXISTS usage_day_log_tenant_day_idx
    ON oxsum.usage_day_log (tenant_id, day DESC);
