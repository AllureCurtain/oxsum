-- Exact, append-only recovery of audited legacy DrCash/CrCreditLine losses.
-- Cash correction cancels the false receipt B and records the actual loss L.
CREATE TABLE oxsum.nonplan_chargeback_corrections (
    dispute_id uuid PRIMARY KEY REFERENCES oxsum.disputes(dispute_id),
    order_id uuid NOT NULL REFERENCES oxsum.payment_orders(order_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    event_receipt_id uuid NOT NULL REFERENCES oxsum.merchant_event_inbox(receipt_id),
    original_audit_id uuid NOT NULL REFERENCES oxsum.finance_audit(audit_id),
    false_receipt_minor bigint NOT NULL CHECK (false_receipt_minor > 0),
    actual_loss_minor bigint NOT NULL CHECK (actual_loss_minor > 0),
    cash_correction_minor bigint NOT NULL CHECK (cash_correction_minor > 0),
    wallet_minor bigint NOT NULL CHECK (wallet_minor >= 0),
    suspense_minor bigint NOT NULL CHECK (suspense_minor > 0),
    original_entry_id uuid NOT NULL,
    original_entry_hash text NOT NULL CHECK (original_entry_hash ~ '^[0-9a-f]{64}$'),
    correction_entry_id uuid NOT NULL,
    correction_entry_hash text NOT NULL CHECK (correction_entry_hash ~ '^[0-9a-f]{64}$'),
    correction_booking_date date NOT NULL,
    source_revision text NOT NULL CHECK (source_revision ~ '^[0-9a-f]{64}$'),
    source jsonb NOT NULL,
    effect jsonb NOT NULL,
    booked_at timestamptz NOT NULL DEFAULT now(),
    CHECK (false_receipt_minor <= actual_loss_minor),
    CHECK (wallet_minor <= actual_loss_minor),
    CHECK (cash_correction_minor::numeric = false_receipt_minor::numeric + actual_loss_minor::numeric),
    CHECK (cash_correction_minor::numeric = wallet_minor::numeric + suspense_minor::numeric)
);
CREATE INDEX nonplan_chargeback_corrections_order
    ON oxsum.nonplan_chargeback_corrections(order_id);
CREATE TRIGGER nonplan_chargeback_corrections_immutable BEFORE UPDATE OR DELETE
    ON oxsum.nonplan_chargeback_corrections FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

ALTER TABLE oxsum.finance_audit DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
    'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
    'journal.void', 'statement.release', 'reconciliation.resolve',
    'reconciliation.waive', 'reconciliation.repair', 'plan_refund.recover',
    'plan_usage.correct', 'nonplan_refund.recover', 'nonplan_chargeback.recover'));
