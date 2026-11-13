-- A run's observable outcome (issue #1090). The manual close-month pass
-- writes its latest attempt's progress here as it walks the organizations:
-- how many it walks, how many it has processed, sealed and failed, and the
-- first fifty failures. `GET /api/v1/admin/closings/jobs/{jobId}` answers the
-- console's poll from this column. `last_error` stays an operator-log field
-- that no response reads.
--
-- NULL until a kind that reports progress writes it; the periodic kinds
-- leave it NULL.
ALTER TABLE oxsum.jobs
    ADD COLUMN result jsonb CHECK (result IS NULL OR jsonb_typeof(result) = 'object');
