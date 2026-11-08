-- Retain every comparison attempt; the old run stays a latest-status pointer.
ALTER TABLE oxsum.reconciliation_runs ADD COLUMN time_basis text NOT NULL DEFAULT 'booking'
    CHECK (time_basis IN ('booking','service','payment'));
ALTER TABLE oxsum.reconciliation_runs ADD COLUMN latest_snapshot_id uuid;
ALTER TABLE oxsum.reconciliation_runs ADD COLUMN coverage jsonb NOT NULL DEFAULT '{"version":0,"complete":false,"comparedFields":[],"notCompared":["legacy projection-only run"],"organizations":0,"sources":0,"verifiedSources":0,"entries":0,"verifiedEntries":0,"basis":"Legacy retained values are not independent ledger evidence","limits":{}}';
DROP INDEX oxsum.reconciliation_runs_period;
CREATE UNIQUE INDEX reconciliation_runs_period ON oxsum.reconciliation_runs(kind,period_start,period_end,time_basis);
CREATE TABLE oxsum.reconciliation_snapshots (
    snapshot_id uuid PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES oxsum.reconciliation_runs(run_id),
    state text NOT NULL CHECK (state IN ('matched','exceptions','legacy')),
    expected_totals jsonb NOT NULL,
    actual_totals jsonb NOT NULL,
    coverage jsonb NOT NULL,
    exception_count bigint NOT NULL CHECK (exception_count>=0),
    evidence jsonb NOT NULL,
    captured_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX reconciliation_snapshots_run ON oxsum.reconciliation_snapshots(run_id,captured_at,snapshot_id);
-- The run ID is also the deterministic first snapshot ID for retained old runs.
INSERT INTO oxsum.reconciliation_snapshots(snapshot_id,run_id,state,expected_totals,actual_totals,coverage,exception_count,evidence,captured_at)
SELECT run_id,run_id,'legacy',expected_totals,actual_totals,coverage,exception_count,
    jsonb_build_object('legacyState',state,'lastError',last_error),COALESCE(finished_at,started_at)
FROM oxsum.reconciliation_runs;
UPDATE oxsum.reconciliation_runs SET latest_snapshot_id=run_id;
CREATE TRIGGER reconciliation_snapshots_immutable BEFORE UPDATE OR DELETE ON oxsum.reconciliation_snapshots
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
ALTER TABLE oxsum.reconciliation_runs ADD CONSTRAINT reconciliation_latest_snapshot
    FOREIGN KEY (latest_snapshot_id) REFERENCES oxsum.reconciliation_snapshots(snapshot_id);
ALTER TABLE oxsum.reconciliation_exceptions ADD COLUMN fact text NOT NULL DEFAULT 'legacy';
DROP INDEX oxsum.reconciliation_exceptions_source_open;
CREATE UNIQUE INDEX reconciliation_exceptions_source_open ON oxsum.reconciliation_exceptions(organization_id,kind,source_table,source_ref,fact) NULLS NOT DISTINCT
    WHERE state='open' AND source_ref IS NOT NULL;

CREATE TABLE oxsum.reconciliation_requests (
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    idempotency_key text NOT NULL,
    period_start date NOT NULL,
    period_end date NOT NULL,
    time_basis text NOT NULL CHECK (time_basis IN ('booking','service','payment')),
    run_id uuid NOT NULL REFERENCES oxsum.reconciliation_runs(run_id),
    snapshot_id uuid NOT NULL REFERENCES oxsum.reconciliation_snapshots(snapshot_id),
    audit_ref uuid NOT NULL REFERENCES oxsum.finance_audit(audit_id),
    result jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (business_id,idempotency_key)
);
CREATE TRIGGER reconciliation_requests_immutable BEFORE UPDATE OR DELETE ON oxsum.reconciliation_requests
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
