-- Original period/payment ownership remains immutable across invoice generations.
CREATE TABLE oxsum.invoice_corrections (
    correction_id uuid PRIMARY KEY,
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    actor_user_id uuid NOT NULL REFERENCES oxsum.users(user_id),
    predecessor_journal_id uuid NOT NULL UNIQUE REFERENCES oxsum.invoice_journals(journal_id),
    predecessor_statement_id uuid NOT NULL UNIQUE REFERENCES oxsum.plan_statements(statement_id),
    journal_id uuid NOT NULL UNIQUE REFERENCES oxsum.invoice_journals(journal_id),
    statement_id uuid NOT NULL UNIQUE REFERENCES oxsum.plan_statements(statement_id),
    source_revision text NOT NULL CHECK(source_revision ~ '^[0-9a-f]{64}$'),
    idempotency_key text NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 200),
    reason text NOT NULL CHECK(length(reason) BETWEEN 1 AND 500),
    request jsonb NOT NULL CHECK(jsonb_typeof(request)='object'),
    result jsonb NOT NULL CHECK(jsonb_typeof(result)='object'),
    entry_id uuid,
    entry_hash text,
    ledger_boundary bigint NOT NULL CHECK(ledger_boundary>=0),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK(predecessor_journal_id<>journal_id),
    CHECK(predecessor_statement_id<>statement_id),
    CHECK((entry_id IS NULL)=(entry_hash IS NULL)),
    UNIQUE(business_id,actor_user_id,idempotency_key)
);
CREATE TRIGGER invoice_correction_immutable BEFORE UPDATE OR DELETE ON oxsum.invoice_corrections
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_accounting_history();

CREATE TABLE oxsum.invoice_correction_lines (
    line_id uuid PRIMARY KEY REFERENCES oxsum.invoice_journal_lines(line_id),
    correction_id uuid NOT NULL REFERENCES oxsum.invoice_corrections(correction_id),
    source_line_id uuid NOT NULL REFERENCES oxsum.invoice_journal_lines(line_id),
    original_minor bigint NOT NULL,
    corrected_minor bigint NOT NULL,
    UNIQUE(correction_id,source_line_id)
);
CREATE TRIGGER invoice_correction_line_immutable BEFORE UPDATE OR DELETE ON oxsum.invoice_correction_lines
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_accounting_history();

-- Both policies retain the original sold interval. Only policy 2 participates in
-- daily recognition; corrected journals deliberately have NULL plan_period_id.
CREATE TABLE oxsum.invoice_accounting_generations (
    journal_id uuid PRIMARY KEY REFERENCES oxsum.invoice_journals(journal_id),
    correction_id uuid NOT NULL UNIQUE REFERENCES oxsum.invoice_corrections(correction_id),
    predecessor_journal_id uuid NOT NULL UNIQUE REFERENCES oxsum.invoice_journals(journal_id),
    original_journal_id uuid NOT NULL REFERENCES oxsum.invoice_journals(journal_id),
    plan_period_id uuid NOT NULL REFERENCES oxsum.plan_periods(plan_period_id),
    customer_plan_id uuid NOT NULL REFERENCES oxsum.customer_plans(customer_plan_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    plan_version_id uuid NOT NULL REFERENCES oxsum.sold_plans(plan_version_id),
    accounting_policy_version smallint NOT NULL CHECK(accounting_policy_version IN (1,2)),
    fee_minor bigint NOT NULL CHECK(fee_minor>=0),
    service_start timestamptz NOT NULL,
    service_end timestamptz NOT NULL CHECK(service_end>service_start),
    recognition_from timestamptz NOT NULL,
    payment_on date NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK(recognition_from>=service_start AND recognition_from<=service_end)
);
CREATE INDEX invoice_accounting_generation_period ON oxsum.invoice_accounting_generations(plan_period_id);
CREATE TRIGGER invoice_accounting_generation_immutable BEFORE UPDATE OR DELETE ON oxsum.invoice_accounting_generations
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_accounting_history();

-- NULL is the original generation. Do not rewrite immutable historical effects.
ALTER TABLE oxsum.plan_accounting_effects
    ADD COLUMN journal_id uuid REFERENCES oxsum.invoice_accounting_generations(journal_id);
ALTER TABLE oxsum.plan_accounting_effects DROP CONSTRAINT plan_accounting_effects_kind_check;
ALTER TABLE oxsum.plan_accounting_effects ADD CHECK(kind IN
    ('activation','recognition','refund','dispute','void','statement_payment','statement_return','correction'));
ALTER TABLE oxsum.plan_accounting_effects
    DROP CONSTRAINT plan_accounting_effects_plan_period_id_kind_source_ref_serv_key;
DROP INDEX oxsum.plan_accounting_recognition_day;
DROP INDEX oxsum.plan_accounting_source_effect;
CREATE UNIQUE INDEX plan_accounting_recognition_day
    ON oxsum.plan_accounting_effects(plan_period_id,journal_id,service_day) NULLS NOT DISTINCT
    WHERE kind='recognition';
CREATE UNIQUE INDEX plan_accounting_source_effect
    ON oxsum.plan_accounting_effects(plan_period_id,journal_id,kind,source_ref) NULLS NOT DISTINCT
    WHERE kind<>'recognition';
CREATE FUNCTION oxsum.check_accounting_generation() RETURNS trigger AS $guard$
BEGIN
    IF NEW.journal_id IS NOT NULL AND NOT EXISTS(
        SELECT 1 FROM oxsum.invoice_accounting_generations g
        WHERE g.journal_id=NEW.journal_id AND g.plan_period_id=NEW.plan_period_id
          AND g.accounting_policy_version=2
    ) THEN RAISE EXCEPTION 'accounting effect belongs to another generation'; END IF;
    IF (NEW.kind='correction' AND NEW.journal_id IS NULL)
       OR (NEW.kind='activation' AND NEW.journal_id IS NOT NULL) THEN
        RAISE EXCEPTION 'accounting opening kind differs from its generation';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER plan_accounting_generation_check BEFORE INSERT ON oxsum.plan_accounting_effects
    FOR EACH ROW EXECUTE FUNCTION oxsum.check_accounting_generation();

CREATE VIEW oxsum.plan_recognition_generations AS
    SELECT a.plan_period_id,a.journal_id,a.customer_plan_id,a.organization_id,
        a.fee_minor,a.service_start,a.service_end,a.payment_on,
        a.service_start AS recognition_from,NULL::uuid AS effect_journal_id
    FROM oxsum.plan_accounting_periods a
    UNION ALL
    SELECT g.plan_period_id,g.journal_id,g.customer_plan_id,g.organization_id,
        g.fee_minor,g.service_start,g.service_end,g.payment_on,
        g.recognition_from,g.journal_id AS effect_journal_id
    FROM oxsum.invoice_accounting_generations g WHERE g.accounting_policy_version=2;
