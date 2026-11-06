-- Manual payment orders (issue #607): an authorized finance action — a verified
-- out-of-band receipt — is recorded as a payment order like a provider-funded
-- one, so the customer's order history and the reconciliation report see every
-- funded top-up in one place. The manual order's evidence is the receipt
-- itself; no provider ever confirms it, so it carries no merchant and no
-- expected-payment binding.

-- `source` names how the order's payment arrived: `provider` for a rail's
-- signed webhook, `manual` for a staff-recorded receipt. `evidence` is the
-- manual order's audit payload — the receipt reference and the note the
-- operator recorded — kept beside the terms so it never has to be re-derived.
ALTER TABLE oxsum.payment_orders
    ADD COLUMN source text NOT NULL DEFAULT 'provider'
        CHECK (source IN ('provider', 'manual')),
    ADD COLUMN evidence jsonb NOT NULL DEFAULT '{}'::jsonb;

-- A manual order has no merchant: its payment never transited a rail, so the
-- merchant version columns go nullable. Provider orders keep their pinned
-- merchant; the composite foreign key is skipped when both columns are NULL.
ALTER TABLE oxsum.payment_orders
    ALTER COLUMN merchant_id DROP NOT NULL,
    ALTER COLUMN merchant_version_id DROP NOT NULL;

-- The order id no longer requires an expected-payment binding: a manual order
-- has no provider payment to expect. Provider orders still write their binding
-- in the same transaction that pins the terms — the binding stays the webhook
-- classification's anchor — but the database no longer forces a manual order
-- to fabricate one.
ALTER TABLE oxsum.payment_orders
    DROP CONSTRAINT payment_orders_order_id_fkey;

-- One receipt can fund exactly one manual order: `evidence.reference` is
-- unique per business among manual orders, so the same bank reference can
-- never be recorded twice. The index is partial — provider orders carry no
-- receipt reference and stay out of it.
CREATE UNIQUE INDEX payment_orders_manual_reference
    ON oxsum.payment_orders (business_id, (evidence ->> 'reference'))
    WHERE source = 'manual';

-- `source` and `evidence` join the terms the protection trigger freezes: a
-- manual order's provenance is set at record time and never rewritten, exactly
-- like the provider order's pinned conversion.
CREATE OR REPLACE FUNCTION oxsum.protect_payment_order() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'payment order history is retained'; END IF;
    IF NEW.order_id IS DISTINCT FROM OLD.order_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.merchant_id IS DISTINCT FROM OLD.merchant_id
        OR NEW.merchant_version_id IS DISTINCT FROM OLD.merchant_version_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.purpose IS DISTINCT FROM OLD.purpose
        OR NEW.amount_minor IS DISTINCT FROM OLD.amount_minor
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.display_currency IS DISTINCT FROM OLD.display_currency
        OR NEW.display_amount_minor IS DISTINCT FROM OLD.display_amount_minor
        OR NEW.rate_mantissa IS DISTINCT FROM OLD.rate_mantissa
        OR NEW.rate_scale IS DISTINCT FROM OLD.rate_scale
        OR NEW.purpose_ref IS DISTINCT FROM OLD.purpose_ref
        OR NEW.promo_minor IS DISTINCT FROM OLD.promo_minor
        OR NEW.idempotency_key IS DISTINCT FROM OLD.idempotency_key
        OR NEW.source IS DISTINCT FROM OLD.source
        OR NEW.evidence IS DISTINCT FROM OLD.evidence
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        OR (OLD.paid_at IS NOT NULL AND NEW.paid_at IS DISTINCT FROM OLD.paid_at)
        OR (OLD.status = 'fulfilled' AND NEW.status IS DISTINCT FROM OLD.status)
        OR (OLD.status = 'needs_attention' AND NEW.status IS DISTINCT FROM OLD.status) THEN
        RAISE EXCEPTION 'payment order terms and terminal states are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
