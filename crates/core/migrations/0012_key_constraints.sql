-- Key constraints (issue #120, roadmap P3-1): beside the cumulative spend limit and
-- the existing expiry, a key may carry a periodic budget window and a model allowlist.
--
-- `budget_duration` makes `spend_limit_minor` periodic: committed spend counts the
-- current UTC calendar period's settled charges plus every outstanding hold. The
-- window without a limit is meaningless — enforced in code (both are written together
-- from one request), not by a constraint the column order could make awkward.
--
-- `model_allowlist` is NULL for "every served model" and a non-empty list for "only
-- these"; an empty list would deny the gateway entirely, which a null already says
-- more clearly, so empty lists are refused at write.

ALTER TABLE oxsum.api_keys
    ADD COLUMN budget_duration text
        CHECK (budget_duration IS NULL OR budget_duration IN ('daily', 'weekly', 'monthly')),
    ADD COLUMN model_allowlist text[];
