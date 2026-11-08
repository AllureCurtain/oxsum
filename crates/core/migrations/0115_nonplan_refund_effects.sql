-- Actual verified provider cash is independent of immutable approval state.
-- Historical restitution has no Cash leg and never rewrites the old note.
-- Old refund writers could commit an orphan Cash leg. New atomic-writer rows
-- can prove absence of an effect under the shared source lock; old rows cannot.
ALTER TABLE oxsum.refunds ADD COLUMN cash_policy_version smallint NOT NULL DEFAULT 0
    CHECK (cash_policy_version IN (0,1));
CREATE FUNCTION oxsum.protect_refund_cash_policy() RETURNS trigger AS $guard$
BEGIN
    IF NEW.cash_policy_version IS DISTINCT FROM OLD.cash_policy_version THEN
        RAISE EXCEPTION 'refund cash policy identity is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER refund_cash_policy_immutable BEFORE UPDATE ON oxsum.refunds
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_refund_cash_policy();

CREATE TABLE oxsum.nonplan_refund_effects (
    refund_id uuid PRIMARY KEY REFERENCES oxsum.refunds(refund_id),
    order_id uuid NOT NULL REFERENCES oxsum.payment_orders(order_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    credit_note_id uuid REFERENCES oxsum.credit_notes(credit_note_id),
    event_receipt_id uuid NOT NULL REFERENCES oxsum.merchant_event_inbox(receipt_id),
    cash_minor bigint NOT NULL CHECK (cash_minor > 0),
    wallet_minor bigint NOT NULL CHECK (wallet_minor >= 0),
    suspense_minor bigint NOT NULL CHECK (suspense_minor >= 0),
    wallet_restored_minor bigint NOT NULL CHECK (wallet_restored_minor >= 0),
    cash_entry_id uuid NOT NULL,
    cash_entry_hash text NOT NULL CHECK (cash_entry_hash ~ '^[0-9a-f]{64}$'),
    cash_booking_date date NOT NULL,
    correction_entry_id uuid,
    correction_entry_hash text CHECK (correction_entry_hash ~ '^[0-9a-f]{64}$'),
    legacy_recovery boolean NOT NULL,
    source_revision text NOT NULL CHECK (source_revision ~ '^[0-9a-f]{64}$'),
    source jsonb NOT NULL,
    effect jsonb NOT NULL,
    booked_at timestamptz NOT NULL DEFAULT now(),
    CHECK (cash_minor::numeric = wallet_minor::numeric + suspense_minor::numeric),
    CHECK ((correction_entry_id IS NULL) = (correction_entry_hash IS NULL)),
    CHECK ((wallet_restored_minor = 0) = (correction_entry_id IS NULL)),
    CHECK (legacy_recovery OR wallet_restored_minor = 0)
);
CREATE INDEX nonplan_refund_effects_order ON oxsum.nonplan_refund_effects(order_id);
CREATE INDEX nonplan_refund_effects_customer_date
    ON oxsum.nonplan_refund_effects(organization_id,cash_booking_date);
CREATE TRIGGER nonplan_refund_effects_immutable BEFORE UPDATE OR DELETE
    ON oxsum.nonplan_refund_effects FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

ALTER TABLE oxsum.finance_audit DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
    'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
    'journal.void', 'statement.release', 'reconciliation.resolve',
    'reconciliation.waive', 'reconciliation.repair', 'plan_refund.recover',
    'plan_usage.correct', 'nonplan_refund.recover'));
