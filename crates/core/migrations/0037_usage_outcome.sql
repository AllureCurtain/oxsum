-- #233 — terminal status and failure reason on billing records.
--
-- A gateway request that was refused (rate limit, insufficient funds, model not
-- served) or that errored after the hold (upstream_error, upstream_unreachable)
-- used to leave only a server log line. Every settled turn already writes a
-- usage row; the row now also names how the turn ended — `settled`, `errored`
-- or `refused` — and, for the turns that carry one, the reason it ended that
-- way. A refused turn's row rides the refusal marker entry the gateway appends
-- beside it, so `entry_id` stays a real ledger id and the reconciler sees no
-- orphan.

ALTER TABLE oxsum.usage_records
    ADD COLUMN status         text NOT NULL DEFAULT 'settled',
    ADD COLUMN failure_reason text;

COMMENT ON COLUMN oxsum.usage_records.status IS
    'how the turn ended: settled (charged or settled cleanly), errored (upstream_error/unreachable/swept), refused (admission refused, no hold taken)';
COMMENT ON COLUMN oxsum.usage_records.failure_reason IS
    'why the turn refused or errored, NULL for a clean settle';
