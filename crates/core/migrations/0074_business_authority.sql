-- Existing sessions keep customer authority until the person explicitly switches.
ALTER TABLE oxsum.sessions ADD COLUMN context text NOT NULL DEFAULT 'customer'
    CHECK (context IN ('customer', 'business'));

-- A personal session can outlive its customer membership without gaining any
-- customer authority. The user must explicitly select a current staff context.
ALTER TABLE oxsum.sessions ALTER COLUMN organization_id DROP NOT NULL;

-- A stable receipt identifies each security mutation. Scope is the session ID
-- for context selection, or the business ID for staff/policy changes.
CREATE TABLE oxsum.business_mutations (
    mutation_id uuid PRIMARY KEY,
    scope_id uuid NOT NULL,
    actor_id uuid NOT NULL REFERENCES oxsum.users(user_id),
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 200),
    action text NOT NULL,
    request jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (scope_id, actor_id, idempotency_key)
);
CREATE TRIGGER business_mutations_immutable BEFORE UPDATE OR DELETE ON oxsum.business_mutations
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

-- Security changes and their audit commit together and remain immutable.
CREATE OR REPLACE FUNCTION oxsum.protect_business_setup_audit() RETURNS trigger AS $audit$
BEGIN
    IF OLD.action LIKE 'business.%' OR OLD.action = 'session.context' THEN
        RAISE EXCEPTION 'business security audit is immutable';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.action LIKE 'business.%' OR NEW.action = 'session.context') THEN
        RAISE EXCEPTION 'business security audit is immutable';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$audit$ LANGUAGE plpgsql;
