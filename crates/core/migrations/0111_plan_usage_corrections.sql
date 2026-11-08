-- Explicit correction of historical plan usage that was already settled.
-- A correction never claims a cash refund: excess collection is a payable.
CREATE TABLE oxsum.plan_usage_corrections (
    journal_id uuid PRIMARY KEY REFERENCES oxsum.invoice_journals(journal_id),
    correction_id uuid NOT NULL UNIQUE,
    business_id uuid NOT NULL,
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    plan_period_id uuid NOT NULL REFERENCES oxsum.plan_periods(plan_period_id),
    statement_id uuid NOT NULL REFERENCES oxsum.plan_statements(statement_id),
    original_entry_id uuid NOT NULL,
    entry_id uuid NOT NULL,
    duplicate_minor bigint NOT NULL CHECK (duplicate_minor > 0),
    receivable_minor bigint NOT NULL CHECK (receivable_minor >= 0),
    payable_minor bigint NOT NULL CHECK (payable_minor >= 0),
    report jsonb NOT NULL,
    actor_user_id uuid NOT NULL REFERENCES oxsum.users(user_id),
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 500),
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (business_id, idempotency_key),
    CHECK (duplicate_minor::numeric = receivable_minor::numeric + payable_minor::numeric)
);
CREATE FUNCTION oxsum.protect_plan_usage_correction() RETURNS trigger AS $guard$
BEGIN
    RAISE EXCEPTION 'plan usage correction history is immutable';
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER plan_usage_corrections_protected BEFORE UPDATE OR DELETE
    ON oxsum.plan_usage_corrections FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_usage_correction();

-- Include the original authorizing order even when it allocated nothing, plus
-- every historical payment order. Later cash outcomes must not use a different
-- period's prepayment merely because that order also authorized a renewal.
CREATE TABLE oxsum.plan_usage_correction_sources (
    journal_id uuid NOT NULL REFERENCES oxsum.plan_usage_corrections(journal_id),
    order_id uuid NOT NULL REFERENCES oxsum.payment_orders(order_id),
    PRIMARY KEY (journal_id, order_id)
);
CREATE INDEX plan_usage_correction_sources_order ON oxsum.plan_usage_correction_sources(order_id);
CREATE TRIGGER plan_usage_correction_sources_protected BEFORE UPDATE OR DELETE
    ON oxsum.plan_usage_correction_sources FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_usage_correction();

ALTER TABLE oxsum.finance_audit DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
    'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
    'journal.void', 'statement.release', 'reconciliation.resolve',
    'reconciliation.waive', 'reconciliation.repair', 'plan_refund.recover',
    'plan_usage.correct'));
