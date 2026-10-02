-- Per-key spend limits (issue #32): the most one key may have committed — settled
-- charges plus outstanding holds — in minor units. NULL is unlimited, so existing keys
-- are unaffected; 0 means the key can never hold. Checked and enforced by
-- `Wallet::hold_for_key`, not by the database: the total is an aggregate over ledger
-- postings attributed to the key, not a column a CHECK could see.
ALTER TABLE oxsum.api_keys
    ADD COLUMN spend_limit_minor bigint
    CHECK (spend_limit_minor IS NULL OR spend_limit_minor >= 0);
