-- V1 histories retain period-close recognition. Only newly published versions
-- select v2; each authorized v2 period snapshots its accounting terms forever.
ALTER TABLE oxsum.sold_plans ADD COLUMN accounting_policy_version smallint NOT NULL DEFAULT 1
    CHECK (accounting_policy_version IN (1,2));
ALTER TABLE oxsum.invoice_journals ADD COLUMN accounting_policy_version smallint NOT NULL DEFAULT 1
    CHECK (accounting_policy_version IN (1,2));

CREATE FUNCTION oxsum.protect_accounting_policy() RETURNS trigger AS $guard$
BEGIN
    IF NEW.accounting_policy_version IS DISTINCT FROM OLD.accounting_policy_version THEN
        RAISE EXCEPTION 'accounting policy history is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER sold_plan_accounting_policy BEFORE UPDATE ON oxsum.sold_plans
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_accounting_policy();
CREATE TRIGGER journal_accounting_policy BEFORE UPDATE ON oxsum.invoice_journals
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_accounting_policy();

CREATE TABLE oxsum.plan_accounting_periods (
    plan_period_id uuid PRIMARY KEY REFERENCES oxsum.plan_periods(plan_period_id),
    journal_id uuid NOT NULL UNIQUE REFERENCES oxsum.invoice_journals(journal_id),
    customer_plan_id uuid NOT NULL REFERENCES oxsum.customer_plans(customer_plan_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    plan_version_id uuid NOT NULL REFERENCES oxsum.sold_plans(plan_version_id),
    accounting_policy_version smallint NOT NULL CHECK (accounting_policy_version=2),
    fee_minor bigint NOT NULL CHECK (fee_minor>=0),
    service_start timestamptz NOT NULL,
    service_end timestamptz NOT NULL CHECK (service_end>service_start AND service_end-service_start<=interval '32 days'),
    authorization_kind text NOT NULL CHECK (authorization_kind IN ('payment_order','complimentary')),
    authorization_ref uuid NOT NULL,
    payment_on date NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (authorization_kind,authorization_ref)
);
CREATE INDEX plan_accounting_periods_work ON oxsum.plan_accounting_periods(service_end,plan_period_id);

CREATE TABLE oxsum.plan_accounting_effects (
    effect_id uuid PRIMARY KEY,
    plan_period_id uuid NOT NULL REFERENCES oxsum.plan_accounting_periods(plan_period_id),
    kind text NOT NULL CHECK (kind IN ('activation','recognition','refund','dispute','void')),
    source_ref uuid NOT NULL,
    service_day date NOT NULL,
    booked_on date NOT NULL,
    payment_on date NOT NULL,
    -- Signed changes, positive means an increase in the named financial fact.
    billed_minor bigint NOT NULL DEFAULT 0,
    earned_minor bigint NOT NULL DEFAULT 0,
    deferred_minor bigint NOT NULL DEFAULT 0,
    collected_minor bigint NOT NULL DEFAULT 0,
    receivable_minor bigint NOT NULL DEFAULT 0,
    payable_minor bigint NOT NULL DEFAULT 0,
    cash_minor bigint NOT NULL DEFAULT 0,
    entry_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (billed_minor::numeric=earned_minor::numeric+deferred_minor::numeric),
    CHECK (receivable_minor::numeric=billed_minor::numeric-collected_minor::numeric),
    UNIQUE (plan_period_id,kind,source_ref,service_day)
);
CREATE UNIQUE INDEX plan_accounting_recognition_day ON oxsum.plan_accounting_effects(plan_period_id,service_day)
    WHERE kind='recognition';
CREATE UNIQUE INDEX plan_accounting_source_effect ON oxsum.plan_accounting_effects(plan_period_id,kind,source_ref)
    WHERE kind<>'recognition';
CREATE INDEX plan_accounting_effects_booking ON oxsum.plan_accounting_effects(booked_on,plan_period_id);

CREATE FUNCTION oxsum.protect_plan_accounting_history() RETURNS trigger AS $guard$
BEGIN
    RAISE EXCEPTION 'plan accounting history is immutable';
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER plan_accounting_period_immutable BEFORE UPDATE OR DELETE ON oxsum.plan_accounting_periods
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_accounting_history();
CREATE TRIGGER plan_accounting_effect_immutable BEFORE UPDATE OR DELETE ON oxsum.plan_accounting_effects
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_accounting_history();

CREATE FUNCTION oxsum.protect_accounted_plan_period() RETURNS trigger AS $guard$
BEGIN
    IF EXISTS (SELECT 1 FROM oxsum.plan_accounting_periods WHERE plan_period_id=OLD.plan_period_id) THEN
        IF TG_OP='DELETE' THEN RAISE EXCEPTION 'accounted period history is retained'; END IF;
        IF NEW.plan_period_id IS DISTINCT FROM OLD.plan_period_id
            OR NEW.customer_plan_id IS DISTINCT FROM OLD.customer_plan_id
            OR NEW.period_index IS DISTINCT FROM OLD.period_index
            OR NEW.period_start IS DISTINCT FROM OLD.period_start
            OR NEW.period_end IS DISTINCT FROM OLD.period_end
            OR NEW.quota_minor IS DISTINCT FROM OLD.quota_minor
            OR NEW.authorization_kind IS DISTINCT FROM OLD.authorization_kind
            OR NEW.authorization_ref IS DISTINCT FROM OLD.authorization_ref
            OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
            RAISE EXCEPTION 'accounted period terms are immutable';
        END IF;
    END IF;
    IF TG_OP='DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER accounted_plan_period_immutable BEFORE UPDATE OR DELETE ON oxsum.plan_periods
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_accounted_plan_period();
