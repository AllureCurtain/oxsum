-- Corrections, partial refunds and disputes (issue #608, task T17).
--
-- T15's orders separated *paid* from *fulfilled*; this migration separates the
-- refund's three facts the same way. A customer *requests* a review — it can
-- never grant itself money — staff resolves it, and the reservation a
-- `provider_pending` refund row holds against the order's refundable bound is
-- durable *before* any provider call: a crash mid-PSP-call leaves a retryable
-- row, never a lost refund.
--
-- The wallet credit is booked only when the signed `refund_succeeded` event
-- lands — the ledger mirrors the funding entry (`debit Wallet, credit Cash`),
-- and the pool's `FundedReservations` limit is what refuses a take-back the
-- customer already spent. A refund the PSP refuses — or one that times out
-- past the retry budget — lands `released`, freeing the bound; a compensation
-- the wallet cannot carry is recorded on the audit row, never silently
-- negative.

-- `oxsum.credit_notes` (issue #352) becomes the refund's correction record:
-- the note names the order it corrects and the refund it was issued under,
-- and walks `issued → applied` as its ledger entry lands — `reversed` when the
-- refund it booked under releases without applying. Legacy notes are
-- `applied` already: their entry always stood.
ALTER TABLE oxsum.credit_notes
    ALTER COLUMN entry_id DROP NOT NULL,
    ADD COLUMN order_id uuid REFERENCES oxsum.payment_orders(order_id),
    ADD COLUMN refund_id uuid,
    ADD COLUMN state text NOT NULL DEFAULT 'applied'
        CHECK (state IN ('issued', 'applied', 'reversed')),
    -- The staff user whose resolution produced the note; NULL for the admin
    -- surface's free-credit notes, which carry no finance audit.
    ADD COLUMN issued_by uuid REFERENCES oxsum.users(user_id),
    -- The `oxsum.finance_audit` row the resolution wrote.
    ADD COLUMN audit_ref uuid,
    ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();

-- Terms and identity are immutable once written; the state machine only walks
-- forward, `reversed` is terminal, and an applied note keeps its entry id.
CREATE FUNCTION oxsum.protect_credit_note() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'credit note history is retained'; END IF;
    IF NEW.credit_note_id IS DISTINCT FROM OLD.credit_note_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.order_id IS DISTINCT FROM OLD.order_id
        OR NEW.refund_id IS DISTINCT FROM OLD.refund_id
        OR NEW.amount_minor IS DISTINCT FROM OLD.amount_minor
        OR NEW.reason IS DISTINCT FROM OLD.reason
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        OR (OLD.state = 'applied' AND NEW.entry_id IS DISTINCT FROM OLD.entry_id)
        OR (OLD.state = 'reversed' AND NEW.state IS DISTINCT FROM OLD.state)
        OR (OLD.state = 'applied' AND NEW.state NOT IN ('applied','reversed')) THEN
        RAISE EXCEPTION 'credit note terms and terminal states are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER credit_notes_protected BEFORE UPDATE OR DELETE
    ON oxsum.credit_notes FOR EACH ROW EXECUTE FUNCTION oxsum.protect_credit_note();

-- One refund per decision: the customer's request creates `requested`, the
-- staff resolution moves it to `provider_pending` (the durable reservation,
-- written before the PSP call) or `declined`, and the signed provider event —
-- or the retry budget running out — lands the terminal `succeeded`,
-- `failed` or `released`. `released` frees the reservation's bound; `failed`
-- is a PSP-reported refusal the audit already records.
CREATE TABLE oxsum.refunds (
    refund_id            uuid PRIMARY KEY,
    order_id             uuid NOT NULL REFERENCES oxsum.payment_orders(order_id),
    organization_id      uuid NOT NULL REFERENCES oxsum.customer_profiles(organization_id),
    business_id          uuid NOT NULL,
    -- The credit amount the customer asked back, in ledger minor units —
    -- bounded by the order's `amount_minor` minus what earlier refunds
    -- already reserved or returned, so the sum can never exceed the funded
    -- amount and no wallet is ever driven negative by it.
    amount_minor         bigint NOT NULL CHECK (amount_minor > 0),
    -- The fiat figure the PSP is asked to return, converted back at the
    -- order's own pinned rate: proportional parts, and the last (full)
    -- refund takes the remainder so rounding never strands a cent.
    display_amount_minor bigint NOT NULL CHECK (display_amount_minor > 0),
    display_currency     text NOT NULL CHECK (display_currency ~ '^[A-Z]{3}$'),
    reason               text NOT NULL CHECK (length(reason) BETWEEN 1 AND 500),
    state                text NOT NULL DEFAULT 'requested' CHECK (state IN (
        'requested', 'declined', 'provider_pending',
        'succeeded', 'failed', 'released')),
    -- The provider's own refund id once issued — the dedup the
    -- `refund_succeeded`/`refund_failed` webhooks match on.
    provider_refund_id   text CHECK (provider_refund_id IS NULL OR length(provider_refund_id) BETWEEN 1 AND 200),
    -- The provider payment the refund runs against, copied from the order.
    provider_payment_id  text CHECK (provider_payment_id IS NULL OR length(provider_payment_id) BETWEEN 1 AND 200),
    retry_count          integer NOT NULL DEFAULT 0,
    next_retry_at        timestamptz,
    requested_by         uuid REFERENCES oxsum.users(user_id),
    resolved_by          uuid REFERENCES oxsum.users(user_id),
    audit_ref            uuid,
    -- The customer's request key: a replay answers the standing refund.
    request_key          text NOT NULL CHECK (length(request_key) BETWEEN 1 AND 200),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, request_key)
);
CREATE INDEX refunds_order ON oxsum.refunds (order_id, created_at);
CREATE INDEX refunds_organization ON oxsum.refunds (organization_id, created_at);
-- The retry sweep's work list: pending refunds whose next attempt is due.
CREATE INDEX refunds_retry_due ON oxsum.refunds (next_retry_at)
    WHERE state = 'provider_pending';
-- One provider refund id can settle at most one refund row.
CREATE UNIQUE INDEX refunds_provider_id ON oxsum.refunds (provider_refund_id)
    WHERE provider_refund_id IS NOT NULL;

CREATE FUNCTION oxsum.protect_refund() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'refund history is retained'; END IF;
    IF NEW.refund_id IS DISTINCT FROM OLD.refund_id
        OR NEW.order_id IS DISTINCT FROM OLD.order_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.amount_minor IS DISTINCT FROM OLD.amount_minor
        OR NEW.display_amount_minor IS DISTINCT FROM OLD.display_amount_minor
        OR NEW.display_currency IS DISTINCT FROM OLD.display_currency
        OR NEW.request_key IS DISTINCT FROM OLD.request_key
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        -- Terminal states cannot be walked back; a bound once freed stays freed.
        OR (OLD.state IN ('declined','succeeded','failed','released')
            AND NEW.state IS DISTINCT FROM OLD.state) THEN
        RAISE EXCEPTION 'refund terms and terminal states are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER refunds_protected BEFORE UPDATE OR DELETE
    ON oxsum.refunds FOR EACH ROW EXECUTE FUNCTION oxsum.protect_refund();

-- A dispute is deliberately *not* a refund: the provider's `dispute_*` events
-- open a contested-charge record of their own, whose lifecycle ends `won`,
-- `lost` or `closed`. `lost` re-charges the drawn receivable through
-- `Wallet::repay_credit` under the dispute's own ledger key — the ledger
-- never confuses a contested reversal with a granted refund.
CREATE TABLE oxsum.disputes (
    dispute_id           uuid PRIMARY KEY,
    order_id             uuid NOT NULL REFERENCES oxsum.payment_orders(order_id),
    organization_id      uuid NOT NULL REFERENCES oxsum.customer_profiles(organization_id),
    merchant_id          uuid NOT NULL,
    -- The provider's case identifier (the event's `dispute_id`/`refund_id`
    -- field, falling back to its payment id).
    provider_dispute_id  text NOT NULL CHECK (length(provider_dispute_id) BETWEEN 1 AND 200),
    amount_minor         bigint NOT NULL CHECK (amount_minor > 0),
    currency             text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    state                text NOT NULL DEFAULT 'opened' CHECK (state IN (
        'opened', 'evidence_due', 'won', 'lost', 'closed')),
    -- The `merchant_event_inbox` receipt that opened it.
    inbox_receipt_id     uuid REFERENCES oxsum.merchant_event_inbox(receipt_id),
    -- The ledger entry a `lost` dispute re-charged under, when it landed.
    recharge_entry_id    uuid,
    resolved_by          uuid REFERENCES oxsum.users(user_id),
    audit_ref            uuid,
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    UNIQUE (merchant_id, provider_dispute_id)
);
CREATE INDEX disputes_order ON oxsum.disputes (order_id, created_at);
CREATE INDEX disputes_state ON oxsum.disputes (state, created_at)
    WHERE state IN ('opened', 'evidence_due');

