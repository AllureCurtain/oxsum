-- Avoid walking recent refused/error operational traffic for a first successful receipt.
CREATE INDEX usage_records_ready_receipt
    ON oxsum.usage_records (tenant_id, settled_at DESC, request_id DESC)
    WHERE status='settled' AND kind IN ('usage','estimated','capped') AND failure_reason IS NULL;
