-- Distributed request rate limiting (issue #498): the shared-backend
-- `requestsPerMinute` window, for a deployment running more than one oxsum
-- instance behind a load balancer.
--
-- `oxsum.rate_windows` keeps one row per limited key — the same unit the
-- in-process `SlidingWindow` keys its queue on, so the per-key RPM cap and the
-- organization-tier RPM cap (which admits under `organization.id`) share the
-- table. `hits` is the array of server timestamps the trailing minute still
-- counts: `PostgresRateLimiter::admit` reads the row `FOR UPDATE`, drops the
-- entries aged out of the window (`hit > now() - interval '60 seconds'`,
-- the server's own clock so two instances share one timeline), and appends
-- `now()` when room remains — a read-then-write made atomic by the row lock,
-- exactly the serialization the issue asks `SELECT ... FOR UPDATE` for.
--
-- An array, not one row per hit: the window is 60 seconds and bounded by the
-- key's own cap, so the row stays small and the count is `cardinality(hits)`
-- rather than a `count(*)` scan. Rows with an empty window are deleted by the
-- same transaction, so the table stays sized by the keys that were busy inside
-- a minute — a cold key leaves no row behind, matching how `SlidingWindow`
-- drops a drained queue.
--
-- The window is a protection limit, not a billing one: it counts requests
-- admitted, never money, and the ledger stays the source of truth for spend.
-- Choosing it is `OXSUM_RATE_LIMIT_BACKEND=postgres` (default `memory`); the
-- env var picks the backend at boot and the table is created unconditionally so
-- flipping the variable needs no manual migration.

CREATE TABLE IF NOT EXISTS oxsum.rate_windows (
    key_id  uuid        PRIMARY KEY,
    hits    timestamptz[] NOT NULL DEFAULT '{}',
    -- When the newest hit landed; the row lock serializes concurrent admits
    -- on the key, so this also serves as the contention read's anchor.
    updated timestamptz NOT NULL DEFAULT now()
);

-- The aged-hit prune inside admit already bounds the array; this index lets a
-- one-off operator cleanup (`DELETE ... WHERE updated < now() - interval '60
-- seconds'`) find stale rows without scanning busy ones.
CREATE INDEX IF NOT EXISTS rate_windows_updated_idx
    ON oxsum.rate_windows (updated);
