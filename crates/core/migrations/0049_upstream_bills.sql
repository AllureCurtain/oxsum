-- Upstream provider bill reconciliation (issue #283): the cost a provider
-- actually billed, recorded beside the estimate the ledger kept, one row per
-- (channel, bill_date). `upstream_cost_minor` on `usage_records` is oxsum's own
-- `Price::upstream_cost` recomputation — an estimate the provider can disprove;
-- this table is what a `fetch_daily_cost` answer disproved or confirmed.
--
-- Grain: the provider cost APIs (OpenAI Costs, Anthropic Usage & Cost Admin)
-- expose (project/key, day)-grained billed totals, never per-request, so the
-- reconciliation grain is (channel, day) — one row per channel per UTC date the
-- sync pass pulled. `our_cost_minor` is the same day's Σ `upstream_cost_minor`
-- summed at write time so a later pass does not recompute it.
--
-- `drift_minor`/`drift_pct` are the answer the margin view reads: provider minus
-- ours (positive means the provider billed more than the estimate priced),
-- and the percentage over the estimate. `reconciliation_entry_id` is NULL until
-- the drift crosses the alert threshold and the pass files a drift entry —
-- it is the row's "this one was flagged" latch, so a re-run cannot double-count.

CREATE TABLE oxsum.upstream_bills (
    channel_id      uuid        NOT NULL REFERENCES oxsum.channels (channel_id),
    bill_date       date        NOT NULL,
    -- The provider-billed total for this channel on this date, minor units.
    -- NULL only on a row the pass could not resolve a provider for.
    provider_billed_minor bigint,
    -- Σ usage_records.upstream_cost_minor for this channel on this date —
    -- our estimate, frozen at write time.
    our_cost_minor  bigint      NOT NULL,
    -- provider_billed_minor - our_cost_minor (NULL when the provider side is).
    drift_minor     bigint,
    -- drift_minor / our_cost_minor * 100, NULL when either side is missing or
    -- our cost is zero.
    drift_pct       numeric,
    -- The reconcile() entry this row's drift filed, once it crossed the
    -- alert threshold; NULL while the drift is under it.
    reconciliation_entry_id uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (channel_id, bill_date)
);

-- A channel's admin billing credential, sealed under OXSUM_SECRET_KEY beside
-- `api_key_sealed` (the metering credential). Separable on purpose: a billing
-- key reads the provider's cost report, a metering key serves the relay —
-- conflating them would hand every relay the power to read the org's bill.
ALTER TABLE oxsum.channels
    ADD COLUMN bill_key_sealed text,
    ADD COLUMN bill_key_last4  text;
