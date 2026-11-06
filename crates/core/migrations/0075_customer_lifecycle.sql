-- Keep each admission blocker independent. Existing suspended_at remains the
-- manual suspension marker, and dunning retains only its own timestamp.
UPDATE oxsum.organizations SET suspended_at = NULL
    WHERE dunning_suspended_at IS NOT NULL;

ALTER TABLE oxsum.customer_profiles ADD COLUMN archived_at timestamptz;
UPDATE oxsum.customer_profiles SET archived_at=now() WHERE state='archived';
ALTER TABLE oxsum.customer_profiles ADD CONSTRAINT customer_archive_timestamp
    CHECK ((state='archived')=(archived_at IS NOT NULL));

-- Mapping and external identity are permanent; changing a display name never
-- reassigns a customer's ledger or historical evidence to another business.
CREATE FUNCTION oxsum.protect_customer_identity() RETURNS trigger AS $identity$
BEGIN
    IF NEW.organization_id <> OLD.organization_id OR NEW.business_id <> OLD.business_id
       OR NEW.external_id <> OLD.external_id THEN
        RAISE EXCEPTION 'customer identity is immutable';
    END IF;
    RETURN NEW;
END;
$identity$ LANGUAGE plpgsql;
CREATE TRIGGER customer_profiles_protect_identity BEFORE UPDATE ON oxsum.customer_profiles
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_customer_identity();

CREATE INDEX customer_profiles_business_cursor ON oxsum.customer_profiles (business_id, organization_id);
