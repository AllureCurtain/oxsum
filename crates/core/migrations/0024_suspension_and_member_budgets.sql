-- Issue #162 (roadmap P7-3): organization suspension and member budgets.
--
-- `organizations.suspended_at` stamps when the platform suspended the
-- organization's spend — NULL means it runs normally. A suspended
-- organization's new holds refuse while its in-flight holds still settle,
-- and money-in still lands so the credit line can be repaid.
ALTER TABLE oxsum.organizations
    ADD COLUMN suspended_at timestamptz;

-- `memberships.budget_limit_minor` caps one member's committed spend —
-- settled charges plus outstanding holds summed over every key they minted
-- (`api_keys.created_by`). NULL carries no cap; 0 caps it at nothing, like a
-- key's `spend_limit_minor` of 0.
ALTER TABLE oxsum.memberships
    ADD COLUMN budget_limit_minor bigint CHECK (budget_limit_minor >= 0);
