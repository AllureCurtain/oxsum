-- 0034_statement_anchor: anchor each finalized statement to the transparency log.
--
-- A finalized statement already records the covered log window
-- (`log_from_index`/`log_to_index`), but nothing commits *what the log contained*
-- at finalize time. The anchor fixes that: finalize appends a marker entry to the
-- tenant ledger whose document hash is the window digest over the covered
-- entries' content hashes, and the statement row records the anchor's identity
-- plus the rollup digest over its own lines and totals. A verifier can then
-- recompute both digests and check the anchor's inclusion proof — a truncated or
-- reordered window no longer verifies.
--
-- All four columns are nullable: statements finalized before this migration have
-- no anchor, and a failed anchor write leaves the draft-only columns absent
-- rather than half-written.

ALTER TABLE oxsum.statements
    ADD COLUMN anchor_entry_id uuid,
    ADD COLUMN anchor_log_index bigint,
    ADD COLUMN window_hash text,
    ADD COLUMN rollup_hash text;
