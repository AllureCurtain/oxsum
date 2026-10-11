-- The webhook event catalog (issue #200): every remaining decided event joins
-- `request.settled` and `org.suspended`.
--
-- `balance.low` needs two fields beside the existing ones: the threshold an
-- endpoint watches (`balance_low_minor`, NULL is off) and a latch
-- (`balance_low_sent`) so a balance that stays below the line does not re-fire
-- on every settle — the latch is set when the event enqueues and cleared when a
-- deposit credits the wallet back above the threshold.
ALTER TABLE oxsum.webhook_endpoints
    ADD COLUMN balance_low_minor bigint,
    ADD COLUMN balance_low_sent  boolean NOT NULL DEFAULT FALSE;
