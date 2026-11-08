-- Bounded gateway key lifecycle for service-managed application accounts.
ALTER TABLE oxsum.integration_credentials DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials ADD CONSTRAINT integration_credentials_scopes_check CHECK (
    cardinality(scopes) BETWEEN 1 AND 6
    AND scopes <@ ARRAY['customersRead','meteringWrite','customersProvision','billingRead','customersLifecycleWrite','customerKeysWrite']::text[]
    AND (application_id IS NOT NULL OR NOT (scopes && ARRAY['customersLifecycleWrite','customerKeysWrite']::text[]))
);
ALTER TABLE oxsum.application_customers ADD UNIQUE (business_id,application_id,organization_id);
ALTER TABLE oxsum.api_keys ADD UNIQUE (organization_id,key_id);
CREATE TABLE oxsum.application_customer_keys (
    key_id uuid PRIMARY KEY,
    business_id uuid NOT NULL,
    application_id uuid NOT NULL,
    organization_id uuid NOT NULL,
    credential_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (business_id,application_id,organization_id)
        REFERENCES oxsum.application_customers(business_id,application_id,organization_id),
    FOREIGN KEY (organization_id,key_id) REFERENCES oxsum.api_keys(organization_id,key_id),
    FOREIGN KEY (business_id,credential_id) REFERENCES oxsum.integration_credentials(business_id,credential_id)
);
CREATE TRIGGER application_key_identity_immutable BEFORE UPDATE OR DELETE ON oxsum.application_customer_keys
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_application_identity();
ALTER TABLE oxsum.integration_customer_operations DROP CONSTRAINT integration_customer_operations_action_check;
ALTER TABLE oxsum.integration_customer_operations ADD CONSTRAINT integration_customer_operations_action_check
    CHECK (action IN ('provision','archive','key.create','key.revoke'));
