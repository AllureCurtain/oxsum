-- Exact supplier material and independently frozen comparisons; no customer postings.
CREATE TABLE oxsum.supplier_statement_sources (
 source_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
 business_id uuid NOT NULL REFERENCES oxsum.businesses,
 scope_key text NOT NULL,
 source_version bigint NOT NULL,
 bill_date date NOT NULL,
 request jsonb NOT NULL,
 parsed jsonb NOT NULL,
 digest text NOT NULL,
 canonical text NOT NULL,
 channel_snapshot jsonb NOT NULL,
 captured_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(business_id,scope_key,source_version)
);
CREATE TABLE oxsum.supplier_statement_comparisons (
 comparison_id uuid PRIMARY KEY,
 source_id uuid NOT NULL REFERENCES oxsum.supplier_statement_sources,
 mutation_id uuid NOT NULL UNIQUE REFERENCES oxsum.business_mutations,
 document jsonb NOT NULL,
 digest text NOT NULL,
 canonical text NOT NULL,
 captured_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX supplier_statement_history ON oxsum.supplier_statement_sources(business_id,captured_at,source_id);
CREATE TRIGGER supplier_statement_sources_immutable BEFORE UPDATE OR DELETE ON oxsum.supplier_statement_sources FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER supplier_statement_comparisons_immutable BEFORE UPDATE OR DELETE ON oxsum.supplier_statement_comparisons FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
