-- Exact statement repayment intents and immutable per-journal cash effects.
-- No legacy payment purpose changes identity or meaning.
ALTER TABLE oxsum.payment_orders ADD COLUMN statement_payment jsonb;
ALTER TABLE oxsum.payment_orders DROP CONSTRAINT payment_orders_purpose_check;
ALTER TABLE oxsum.payment_orders ADD CHECK (purpose IN ('wallet_topup','bundle','plan_period','invoice_repayment','statement_repayment'));
ALTER TABLE oxsum.payment_fulfillments DROP CONSTRAINT payment_fulfillments_purpose_check;
ALTER TABLE oxsum.payment_fulfillments ADD CHECK (purpose IN ('wallet_topup','bundle','plan_period','invoice_repayment','statement_repayment'));
CREATE FUNCTION oxsum.protect_statement_payment_terms() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='UPDATE' AND NEW.statement_payment IS DISTINCT FROM OLD.statement_payment THEN
        RAISE EXCEPTION 'statement payment terms are immutable';
    END IF;
    IF (NEW.purpose='statement_repayment') IS DISTINCT FROM (NEW.statement_payment IS NOT NULL) THEN
        RAISE EXCEPTION 'statement repayment requires its exact quote';
    END IF;
    IF NEW.purpose='statement_repayment' AND (
        NEW.purpose_ref IS DISTINCT FROM (NEW.statement_payment->>'statementId')::uuid OR
        NEW.organization_id IS DISTINCT FROM (NEW.statement_payment->>'organizationId')::uuid OR
        NEW.business_id IS DISTINCT FROM (NEW.statement_payment->>'businessId')::uuid OR
        NEW.amount_minor IS DISTINCT FROM (NEW.statement_payment->>'amountMinor')::bigint OR
        NEW.display_amount_minor IS DISTINCT FROM (NEW.statement_payment->>'displayAmountMinor')::bigint OR
        NEW.display_currency IS DISTINCT FROM NEW.statement_payment->>'displayCurrency' OR
        NEW.rate_mantissa IS DISTINCT FROM (NEW.statement_payment->>'rateMantissa')::bigint OR
        NEW.rate_scale IS DISTINCT FROM (NEW.statement_payment->>'rateScale')::integer OR
        NEW.promo_minor<>0 OR NEW.plan_payment IS NOT NULL OR NEW.bundle_offer IS NOT NULL
    ) THEN RAISE EXCEPTION 'statement repayment snapshot differs from order'; END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER statement_payment_terms BEFORE INSERT OR UPDATE ON oxsum.payment_orders
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_statement_payment_terms();

