-- The Solana rail's sanctions park (issue #1144): a transfer that honors the
-- intent's terms but whose payer hits a screening source never credits — the
-- row walks `pending → flagged` and waits on the operator's review queue
-- rather than booking unscreened money or auto-refunding (deny-until-cleared:
-- a screening source that cannot answer leaves the row `pending`, never
-- `flagged`). The deposit `status` CHECK widens to admit 'flagged' on the
-- base status set; the partial index on flagged solana rows keeps the queue
-- lookup off the deposits walk the per-organization history reads.
--
-- Numbering/hand-off (issue #1142): the exception-queue work owns the wider
-- exception status set (`underpaid`, `overpaid`, `late`, `refund_pending`,
-- `refunded` — and it reserves `flagged` for this feature). This migration
-- adds `flagged` alone on the statuses this base knows; #1142's own
-- migration widens the same CHECK to its full set, `flagged` included, when
-- it lands. Whichever PR merges second rebases and renumbers so the CHECK
-- the later migration writes keeps every earlier status — the standing
-- rule for two migrations that touch one CHECK.

ALTER TABLE oxsum.deposits
    DROP CONSTRAINT deposits_status_check,
    ADD CONSTRAINT deposits_status_check
        CHECK (status IN ('pending', 'confirmed', 'credited', 'reversed', 'expired', 'flagged'));

CREATE INDEX deposits_solana_flagged
    ON oxsum.deposits (created_at DESC, deposit_id DESC)
    WHERE rail = 'solana' AND status = 'flagged';
