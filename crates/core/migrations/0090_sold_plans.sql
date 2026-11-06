-- Sold plans and the customer plan lifecycle (issue #610, T19).
--
-- The legacy `subscription_plans`/`org_subscriptions` machinery stays the quota
-- engine: a period's consumption still counts on the subscription row and its
-- draws still land on `subscription_draws`. What this migration adds is the
-- *sold* layer on top — the versioned catalogue a business sells from, the
-- customer's entitlement to one plan at a time, and the per-period
-- authorization D06 requires so a timer rollover can never issue an unpaid
-- period.
--
-- `oxsum.sold_plans` is the versioned catalogue. A plan (`plan_id`) keeps its
-- identity across versions; each `sold_plans` row is one immutable version of
-- the terms — price, quota, period kind, overage policy. A customer binding
-- references a specific version, so a later edit never rewrites what was sold.
-- `period_kind` names the boundary policy explicitly: `calendar_month`
-- preserves the legacy UTC-month semantics, `anniversary` is the new versioned
-- policy that rolls on the activation date's monthly anniversary (clamped to
-- the month's last day for short months — a Jan 31 activation renews Feb 28).
--
-- `oxsum.customer_plans` is one customer's link to a sold plan version — at
-- most one non-terminal row per organization. Its state machine is D06's:
-- `pending_payment` before the first period's authorization lands, `active`
-- while an authorized period runs, `renewal_pending`/`past_due` once a
-- boundary passed without a paid renewal, `expired` when the past-due grace
-- closes, `cancelled` when the customer or the business ends it. The two
-- boundary modifiers are explicit columns, never implied state:
-- `cancel_at_period_end` serves through the paid period but blocks renewal,
-- and `scheduled_change` books a plan-version swap that takes effect at the
-- next boundary — no mid-period pro-rating.
--
-- `oxsum.plan_periods` is the authorization ledger every period lives by. A
-- period only exists once its authorization does: `authorization_kind` is
-- `payment_order` for a paid period (the `payment_orders` row that funded it)
-- or `complimentary` for a free plan's grant (`authorization_ref` names the
-- grant's durable key). A timer can never open a period on its own — the
-- rollover path inserts the next row only when a paid order for it has
-- fulfilled, and a renewal order's `purpose_ref` names the `customer_plans`
-- row it buys the next period for. `(customer_plan_id, period_index)` is
-- unique, so a retried fulfillment cannot double-issue a period.

CREATE TABLE oxsum.sold_plans (
    -- The version row's own id — what a customer binding references.
    plan_version_id uuid PRIMARY KEY,
    -- The plan's stable identity across versions.
    plan_id         uuid NOT NULL,
    business_id     uuid NOT NULL REFERENCES oxsum.businesses(business_id) ON DELETE RESTRICT,
    -- Monotonic within `plan_id`: version 1 is the first sold form.
    version         integer NOT NULL CHECK (version > 0),
    name            text NOT NULL,
    -- The catalogue price in ledger minor units; 0 is a free plan, which
    -- activates on a complimentary grant rather than a payment order.
    price_minor     bigint NOT NULL CHECK (price_minor >= 0),
    -- The display currency the price is quoted in.
    currency        text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    -- Quota per period in credit minor units — the same axis the wallet and
    -- the legacy `subscription_plans.quota_minor` price in. NULL is unmetered.
    quota_minor     bigint CHECK (quota_minor IS NULL OR quota_minor > 0),
    -- The boundary policy: `calendar_month` keeps the legacy UTC-month
    -- semantics `subscription_periods` already records; `anniversary` rolls on
    -- the activation date's monthly anniversary instead.
    period_kind     text NOT NULL CHECK (period_kind IN ('calendar_month', 'anniversary')),
    -- Opt-in overage: when the period's quota is exhausted, a request may draw
    -- past it and charge the wallet at the plan's rates. Explicit, never silent.
    overage_enabled boolean NOT NULL DEFAULT false,
    sort_order      integer NOT NULL DEFAULT 0,
    -- NULL while the version is a draft: only a published version can be sold.
    published_at    timestamptz,
    -- Set when the version stops accepting new binds; existing subscriptions
    -- keep running under it to their boundaries.
    retired_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (plan_id, version)
);
CREATE INDEX sold_plans_catalog
    ON oxsum.sold_plans (business_id, plan_id, version);

-- A sold version's terms are immutable once published — a correction publishes
-- a new version, never rewrites what a customer bought.
CREATE FUNCTION oxsum.protect_sold_plan() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'sold plan history is retained'; END IF;
    IF OLD.published_at IS NOT NULL AND (
        NEW.plan_id IS DISTINCT FROM OLD.plan_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.version IS DISTINCT FROM OLD.version
        OR NEW.name IS DISTINCT FROM OLD.name
        OR NEW.price_minor IS DISTINCT FROM OLD.price_minor
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.quota_minor IS DISTINCT FROM OLD.quota_minor
        OR NEW.period_kind IS DISTINCT FROM OLD.period_kind
        OR NEW.overage_enabled IS DISTINCT FROM OLD.overage_enabled
        OR NEW.published_at IS DISTINCT FROM OLD.published_at
    ) THEN
        RAISE EXCEPTION 'a published sold plan version is immutable';
    END IF;
    IF NEW.plan_version_id IS DISTINCT FROM OLD.plan_version_id
        OR NEW.plan_id IS DISTINCT FROM OLD.plan_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.version IS DISTINCT FROM OLD.version
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'sold plan identity is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER sold_plans_protected BEFORE UPDATE OR DELETE
    ON oxsum.sold_plans FOR EACH ROW EXECUTE FUNCTION oxsum.protect_sold_plan();

CREATE TABLE oxsum.customer_plans (
    customer_plan_id uuid PRIMARY KEY,
    organization_id  uuid NOT NULL REFERENCES oxsum.organizations(organization_id) ON DELETE RESTRICT,
    -- The exact sold version this customer bought — the binding never follows
    -- a later version silently.
    plan_version_id  uuid NOT NULL REFERENCES oxsum.sold_plans(plan_version_id) ON DELETE RESTRICT,
    -- The quota engine this entitlement runs on: one `org_subscriptions` row
    -- per `customer_plans` row, created when the first period activates. The
    -- draw/settle machinery and its `subscription_periods`/`subscription_draws`
    -- audit are reused unchanged.
    subscription_id  uuid REFERENCES oxsum.org_subscriptions(subscription_id) ON DELETE RESTRICT,
    -- D06: pending_payment → active → renewal_pending/past_due → active|expired,
    -- cancelled the explicit exit. `active` covers both the paid period and the
    -- still-serving remainder of one whose renewal is pending.
    state            text NOT NULL CHECK (state IN (
        'pending_payment', 'active', 'renewal_pending', 'past_due',
        'expired', 'cancelled')),
    activated_at     timestamptz,
    cancel_at_period_end boolean NOT NULL DEFAULT false,
    -- A booked plan-version swap effective at the next boundary — NULL when no
    -- change is scheduled. Authorized at the boundary, never mid-period.
    scheduled_change uuid REFERENCES oxsum.sold_plans(plan_version_id) ON DELETE RESTRICT,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);
-- At most one live plan per customer: the partial unique index is the
-- constraint bind/change transactions serialize on.
CREATE UNIQUE INDEX customer_plans_one_live
    ON oxsum.customer_plans (organization_id)
    WHERE state IN ('pending_payment', 'active', 'renewal_pending', 'past_due');
CREATE INDEX customer_plans_subscription
    ON oxsum.customer_plans (subscription_id) WHERE subscription_id IS NOT NULL;

CREATE TABLE oxsum.plan_periods (
    plan_period_id   uuid PRIMARY KEY,
    customer_plan_id uuid NOT NULL REFERENCES oxsum.customer_plans(customer_plan_id) ON DELETE RESTRICT,
    -- 0-based: period 0 is the activation period.
    period_index     integer NOT NULL CHECK (period_index >= 0),
    period_start     timestamptz NOT NULL,
    period_end       timestamptz NOT NULL,
    quota_minor      bigint CHECK (quota_minor IS NULL OR quota_minor > 0),
    consumed_minor   bigint NOT NULL DEFAULT 0 CHECK (consumed_minor >= 0),
    -- What authorized the period: 'payment_order' names the `payment_orders`
    -- row that funded it, 'complimentary' the free plan's grant key.
    authorization_kind text NOT NULL CHECK (authorization_kind IN ('payment_order', 'complimentary')),
    -- The authorizing fact's identity: the order id or the grant key's UUID.
    authorization_ref  uuid NOT NULL,
    state            text NOT NULL CHECK (state IN ('active', 'closed', 'expired')),
    created_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (customer_plan_id, period_index),
    -- A payment order authorizes at most one period.
    UNIQUE (authorization_kind, authorization_ref)
);
CREATE INDEX plan_periods_plan ON oxsum.plan_periods (customer_plan_id, period_index);