CREATE TABLE oxsum.statement_payment_reservations (
    order_id uuid PRIMARY KEY REFERENCES oxsum.payment_orders(order_id),
    statement_id uuid NOT NULL REFERENCES oxsum.plan_statements(statement_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    amount_minor bigint NOT NULL CHECK(amount_minor>0),
    state text NOT NULL DEFAULT 'reserved' CHECK(state IN ('reserved','released','applied')),
    release_receipt_id uuid REFERENCES oxsum.merchant_event_inbox(receipt_id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((state='released')=(release_receipt_id IS NOT NULL))
);
CREATE INDEX statement_payment_reserved ON oxsum.statement_payment_reservations(statement_id) WHERE state='reserved';
CREATE FUNCTION oxsum.protect_statement_reservation() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'statement reservation history is retained'; END IF;
    IF NEW.order_id IS DISTINCT FROM OLD.order_id OR NEW.statement_id IS DISTINCT FROM OLD.statement_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id OR NEW.amount_minor IS DISTINCT FROM OLD.amount_minor
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        OR (OLD.state='applied' AND NEW.state<>'applied')
        OR (OLD.state='released' AND NEW.state NOT IN ('released','applied'))
        OR (NEW.state='released' AND NOT EXISTS(SELECT 1 FROM oxsum.merchant_event_inbox e WHERE e.receipt_id=NEW.release_receipt_id AND (e.normalized->>'paymentId'=NEW.order_id::text OR (e.normalized->>'paymentId' IS NULL AND EXISTS(SELECT 1 FROM oxsum.merchant_payment_refs r JOIN oxsum.merchant_payment_bindings b USING(payment_id) WHERE r.payment_id=NEW.order_id AND r.merchant_id=e.merchant_id AND r.provider_payment_id=e.normalized->>'providerPaymentId' AND b.merchant_id=e.merchant_id AND b.version_id=e.version_id AND b.organization_id=NEW.organization_id))) AND e.state IN ('accepted','applied') AND e.normalized->>'kind' IN ('failed','expired')))
    THEN RAISE EXCEPTION 'invalid statement reservation transition'; END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER statement_reservation_protected BEFORE UPDATE OR DELETE ON oxsum.statement_payment_reservations
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_statement_reservation();

CREATE TABLE oxsum.statement_payment_effects (
    effect_id uuid PRIMARY KEY,
    order_id uuid NOT NULL REFERENCES oxsum.payment_orders(order_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    statement_id uuid NOT NULL REFERENCES oxsum.plan_statements(statement_id),
    journal_id uuid REFERENCES oxsum.invoice_journals(journal_id),
    kind text NOT NULL CHECK(kind IN ('payment','refund','dispute')),
    source_ref uuid NOT NULL,
    original_effect_id uuid REFERENCES oxsum.statement_payment_effects(effect_id),
    event_receipt_id uuid REFERENCES oxsum.merchant_event_inbox(receipt_id),
    cash_minor bigint NOT NULL CHECK(cash_minor>0),
    receivable_minor bigint NOT NULL CHECK(receivable_minor>=0),
    payable_minor bigint NOT NULL CHECK(payable_minor>=0),
    suspense_minor bigint NOT NULL CHECK(suspense_minor>=0),
    entry_id uuid NOT NULL UNIQUE,
    entry_hash text NOT NULL,
    booked_on date NOT NULL,
    source jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK(cash_minor::numeric=receivable_minor::numeric+payable_minor::numeric+suspense_minor::numeric),
    CHECK(kind<>'payment' OR (source_ref=order_id AND original_effect_id IS NULL AND suspense_minor=0)),
    UNIQUE(kind,source_ref,journal_id)
);
CREATE INDEX statement_payment_effect_order ON oxsum.statement_payment_effects(order_id,kind);
CREATE INDEX statement_payment_effect_journal ON oxsum.statement_payment_effects(journal_id,kind);
CREATE TRIGGER statement_payment_effect_immutable BEFORE UPDATE OR DELETE ON oxsum.statement_payment_effects
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

ALTER TABLE oxsum.plan_accounting_effects DROP CONSTRAINT plan_accounting_effects_kind_check;
ALTER TABLE oxsum.plan_accounting_effects ADD CHECK(kind IN ('activation','recognition','refund','dispute','void','statement_payment','statement_return'));

CREATE OR REPLACE FUNCTION oxsum.protect_invoice_journal() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'journal history is retained'; END IF;
    IF NEW.journal_id IS DISTINCT FROM OLD.journal_id
        OR NEW.customer_plan_id IS DISTINCT FROM OLD.customer_plan_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.plan_period_id IS DISTINCT FROM OLD.plan_period_id
        OR NEW.period_index IS DISTINCT FROM OLD.period_index
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.replaces_journal_id IS DISTINCT FROM OLD.replaces_journal_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'journal identity and terms are immutable';
    END IF;
    -- `total_minor` is the lines' sum: it may only move on a live journal, and
    -- only to what the lines now add to — the consistency the posted-journal
    -- immutability guarantee leans on. `void` is immutable outright; a `paid`
    -- journal's total may still fall — never rise — under an appended
    -- `discount` line, which is exactly the refund correction (`debit
    -- Discounts / credit Cash`) the bill already settled. The trigger cannot
    -- see same-transaction deferred order, so writers insert lines before
    -- updating the figure.
    IF NEW.total_minor IS DISTINCT FROM OLD.total_minor THEN
        IF OLD.state = 'void' THEN
            RAISE EXCEPTION 'a settled journal cannot be re-priced';
        END IF;
        IF OLD.state = 'paid' AND NOT (
            NEW.total_minor < OLD.total_minor AND NEW.state = 'paid'
        ) THEN
            RAISE EXCEPTION 'only a refund correction re-prices a paid journal';
        END IF;
        IF NEW.total_minor IS DISTINCT FROM (
            SELECT COALESCE(SUM(amount_minor),0)::bigint
            FROM oxsum.invoice_journal_lines WHERE journal_id = NEW.journal_id
        ) THEN
            RAISE EXCEPTION 'total_minor must equal the journal lines sum';
        END IF;
    END IF;
    -- The state walk itself: each step names the only states it may leave to.
    IF NEW.state IS DISTINCT FROM OLD.state THEN
        IF NOT (
            (OLD.state='draft'  AND NEW.state IN ('posted','void')) OR
            (OLD.state='posted' AND NEW.state IN ('paid','void')) OR
            (OLD.state='paid'   AND NEW.state IN ('posted','void'))
        ) THEN
            RAISE EXCEPTION 'invalid journal state transition % -> %', OLD.state, NEW.state;
        END IF;
        -- Reopening requires either a dispute increase or a same-transaction
        -- immutable statement return matching the exact settled reduction.
        IF OLD.state='paid' AND NEW.state='posted'
            AND NOT (NEW.disputed_minor > OLD.disputed_minor OR (
                NEW.settled_minor<OLD.settled_minor AND EXISTS(
                    SELECT 1 FROM oxsum.statement_payment_effects e WHERE e.journal_id=NEW.journal_id
                    AND e.kind IN ('refund','dispute') AND e.receivable_minor=OLD.settled_minor-NEW.settled_minor
                    AND e.xmin::text=pg_current_xact_id()::text))) THEN
            RAISE EXCEPTION 'a paid journal requires a source-linked return to reopen';
        END IF;
    END IF;
    -- Terminal rows: only the dispute counters may still move on `paid` —
    -- `disputed_minor` tracking and the audit-facing `settled_minor` stay
    -- honest under arbitration; `void` accepts nothing at all.
    IF OLD.state='void' THEN
        RAISE EXCEPTION 'a void journal is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION oxsum.protect_plan_statement() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'statement history is retained'; END IF;
    IF NEW.statement_id IS DISTINCT FROM OLD.statement_id
        OR NEW.customer_plan_id IS DISTINCT FROM OLD.customer_plan_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.period_index IS DISTINCT FROM OLD.period_index
        OR NEW.journal_id IS DISTINCT FROM OLD.journal_id
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.replaces_statement_id IS DISTINCT FROM OLD.replaces_statement_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'statement identity and terms are immutable';
    END IF;
    -- `total_minor` tracks charge/credit lines, excluding payment allocations.
    -- A refund or
    -- dispute correction appends a line then lands the figure — the trigger
    -- sees the committed lines, so writers append before they update.
    IF NEW.total_minor IS DISTINCT FROM OLD.total_minor THEN
        IF OLD.state = 'void' THEN
            RAISE EXCEPTION 'a settled statement cannot be re-priced';
        END IF;
        IF NEW.total_minor IS DISTINCT FROM (
            SELECT COALESCE(SUM(amount_minor),0)::bigint
            FROM oxsum.plan_statement_lines WHERE statement_id = NEW.statement_id
                AND kind <> 'payment_applied'
        ) THEN
            RAISE EXCEPTION 'total_minor must equal the statement charge lines sum';
        END IF;
    END IF;
    IF NEW.state IS DISTINCT FROM OLD.state THEN
        IF NOT (
            (OLD.state='open'     AND NEW.state IN ('issued','void')) OR
            (OLD.state='issued'   AND NEW.state IN ('paid','past_due','void')) OR
            (OLD.state='past_due' AND NEW.state IN ('paid','issued','void')) OR
            (OLD.state='paid' AND NEW.state='void') OR
            (OLD.state='paid' AND NEW.state='issued' AND NEW.paid_minor<OLD.paid_minor
             AND EXISTS(SELECT 1 FROM oxsum.statement_payment_effects e WHERE e.statement_id=NEW.statement_id
                 AND e.kind IN ('refund','dispute') AND e.receivable_minor>0
                 AND e.xmin::text=pg_current_xact_id()::text)
             AND NEW.paid_minor IS NOT DISTINCT FROM (SELECT -COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.plan_statement_lines WHERE statement_id=NEW.statement_id AND kind='payment_applied'))
        ) THEN
            RAISE EXCEPTION 'invalid statement state transition % -> %', OLD.state, NEW.state;
        END IF;
    END IF;
    IF OLD.state='void' THEN
        RAISE EXCEPTION 'a void statement is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;

ALTER TABLE oxsum.finance_audit DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
    'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
    'journal.void', 'statement.release', 'reconciliation.resolve',
    'reconciliation.waive', 'reconciliation.repair', 'plan_refund.recover',
    'plan_usage.correct', 'nonplan_refund.recover', 'nonplan_chargeback.recover', 'statement.refund'));

-- A financial void reserves its exact repayment-funded payable for that payment.
-- Old plan-order refunds cannot consume these independently funded shares.
CREATE TABLE oxsum.statement_void_allocations (
    effect_id uuid PRIMARY KEY REFERENCES oxsum.statement_payment_effects(effect_id),
    journal_id uuid NOT NULL REFERENCES oxsum.invoice_journals(journal_id),
    amount_minor bigint NOT NULL CHECK(amount_minor>0),
    entry_id uuid NOT NULL,
    entry_hash text NOT NULL
);
CREATE TRIGGER statement_void_allocation_immutable BEFORE UPDATE OR DELETE ON oxsum.statement_void_allocations
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
