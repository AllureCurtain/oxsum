-- Per-request cost basis (issue #604, roadmap T13): how the settled turn's
-- `upstream_cost_minor` figure was derived, so a cost-allocation pass never has
-- to infer it from `kind` and a NULL `upstream_cost_minor` is never read as a
-- zero the platform really paid nothing for.
--
-- `upstream_cost_minor` already separates "untracked" (NULL) from "priced at
-- nothing" (0). What it cannot say is *why*: a real upstream attempt's report,
-- a cache hit that made no upstream call at all, a local estimate the degraded
-- or cancelled settle priced from, or a figure reconciled later against the
-- provider's own bill. `cost_basis` names that provenance:
--
--   real_attempt        a real upstream attempt reported usage and the price's
--                       `upstream` block priced it. `upstream_cost_minor` is
--                       Some — including on a failed-but-billed attempt upstream
--                       still charged for (partial bytes, a timeout after
--                       accept); the platform's cost is never fabricated to zero.
--   cache_reuse         a semantic-cache hit: no upstream call ran this turn, so
--                       `upstream_attempts` is 0 and the reused answer's cost was
--                       already paid once by the turn that produced it.
--   estimate            the priced usage came from a local estimate — the
--                       degraded policy, or a client-cancel estimate — not an
--                       upstream report.
--   missing             `upstream_cost_minor` is NULL: the price carried no
--                       `upstream` block, a swept turn had no price to price
--                       from, or the row predates the column. The platform's
--                       cost is untracked, not zero.
--   provider_reconciled reserved: a later pass reconciled this row's figure
--                       against the provider's own bill (oxsum.upstream_bills,
--                       which reconciles at (channel, day) grain). No writer sets
--                       it yet; the value reserves the spelling so an allocation
--                       backfill never has to invent it.
--
-- NULL on rows written before the column existed — the same "untracked" the
-- `upstream_cost_minor` NULL already means, so a historical row reads as one
-- figure, not two columns disagreeing.

ALTER TABLE oxsum.usage_records
    ADD COLUMN cost_basis text
    CHECK (cost_basis IS NULL OR cost_basis IN
        ('real_attempt', 'cache_reuse', 'estimate', 'missing', 'provider_reconciled'));

COMMENT ON COLUMN oxsum.usage_records.cost_basis IS
    'how upstream_cost_minor was derived: real_attempt (a reported-usage attempt priced it), cache_reuse (a cache hit made no upstream call), estimate (a local estimate priced it), missing (untracked — no upstream block or a swept turn), provider_reconciled (reconciled against the provider''s bill). NULL on pre-column rows.';
