-- Replace implicit deployment-wide metering authority with explicit business,
-- scope and customer grants. Legacy identities/hashes remain audit history.
UPDATE oxsum.service_credentials SET revoked_at = COALESCE(revoked_at, now());

CREATE TABLE oxsum.integration_credentials (
    credential_id uuid PRIMARY KEY,
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    name text CHECK (char_length(name) BETWEEN 1 AND 80),
    prefix text NOT NULL,
    secret_hash bytea NOT NULL UNIQUE,
    scopes text[] NOT NULL CHECK (
        cardinality(scopes) BETWEEN 1 AND 2 AND
        scopes <@ ARRAY['customersRead','meteringWrite']::text[]
    ),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    revoked_at timestamptz,
    UNIQUE (business_id, credential_id)
);
CREATE TABLE oxsum.integration_credential_customers (
    business_id uuid NOT NULL,
    credential_id uuid NOT NULL,
    organization_id uuid NOT NULL,
    PRIMARY KEY (credential_id, organization_id),
    FOREIGN KEY (business_id, credential_id)
        REFERENCES oxsum.integration_credentials(business_id, credential_id),
    FOREIGN KEY (business_id, organization_id)
        REFERENCES oxsum.customer_profiles(business_id, organization_id)
);
CREATE INDEX integration_credentials_business ON oxsum.integration_credentials(business_id, credential_id);

-- A committed authorization pins the caller and request before any ledger
-- effect. This is not the full serving-operation state machine (T10-T13).
CREATE TABLE oxsum.metering_authorizations (
    hold_key text PRIMARY KEY,
    business_id uuid NOT NULL,
    credential_id uuid NOT NULL,
    organization_id uuid NOT NULL,
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 200),
    request jsonb NOT NULL,
    admission jsonb NOT NULL,
    finalization jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (credential_id, organization_id, idempotency_key),
    FOREIGN KEY (business_id, credential_id)
        REFERENCES oxsum.integration_credentials(business_id, credential_id),
    FOREIGN KEY (business_id, organization_id)
        REFERENCES oxsum.customer_profiles(business_id, organization_id)
);
CREATE FUNCTION oxsum.protect_metering_authorization() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'metering authorizations are immutable';
    END IF;
    IF (to_jsonb(NEW) - 'finalization') IS DISTINCT FROM (to_jsonb(OLD) - 'finalization')
        OR (OLD.finalization IS NOT NULL AND NEW.finalization IS DISTINCT FROM OLD.finalization) THEN
        RAISE EXCEPTION 'metering authorization terms are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER metering_authorizations_immutable BEFORE UPDATE OR DELETE ON oxsum.metering_authorizations
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_metering_authorization();
