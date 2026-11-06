-- Routing chains previously relied on deployment-global ownership. Preserve
-- their identity/order while attaching the same business as legacy channels.
ALTER TABLE oxsum.model_fallbacks ADD COLUMN business_id uuid NOT NULL
    DEFAULT oxsum.default_business_id() REFERENCES oxsum.businesses(business_id);
CREATE INDEX model_fallbacks_business ON oxsum.model_fallbacks(business_id, model, position);

-- An exact retry returns its original resource/version after later edits.
-- Request fingerprints live in business_mutations; no secret plaintext does.
CREATE TABLE oxsum.provider_mutation_results (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    resource_id uuid,
    version bigint
);
CREATE TRIGGER provider_mutation_results_immutable BEFORE UPDATE OR DELETE
    ON oxsum.provider_mutation_results
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

-- Configuration identity is never transferred across businesses. Existing
-- price versions and in-flight references keep their original channel owner.
CREATE FUNCTION oxsum.protect_channel_owner() RETURNS trigger AS $guard$
BEGIN
    IF NEW.channel_id IS DISTINCT FROM OLD.channel_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.name IS DISTINCT FROM OLD.name THEN
        RAISE EXCEPTION 'channel identity and business ownership are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER channels_owner_immutable BEFORE UPDATE ON oxsum.channels
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_channel_owner();
