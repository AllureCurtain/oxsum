-- The daily usage anchors (issue #338, docs/decisions.md 2026-10-22): one row
-- per organization and closed UTC day, naming the ledger entry that commits the
-- day's sorted-map root over every customer's day-log head.
--
-- The ledger entry is the record; this table is the lookup a report mint and the
-- operator read use to find it without scanning the log by idempotency key. A
-- reconciler can prove the two agree: the entry's `DocumentRef` content hash is
-- `map_root`, and its description names the same day and count.

CREATE TABLE IF NOT EXISTS oxsum.usage_anchors (
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE CASCADE,
    day             date        NOT NULL,
    entry_id        uuid        NOT NULL,
    map_root        bytea       NOT NULL CHECK (octet_length(map_root) = 32),
    enduser_count   bigint      NOT NULL CHECK (enduser_count >= 0),
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, day)
);

-- A report mint looks up the anchor for its organization's recorded days.
CREATE INDEX IF NOT EXISTS usage_anchors_entry_idx
    ON oxsum.usage_anchors (entry_id);
