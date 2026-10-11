-- Per-key token rate limiting (issue #203, roadmap P3-4): beside the rolling-minute
-- *request* allowance, a key may carry a rolling-minute *token* allowance — how many
-- tokens its settled turns may burn inside a minute.
--
-- The two limits differ in what counts them. RPM consumes a slot at admission — a
-- refused request spends one anyway, and a restart forgiving the in-process window is
-- the protection semantics' own rule. TPM counts *settled* usage: billing-adjacent,
-- permanent, and never advanced by a hold. So the window is durable, written by the
-- same transaction that records the usage row, where a replay lands nothing twice.
--
-- `oxsum.tpm_windows` buckets settled tokens by the second they landed in. A request
-- is admitted only when the tokens settled inside the trailing minute, plus this
-- request's own estimate, fit under the cap — "window-actual plus this estimate"
-- (docs/decisions.md). Holds never occupy the window; only a settle moves it.
-- One-second buckets keep the minute sliding rather than quantized to wall-clock
-- minute boundaries; rows older than the window are pruned as each write lands.

ALTER TABLE oxsum.api_keys
    ADD COLUMN tokens_per_minute integer
        CHECK (tokens_per_minute IS NULL OR tokens_per_minute > 0);

CREATE TABLE oxsum.tpm_windows (
    key_id       uuid        NOT NULL,
    window_start timestamptz NOT NULL,
    tokens       bigint      NOT NULL CHECK (tokens >= 0),
    PRIMARY KEY (key_id, window_start)
);

-- The trailing-minute sum a request checks runs one index probe over the primary
-- key's leading column; no extra index is needed for a per-key read this small.
