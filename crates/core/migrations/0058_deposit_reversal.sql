-- Deposit reversal (issue #356, decision D12): the ledger half of a refund.
-- Reversing a credited deposit posts one entry — Wallet debited, Cash
-- credited — and the row flips to `reversed`, naming the entry that reversed
-- it and when. Both are nullable: a deposit that never reversed carries
-- neither, and `entry_id` keeps naming the credit it booked.
--
-- The reversal's ledger key is derived from the deposit id, so the pair here
-- is the lookup the replay answers rather than a second write. The PSP call
-- that returns the customer's money is a separate issue — no column here
-- tracks it; the books are right before it runs.

ALTER TABLE oxsum.deposits
    ADD COLUMN reversal_entry_id uuid,
    ADD COLUMN reversed_at      timestamptz;
