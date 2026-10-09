-- The metering API (issue #172, roadmap P8-4): a metered event's usage row
-- records `upstream_attempts = 0` — nothing was relayed — where a gateway turn
-- always made at least the one call. The check relaxes to admit zero; the
-- writer, not the table, decides which count is honest.

ALTER TABLE oxsum.usage_records
    DROP CONSTRAINT usage_records_upstream_attempts_check,
    ADD CONSTRAINT usage_records_upstream_attempts_check
        CHECK (upstream_attempts >= 0);
