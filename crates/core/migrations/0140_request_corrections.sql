-- Immutable append-only corrections bound to verified original ledger sources.
CREATE TABLE oxsum.request_corrections (
    correction_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    source_entry_id uuid NOT NULL,
    source_hash text NOT NULL,
    entry_id uuid NOT NULL,
    content_hash text NOT NULL,
    amount_minor bigint NOT NULL CHECK (amount_minor > 0),
    reason text NOT NULL,
    actor_id uuid NOT NULL REFERENCES oxsum.users(user_id),
    booked_on date NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, entry_id)
);
CREATE INDEX request_corrections_source ON oxsum.request_corrections(organization_id, source_entry_id);
CREATE TRIGGER request_corrections_immutable BEFORE UPDATE OR DELETE ON oxsum.request_corrections
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

ALTER TABLE oxsum.finance_audit DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
    'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
    'journal.void', 'statement.release', 'reconciliation.resolve',
    'reconciliation.waive', 'reconciliation.repair', 'plan_refund.recover',
    'plan_usage.correct', 'nonplan_refund.recover', 'nonplan_chargeback.recover',
    'statement.refund', 'request.correct'));
