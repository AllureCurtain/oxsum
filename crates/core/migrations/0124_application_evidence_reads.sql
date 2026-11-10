-- Existing grants gain no authority to export frozen customer evidence.
ALTER TABLE oxsum.integration_credentials DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials ADD CONSTRAINT integration_credentials_scopes_check CHECK (
    cardinality(scopes) BETWEEN 1 AND 9
    AND scopes <@ ARRAY['customersRead','meteringWrite','customersProvision','billingRead','customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite','evidenceRead']::text[]
    AND (application_id IS NOT NULL OR NOT (scopes && ARRAY['customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite','evidenceRead']::text[]))
);
