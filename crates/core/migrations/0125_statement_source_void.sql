-- Each source-specific correction retains the original line, its exact opposite
-- and the canonical financial void. Original statements/effects remain immutable.
CREATE TABLE oxsum.statement_source_voids (
    original_line_id uuid PRIMARY KEY REFERENCES oxsum.plan_statement_lines(line_id),
    reversal_line_id uuid NOT NULL UNIQUE REFERENCES oxsum.plan_statement_lines(line_id),
    statement_id uuid NOT NULL REFERENCES oxsum.plan_statements(statement_id),
    journal_id uuid NOT NULL REFERENCES oxsum.invoice_journals(journal_id),
    entry_id uuid NOT NULL,
    entry_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK(original_line_id<>reversal_line_id)
);
CREATE INDEX statement_source_voids_document ON oxsum.statement_source_voids(statement_id);
CREATE TRIGGER statement_source_voids_immutable BEFORE UPDATE OR DELETE ON oxsum.statement_source_voids
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
