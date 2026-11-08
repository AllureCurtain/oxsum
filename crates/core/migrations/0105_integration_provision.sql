-- Allow the customersProvision scope on integration credentials and raise the
-- scope-count bound for it. The grant itself stays a service-only, per-call
-- checked fact; this widens only what a credential may be issued.
ALTER TABLE oxsum.integration_credentials
    DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials
    ADD CONSTRAINT integration_credentials_scopes_check CHECK (
        cardinality(scopes) BETWEEN 1 AND 3 AND
        scopes <@ ARRAY['customersRead','meteringWrite','customersProvision']::text[]
    );
