-- Widen the integration scope set with billingRead, the read-only scope that
-- lets a service credential read a granted customer's balance and bill log.
-- The set already carries customersProvision from 0105; this adds billingRead.
ALTER TABLE oxsum.integration_credentials
    DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials
    ADD CONSTRAINT integration_credentials_scopes_check CHECK (
        cardinality(scopes) BETWEEN 1 AND 4 AND
        scopes <@ ARRAY['customersRead','meteringWrite','customersProvision','billingRead']::text[]
    );
