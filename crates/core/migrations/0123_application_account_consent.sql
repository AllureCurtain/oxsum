-- Existing application credentials gain no implicit linking authority.
ALTER TABLE oxsum.integration_credentials DROP CONSTRAINT integration_credentials_scopes_check;
ALTER TABLE oxsum.integration_credentials ADD CONSTRAINT integration_credentials_scopes_check CHECK (
    cardinality(scopes) BETWEEN 1 AND 8
    AND scopes <@ ARRAY['customersRead','meteringWrite','customersProvision','billingRead','customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite']::text[]
    AND (application_id IS NOT NULL OR NOT (scopes && ARRAY['customersLifecycleWrite','customerKeysWrite','entitlementsRead','customerLinkWrite']::text[]))
);
ALTER TABLE oxsum.integration_customer_operations DROP CONSTRAINT integration_customer_operations_action_check;
ALTER TABLE oxsum.integration_customer_operations ADD CONSTRAINT integration_customer_operations_action_check
    CHECK (action IN ('provision','archive','key.create','key.revoke','link.create','link.revoke'));
CREATE TABLE oxsum.application_link_invitations (
    invitation_id uuid PRIMARY KEY,
    business_id uuid NOT NULL,
    application_id uuid NOT NULL,
    organization_id uuid NOT NULL,
    credential_id uuid NOT NULL,
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash)=32),
    intended_email text NOT NULL CHECK (length(intended_email) BETWEEN 3 AND 320 AND intended_email=lower(intended_email)),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL DEFAULT clock_timestamp()+interval '7 days',
    CHECK (expires_at>created_at),
    FOREIGN KEY (business_id,application_id,organization_id)
        REFERENCES oxsum.application_customers(business_id,application_id,organization_id),
    FOREIGN KEY (business_id,credential_id)
        REFERENCES oxsum.integration_credentials(business_id,credential_id)
);
CREATE INDEX application_link_customer ON oxsum.application_link_invitations(organization_id);
-- A terminal result is append-only; a replay can never recreate membership.
CREATE TABLE oxsum.application_link_resolutions (
    invitation_id uuid PRIMARY KEY REFERENCES oxsum.application_link_invitations(invitation_id),
    kind text NOT NULL CHECK (kind IN ('accepted','revoked')),
    user_id uuid REFERENCES oxsum.users(user_id),
    resolved_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((kind='accepted')=(user_id IS NOT NULL))
);
CREATE TRIGGER application_link_invitation_immutable BEFORE UPDATE OR DELETE ON oxsum.application_link_invitations
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_application_identity();
CREATE TRIGGER application_link_resolution_immutable BEFORE UPDATE OR DELETE ON oxsum.application_link_resolutions
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_application_identity();