CREATE FUNCTION oxsum.protect_dispute() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'dispute history is retained'; END IF;
    IF NEW.dispute_id IS DISTINCT FROM OLD.dispute_id
        OR NEW.order_id IS DISTINCT FROM OLD.order_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.merchant_id IS DISTINCT FROM OLD.merchant_id
        OR NEW.provider_dispute_id IS DISTINCT FROM OLD.provider_dispute_id
        OR NEW.amount_minor IS DISTINCT FROM OLD.amount_minor
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        OR (OLD.state IN ('won','lost','closed')
            AND NEW.state IS DISTINCT FROM OLD.state) THEN
        RAISE EXCEPTION 'dispute terms and terminal states are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER disputes_protected BEFORE UPDATE OR DELETE
    ON oxsum.disputes FOR EACH ROW EXECUTE FUNCTION oxsum.protect_dispute();

-- The finance resolution's audit: every approve/decline/close names the staff
-- actor, the reason and the customer-visible note it produced — immutable
-- once written, like `admin_audit` but scoped to the business.
CREATE TABLE oxsum.finance_audit (
    audit_id        uuid PRIMARY KEY,
    business_id     uuid NOT NULL,
    actor_user_id   uuid NOT NULL REFERENCES oxsum.users(user_id),
    action          text NOT NULL CHECK (action IN (
        'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve')),
    target          text NOT NULL,
    detail          jsonb NOT NULL DEFAULT '{}'::jsonb,
    note_id         uuid,
    idempotency_key text CHECK (idempotency_key IS NULL OR length(idempotency_key) BETWEEN 1 AND 200),
    created_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (business_id, action, idempotency_key)
);
CREATE INDEX finance_audit_business ON oxsum.finance_audit (business_id, created_at DESC);
CREATE TRIGGER finance_audit_immutable BEFORE UPDATE OR DELETE
    ON oxsum.finance_audit FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

-- The deferred FKs: a credit note and a refund each name the audit row that
-- resolved them, and the audit row names the note it produced — resolvable
-- inside one transaction.
ALTER TABLE oxsum.credit_notes
    ADD CONSTRAINT credit_notes_audit
    FOREIGN KEY (audit_ref) REFERENCES oxsum.finance_audit(audit_id)
    DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE oxsum.refunds
    ADD CONSTRAINT refunds_audit
    FOREIGN KEY (audit_ref) REFERENCES oxsum.finance_audit(audit_id)
    DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE oxsum.finance_audit
    ADD CONSTRAINT finance_audit_note
    FOREIGN KEY (note_id) REFERENCES oxsum.credit_notes(credit_note_id)
    DEFERRABLE INITIALLY DEFERRED;
-- The refund a credit note was issued under.
ALTER TABLE oxsum.credit_notes
    ADD CONSTRAINT credit_notes_refund
    FOREIGN KEY (refund_id) REFERENCES oxsum.refunds(refund_id)
    DEFERRABLE INITIALLY DEFERRED;
