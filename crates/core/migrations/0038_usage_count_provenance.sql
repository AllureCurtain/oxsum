-- #230 — provenance columns for the usage record's counts.
--
-- A settled turn's `input_tokens`/`output_tokens` say what was billed but not
-- who counted it: upstream's own usage report, or oxsum's local estimate when
-- upstream reported nothing (the degraded policy) or the client left before a
-- report could land. The distinction matters at reconciliation — a locally-
-- derived count is the row to recheck against the provider, and until now it
-- could only be inferred from `kind`, which says *that* an estimate happened,
-- not *which* columns it produced.
--
-- The columns pair by provenance. A provider-reported turn writes its report's
-- counts under `provider_*` and leaves `local_*` NULL; a locally-derived turn
-- the reverse. Under the degraded policy only the input side is ever counted,
-- so `local_output_tokens` stays NULL where no count exists — NULL means
-- "not counted", never a silently-zero bill. A refusal or a sweep, which
-- counts nothing, leaves all four NULL.

ALTER TABLE oxsum.usage_records
    ADD COLUMN provider_input_tokens  bigint,
    ADD COLUMN provider_output_tokens bigint,
    ADD COLUMN local_input_tokens     bigint,
    ADD COLUMN local_output_tokens    bigint;

COMMENT ON COLUMN oxsum.usage_records.provider_input_tokens IS
    'input tokens as upstream reported them; NULL when no report was made';
COMMENT ON COLUMN oxsum.usage_records.provider_output_tokens IS
    'output tokens as upstream reported them; NULL when no report was made';
COMMENT ON COLUMN oxsum.usage_records.local_input_tokens IS
    'input tokens counted locally (tiktoken estimate or a token-array request''s exact count); NULL when upstream reported';
COMMENT ON COLUMN oxsum.usage_records.local_output_tokens IS
    'output tokens counted locally (forwarded-prefix estimate); NULL when upstream reported, and NULL on a degraded settle, where output is never counted';
