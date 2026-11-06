-- Payment orders (issue #606): a paid provider event and the value it buys are
-- deliberately two facts. The order pins immutable purpose and amount before any
-- provider call; `merchant_payment_bindings` already holds the matching expected
-- payment under the same UUID (purpose 'order'), so the provider idempotency and
-- the signed-callback classification T14 built keep working unchanged — one
-- provider payment can never fund two orders, and a retry of the same order
-- intent never issues a second charge.

-- The order's terms freeze at create: amount and currency are the ledger terms
-- the wallet books, purpose is the one fulfillment arm the order is allowed to
-- take, and the conversion that produced the ledger amount from the display
-- currency is recorded, never re-rated. `merchant_id`/`version_id` point at the
-- immutable merchant version the checkout was offered under, so a later
-- credential rotation cannot rewrite what the customer agreed to pay.
CREATE TABLE oxsum.payment_orders (
    order_id uuid PRIMARY KEY
        REFERENCES oxsum.merchant_payment_bindings(payment_id),
    business_id uuid NOT NULL,
    merchant_id uuid NOT NULL,
    merchant_version_id uuid NOT NULL,
    organization_id uuid NOT NULL REFERENCES oxsum.customer_profiles(organization_id),
    -- The one fulfillment arm this order may take. The purpose decides what
    -- `paid` may buy: a wallet top-up credits wallet cash, a bundle posts
    -- purchased credit plus a separate promo lot, a plan period activates
    -- quota, and an invoice repayment repays a drawn receivable only. No
    -- purpose may both grant quota and wallet cash.
    purpose text NOT NULL CHECK (purpose IN (
        'wallet_topup', 'bundle', 'plan_period', 'invoice_repayment')),
    -- Ledger terms: integer minor units of one credit, frozen at create.
    amount_minor bigint NOT NULL CHECK (amount_minor > 0),
    currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    -- The server-rated conversion: the customer saw `display_amount_minor` of
    -- `display_currency`, and `amount_minor` was derived from it at the rate
    -- `rate_mantissa / 10^rate_scale` credits per unit, computed server-side.
    -- A client-supplied rate is rejected at the API, so what is stored here is
    -- always the rate the deployment rated, never the customer's claim.
    display_currency text NOT NULL CHECK (display_currency ~ '^[A-Z]{3}$'),
    display_amount_minor bigint NOT NULL CHECK (display_amount_minor > 0),
    rate_mantissa bigint NOT NULL CHECK (rate_mantissa > 0),
    rate_scale integer NOT NULL CHECK (rate_scale BETWEEN 0 AND 8),
    -- Purpose parameters, frozen beside the terms: the plan a `plan_period`
    -- activates, the receivable a `invoice_repayment` settles. NULL for the
    -- purposes that carry none.
    purpose_ref uuid,
    -- A `bundle` order's second leg: the separate promo lot, in credit-minor,
    -- booked into the bonus pool at fulfillment. 0 for the single-leg purposes.
    promo_minor bigint NOT NULL DEFAULT 0 CHECK (promo_minor >= 0),
    -- The provider payment that paid it, copied from merchant_payment_refs once
    -- bound — kept denormalized on the order so the read surface does not join.
    provider_payment_id text CHECK (provider_payment_id IS NULL OR length(provider_payment_id) BETWEEN 1 AND 200),
    -- Transient checkout handles the provider answered (`clientSecret`,
    -- `checkoutUrl`), kept so a replayed create returns the same checkout the
    -- first attempt minted instead of a second charge. Never terms — terms are
    -- the frozen columns above.
    checkout jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- The customer's create key: replaying it returns the standing order, and a
    -- different order body under it conflicts — the same idempotency anchor the
    -- deposit rails use, scoped to the organization.
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    -- created → provider_pending → paid → fulfilled, with expired/failed the
    -- exits and needs_attention the discrepancy state a mismatched paid event
    -- parks in (it can never be silently credited).
    status text NOT NULL DEFAULT 'created' CHECK (status IN (
        'created', 'provider_pending', 'paid', 'fulfilled',
        'expired', 'failed', 'needs_attention')),
    created_at timestamptz NOT NULL DEFAULT now(),
    paid_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, idempotency_key),
    FOREIGN KEY (merchant_id, merchant_version_id)
        REFERENCES oxsum.merchant_versions(merchant_id, version_id)
);
CREATE INDEX payment_orders_organization ON oxsum.payment_orders (organization_id, created_at);
-- The recovery sweep's work list: paid orders that have not fulfilled.
CREATE INDEX payment_orders_unfulfilled ON oxsum.payment_orders (status, created_at)
    WHERE status = 'paid';

-- Order terms and identity are immutable; only the lifecycle columns move, and
-- they never move backwards past a terminal or discrepancy state.
CREATE FUNCTION oxsum.protect_payment_order() RETURNS trigger AS $guard$
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
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        -- paid and fulfilled are terminal-ish: paid_at and the paid/fulfilled
        -- states cannot be unset or walked back; needs_attention is stuck for a
        -- human, not auto-credited.
        OR (OLD.paid_at IS NOT NULL AND NEW.paid_at IS DISTINCT FROM OLD.paid_at)
        OR (OLD.status = 'fulfilled' AND NEW.status IS DISTINCT FROM OLD.status)
        OR (OLD.status = 'needs_attention' AND NEW.status IS DISTINCT FROM OLD.status) THEN
        RAISE EXCEPTION 'payment order terms and terminal states are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER payment_orders_protected BEFORE UPDATE OR DELETE
    ON oxsum.payment_orders FOR EACH ROW EXECUTE FUNCTION oxsum.protect_payment_order();

-- One fulfillment per order, written the moment the purpose's grant lands. The
-- payload records exactly what was granted — the wallet entry the top-up
-- booked, the plan period activated, the receivable repaid — so the grant is
-- auditable without re-deriving it, and the outbox row is the dispatch record
-- downstream projections reconcile against.
CREATE TABLE oxsum.payment_fulfillments (
    fulfillment_id uuid PRIMARY KEY,
    order_id uuid NOT NULL UNIQUE REFERENCES oxsum.payment_orders(order_id),
    purpose text NOT NULL CHECK (purpose IN (
        'wallet_topup', 'bundle', 'plan_period', 'invoice_repayment')),
    purpose_payload jsonb NOT NULL,
    outbox_id uuid,
    fulfilled_at timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER payment_fulfillments_immutable BEFORE UPDATE OR DELETE
    ON oxsum.payment_fulfillments FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

-- The durable outbox: one row per fulfillment for downstream projections and
-- reconciliations to drain. Fulfillment writes it in the same transaction that
-- lands the grant, so the grant and its dispatch commit or roll back together.
CREATE TABLE oxsum.payment_outbox (
    outbox_id uuid PRIMARY KEY,
    order_id uuid NOT NULL REFERENCES oxsum.payment_orders(order_id),
    event_type text NOT NULL,
    payload jsonb NOT NULL,
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'delivered')),
    created_at timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz
);
CREATE INDEX payment_outbox_pending ON oxsum.payment_outbox (state, created_at)
    WHERE state = 'pending';
ALTER TABLE oxsum.payment_fulfillments
    ADD CONSTRAINT payment_fulfillments_outbox
    FOREIGN KEY (outbox_id) REFERENCES oxsum.payment_outbox(outbox_id)
    DEFERRABLE INITIALLY DEFERRED;
