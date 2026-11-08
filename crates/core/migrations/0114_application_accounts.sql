-- Stable application ownership never moves customers, ledgers or old grants.
CREATE TABLE oxsum.integration_applications (
    application_id uuid PRIMARY KEY,
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    application_key text NOT NULL CHECK (application_key ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND char_length(application_key)<=80),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    customer_limit integer NOT NULL CHECK (customer_limit BETWEEN 1 AND 10000),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (business_id,application_id),
    UNIQUE (business_id,application_key)
);
ALTER TABLE oxsum.integration_credentials ADD COLUMN application_id uuid;
ALTER TABLE oxsum.integration_credentials ADD FOREIGN KEY (business_id,application_id)
    REFERENCES oxsum.integration_applications(business_id,application_id);
ALTER TABLE oxsum.integration_credentials DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials ADD CONSTRAINT integration_credentials_scopes_check CHECK (
    cardinality(scopes) BETWEEN 1 AND 5 AND scopes <@ ARRAY['customersRead','meteringWrite','customersProvision','billingRead','customersLifecycleWrite']::text[]
    AND (application_id IS NOT NULL OR NOT ('customersLifecycleWrite'=ANY(scopes)))
);
CREATE TABLE oxsum.application_customers (
    business_id uuid NOT NULL,
    application_id uuid NOT NULL,
    external_id text NOT NULL CHECK (char_length(external_id) BETWEEN 1 AND 200),
    organization_id uuid NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (application_id,external_id),
    FOREIGN KEY (business_id,application_id) REFERENCES oxsum.integration_applications(business_id,application_id),
    FOREIGN KEY (business_id,organization_id) REFERENCES oxsum.customer_profiles(business_id,organization_id)
);
-- The namespace is the stable application, or the credential for old grants.
CREATE TABLE oxsum.integration_customer_operations (
    namespace_id uuid NOT NULL,
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 200),
    business_id uuid NOT NULL,
    credential_id uuid NOT NULL,
    action text NOT NULL CHECK (action IN ('provision','archive')),
    request jsonb NOT NULL,
    result jsonb NOT NULL,
    organization_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (namespace_id,idempotency_key),
    FOREIGN KEY (business_id,credential_id) REFERENCES oxsum.integration_credentials(business_id,credential_id),
    FOREIGN KEY (business_id,organization_id) REFERENCES oxsum.customer_profiles(business_id,organization_id)
);
CREATE FUNCTION oxsum.protect_application_identity() RETURNS trigger AS $guard$
BEGIN
    RAISE EXCEPTION 'application identity and operation history are immutable';
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER application_identity_immutable BEFORE UPDATE OR DELETE ON oxsum.integration_applications
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_application_identity();
CREATE TRIGGER application_customer_immutable BEFORE UPDATE OR DELETE ON oxsum.application_customers
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_application_identity();
CREATE TRIGGER integration_customer_operation_immutable BEFORE UPDATE OR DELETE ON oxsum.integration_customer_operations
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_application_identity();
CREATE FUNCTION oxsum.protect_credential_application() RETURNS trigger AS $guard$
BEGIN
    IF NEW.application_id IS DISTINCT FROM OLD.application_id THEN
        RAISE EXCEPTION 'credential application identity is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER credential_application_immutable BEFORE UPDATE ON oxsum.integration_credentials
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_credential_application();
