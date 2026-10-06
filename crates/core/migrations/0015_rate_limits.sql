-- Per-key rate limiting (issue #130, roadmap P3-4): beside the spend
-- constraints and the allowlist, a key may carry a rolling-minute request
-- allowance and a cap on how many holds it may have outstanding at once.
--
-- `requests_per_minute` is consumed at hold creation by the in-process
-- sliding-window limiter (docs/decisions.md: RPM consumes at admission,
-- Redis is a drop-in backend later). `max_concurrent_holds` is counted from
-- the ledger's own pending entries under the per-key advisory lock, beside
-- the spend-limit check. NULL lifts the constraint, as with the others.

ALTER TABLE oxsum.api_keys
    ADD COLUMN requests_per_minute integer
        CHECK (requests_per_minute IS NULL OR requests_per_minute > 0),
    ADD COLUMN max_concurrent_holds integer
        CHECK (max_concurrent_holds IS NULL OR max_concurrent_holds > 0);
