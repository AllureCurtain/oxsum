-- Customer display preferences are not authority to set purchased credit value.
CREATE TABLE oxsum.collection_terms (
    business_id uuid PRIMARY KEY REFERENCES oxsum.businesses(business_id),
    revision bigint NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
    currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    rate_per_credit text NOT NULL CHECK (rate_per_credit ~ '^[0-9]+(\.[0-9]+)?$'),
    source text NOT NULL CHECK (source IN ('environment','owner')),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE oxsum.collection_term_revisions (
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    revision bigint NOT NULL,
    mutation_id uuid UNIQUE REFERENCES oxsum.business_mutations(mutation_id),
    result jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (business_id,revision)
);
CREATE TRIGGER collection_term_revisions_immutable BEFORE UPDATE OR DELETE
    ON oxsum.collection_term_revisions FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

-- Adoption is explicit at boot or through owner consent. Historical order
-- snapshots, display preferences, provider bindings and ledger entries stay intact.
