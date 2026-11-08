-- A confirmed external cash return has one immutable source allocation.
-- Exceptions are retained reconciliation suspense, never invented receivables.
CREATE TABLE oxsum.plan_refund_effects (
    refund_id uuid PRIMARY KEY REFERENCES oxsum.refunds(refund_id),
    order_id uuid NOT NULL REFERENCES oxsum.payment_orders(order_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    plan_period_id uuid REFERENCES oxsum.plan_periods(plan_period_id),
    journal_id uuid REFERENCES oxsum.invoice_journals(journal_id),
    credit_note_id uuid NOT NULL REFERENCES oxsum.credit_notes(credit_note_id),
    event_receipt_id uuid REFERENCES oxsum.merchant_event_inbox(receipt_id),
    cash_minor bigint NOT NULL CHECK (cash_minor > 0),
    deferred_minor bigint NOT NULL CHECK (deferred_minor >= 0),
    earned_minor bigint NOT NULL CHECK (earned_minor >= 0),
    payable_minor bigint NOT NULL CHECK (payable_minor >= 0),
    unallocated_minor bigint NOT NULL CHECK (unallocated_minor >= 0),
    -- The correction may be absent when a legacy entry already has the exact
    -- required legs. entry_id always names the entry proving this allocation.
    entry_id uuid NOT NULL,
    original_entry_id uuid,
    legacy_recovery boolean NOT NULL,
    booked_at timestamptz NOT NULL DEFAULT now(),
    CHECK (cash_minor::numeric = deferred_minor::numeric + earned_minor::numeric
        + payable_minor::numeric + unallocated_minor::numeric)
);
CREATE INDEX plan_refund_effects_period ON oxsum.plan_refund_effects(plan_period_id);
CREATE INDEX plan_refund_effects_order ON oxsum.plan_refund_effects(order_id);
CREATE FUNCTION oxsum.protect_plan_refund_effect() RETURNS trigger AS $guard$
BEGIN
    RAISE EXCEPTION 'plan refund allocation history is immutable';
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER plan_refund_effects_protected BEFORE UPDATE OR DELETE
    ON oxsum.plan_refund_effects FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_refund_effect();

ALTER TABLE oxsum.finance_audit DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
    'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
    'journal.void', 'statement.release', 'reconciliation.resolve',
    'reconciliation.waive', 'reconciliation.repair', 'plan_refund.recover'));
