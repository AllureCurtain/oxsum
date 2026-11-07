-- The marker distinguishes unsaved deployment defaults from explicit nulls.
-- Existing setup names and all customer/ledger/document data are preserved.
ALTER TABLE oxsum.businesses
    ADD COLUMN profile_revision bigint NOT NULL DEFAULT 0
        CHECK (profile_revision BETWEEN 0 AND 9007199254740991),
    ADD COLUMN contact_email text,
    ADD COLUMN help_url text,
    ADD COLUMN logo_url text,
    ADD COLUMN accent_color text,
    ADD COLUMN profile_updated_at timestamptz;

-- An exact retry returns its original snapshot, even after a later edit.
CREATE TABLE oxsum.business_profile_revisions (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    revision bigint NOT NULL CHECK (revision > 0),
    result jsonb NOT NULL,
    UNIQUE (business_id, revision)
);
CREATE TRIGGER business_profile_revisions_immutable
    BEFORE UPDATE OR DELETE ON oxsum.business_profile_revisions
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
