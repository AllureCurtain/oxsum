-- The settled turn's upstream cost (issue #112, roadmap P1-6): what the
-- channel's `upstream` prices made of the same usage, in minor units, beside
-- what the organization was charged. The margin view sums the two; the
-- reconciler compares it against upstream's own bill.
--
-- NULL means untracked, not zero: a price with no `upstream` block, a swept
-- turn whose watch row carries no price, and every row from before the column
-- existed. Zero means the upstream block priced the turn at nothing.

ALTER TABLE oxsum.usage_records
    ADD COLUMN upstream_cost_minor bigint;
