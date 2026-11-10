-- Preserve the result even when an empty month later acquires usage or a draft
-- advances. Exact retries never repeat financial effects or return a new id.
CREATE TABLE oxsum.monthly_statement_actions (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    statement_id uuid REFERENCES oxsum.statements(statement_id),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER monthly_statement_actions_immutable BEFORE UPDATE OR DELETE ON oxsum.monthly_statement_actions
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
