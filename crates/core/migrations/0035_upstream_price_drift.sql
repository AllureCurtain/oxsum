-- #222 — upstream price drift as open-drift state.
--
-- The scheduled SYNC_UPSTREAM_PRICES job probes each channel's live catalog and
-- diffs every model's `upstream` rates against the rates oxsum stores. Each
-- divergent (channel, model, field) lands one row here — not an event per probe,
-- but the *current* divergence, upserted on every detection and resolved the
-- moment the stored and live rates agree again. Reconciliation therefore reports
-- a stable count of outstanding drift rather than an ever-growing probe log,
-- and a rate that converges clears itself without an operator touching a queue.
--
-- One row per (channel, model, field) while the drift is open: the partial
-- unique index keeps a second detection from double-counting the same open
-- drift, and resolving writes `resolved_at` so the row stays as history without
-- holding the unique slot.

CREATE TABLE oxsum.upstream_price_drift (
    drift_id     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    channel      text NOT NULL,
    model        text NOT NULL,
    -- The UpstreamPrices wire name that diverges, e.g. "inputPricePerMillion".
    field        text NOT NULL,
    -- The rate oxsum stores for the model's current price version, or NULL when
    -- the stored price has no rate on this dimension (the field was absent).
    stored_minor bigint,
    -- The rate the live catalog answers for the same model, or NULL when the
    -- live catalog prices the model but carries no rate on this dimension.
    live_minor   bigint,
    detected_at  timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    resolved_at  timestamptz
);

-- An open drift per (channel, model, field): resolved rows step out of the way.
CREATE UNIQUE INDEX upstream_price_drift_open
    ON oxsum.upstream_price_drift (channel, model, field)
    WHERE resolved_at IS NULL;

-- How one channel's probe ended for a run: reached and diffed, or unreachable /
-- not an oxsum-compatible catalog. Unprobeable is a recorded outcome, never a
-- job failure — drift is detected, never inferred from silence.
CREATE TABLE oxsum.upstream_price_probes (
    probe_id     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    channel      text NOT NULL,
    probed_at    timestamptz NOT NULL DEFAULT now(),
    -- `ok`, `unreachable`, `invalid` (a response that was not an
    -- oxsum-compatible catalog) or `auth` (a credential or 401/403 problem).
    outcome      text NOT NULL,
    detail       text NOT NULL DEFAULT ''
);
