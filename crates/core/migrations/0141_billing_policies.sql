-- Exception policies are independent of the original sale snapshot encoding.
CREATE TABLE oxsum.billing_policy_versions (
    business_id uuid NOT NULL REFERENCES oxsum.businesses,
    version bigint NOT NULL CHECK(version>0),
    canonical text NOT NULL,
    digest text NOT NULL,
    mutation_id uuid NOT NULL REFERENCES oxsum.business_mutations,
    published_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(business_id,version),
    UNIQUE(business_id,version,digest)
);
CREATE TABLE oxsum.billing_policy_acceptances (
    acceptance_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    organization_id uuid NOT NULL,
    business_id uuid NOT NULL,
    version bigint NOT NULL,
    digest text NOT NULL,
    acceptance_revision bigint NOT NULL CHECK(acceptance_revision>0),
    UNIQUE(organization_id,acceptance_revision),
    accepted_by uuid NOT NULL REFERENCES oxsum.users,
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY(business_id,organization_id) REFERENCES oxsum.customer_profiles(business_id,organization_id),
    FOREIGN KEY(business_id,version,digest) REFERENCES oxsum.billing_policy_versions(business_id,version,digest)
);
CREATE INDEX billing_policy_customer ON oxsum.billing_policy_acceptances(organization_id,acceptance_revision DESC);
-- No FK to diagnostic operations: immutable financial context survives their retention.
-- Canonical bytes bind operation/customer/key/hold identity and reads verify scope.
CREATE TABLE oxsum.operation_billing_terms (
    operation_id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES oxsum.organizations,
    canonical text NOT NULL,
    digest text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TRIGGER billing_policy_versions_immutable BEFORE UPDATE OR DELETE ON oxsum.billing_policy_versions FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER billing_policy_acceptances_immutable BEFORE UPDATE OR DELETE ON oxsum.billing_policy_acceptances FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER operation_billing_terms_immutable BEFORE UPDATE OR DELETE ON oxsum.operation_billing_terms FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
