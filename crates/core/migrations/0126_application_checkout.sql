-- Explicit application grant; existing credentials gain no checkout authority.
ALTER TABLE oxsum.integration_credentials DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials ADD CONSTRAINT integration_credentials_scopes_check CHECK (
    cardinality(scopes) BETWEEN 1 AND 10
    AND scopes <@ ARRAY['customersRead','meteringWrite','customersProvision','billingRead','customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite','evidenceRead','checkoutWrite']::text[]
    AND (application_id IS NOT NULL OR NOT (scopes && ARRAY['customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite','evidenceRead','checkoutWrite']::text[]))
);
CREATE TABLE oxsum.application_checkout_orders (
    order_id uuid PRIMARY KEY REFERENCES oxsum.payment_orders(order_id),
    application_id uuid NOT NULL REFERENCES oxsum.integration_applications(application_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    credential_id uuid NOT NULL REFERENCES oxsum.integration_credentials(credential_id),
    request jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX application_checkout_history ON oxsum.application_checkout_orders(application_id,organization_id,order_id);
CREATE TRIGGER application_checkout_immutable BEFORE UPDATE OR DELETE ON oxsum.application_checkout_orders
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
