-- The itemized price book (issue #108, roadmap P1-4): one price version grows from the
-- two-component input/output price to the full set a settlement can decompose into.
--
-- The new token dimensions are all optional and all per million units, in the same minor
-- units as the existing prices. A dimension without a price is not free: it bills at the
-- base rate of its side — cached, cache-write and reasoning counts fold into the `input` /
-- `output` line, so a price book that ignores a provider's discount dimensions still bills
-- the full totals.
--
-- `rules` and `upstream_prices` are JSONB rather than columns: a rule is a whole spare
-- price set plus its match, and the upstream set mirrors the token dimensions without the
-- columns that would double the row. Both are part of the version, so the append-only
-- trigger covers them like the rest — changing a rule is a new version, not an edit.

ALTER TABLE oxsum.channel_prices
    -- Minor units per million cached input tokens; NULL bills the cached part at the input price.
    ADD COLUMN cache_read_price_per_million    bigint CHECK (cache_read_price_per_million >= 0),
    -- Minor units per million tokens written into the provider's 5-minute cache tier.
    ADD COLUMN cache_write_5m_price_per_million bigint CHECK (cache_write_5m_price_per_million >= 0),
    -- As above, for the 1-hour cache tier.
    ADD COLUMN cache_write_1h_price_per_million bigint CHECK (cache_write_1h_price_per_million >= 0),
    -- Minor units per million reasoning tokens; NULL bills the reasoning part at the output price.
    ADD COLUMN reasoning_price_per_million     bigint CHECK (reasoning_price_per_million >= 0),
    -- A flat amount in minor units, charged once per billed request in addition to the token lines.
    ADD COLUMN cost_per_request                bigint CHECK (cost_per_request >= 0),
    -- The billing mode the price applies to: only 'chat' is priced today; the column
    -- reserves embedding/image and friends for when an adapter meters them.
    ADD COLUMN mode                            text NOT NULL DEFAULT 'chat',
    -- What the upstream bills the deployment for the same usage, same units: a sparse object
    -- naming only the dimensions that differ from the input/output pair. The margin and
    -- misbilling view reads it (roadmap P1-6); it never reaches an organization's bill.
    ADD COLUMN upstream_prices                 jsonb,
    -- Conditional price sets, a JSON array of {"match": {...}, "price": {...}}: a matched
    -- rule replaces the whole base set, most-specific-wins, and same-specificity overlaps
    -- are refused when the version is written (docs/decisions.md, "Pricing").
    ADD COLUMN rules                           jsonb;
