-- Evidence publication is explicit; existing readers gain no mutation authority.
ALTER TABLE oxsum.integration_credentials DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials ADD CONSTRAINT integration_credentials_scopes_check CHECK (
    cardinality(scopes) BETWEEN 1 AND 11
    AND scopes <@ ARRAY['customersRead','meteringWrite','customersProvision','billingRead','customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite','evidenceRead','checkoutWrite','evidenceWrite']::text[]
    AND (application_id IS NOT NULL OR NOT (scopes && ARRAY['customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite','evidenceRead','checkoutWrite','evidenceWrite']::text[]))
);
CREATE TABLE oxsum.application_evidence_publications (
    request_id uuid PRIMARY KEY,
    application_id uuid NOT NULL REFERENCES oxsum.integration_applications(application_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    credential_id uuid NOT NULL REFERENCES oxsum.integration_credentials(credential_id),
    idempotency_key text NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 200),
    request jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(application_id,idempotency_key)
);
CREATE TRIGGER application_evidence_publication_immutable BEFORE UPDATE OR DELETE ON oxsum.application_evidence_publications
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
