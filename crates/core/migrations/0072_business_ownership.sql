-- Preserve organizations as customer billing accounts. This adds seller
-- ownership without modifying any tenant ledger, price history or key identity.
CREATE TABLE oxsum.businesses (
    business_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    singleton boolean NOT NULL DEFAULT true CHECK (singleton) UNIQUE,
    name text NOT NULL DEFAULT 'My business',
    registration_mode text NOT NULL DEFAULT 'invite' CHECK (registration_mode IN ('invite', 'open')),
    initialized_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO oxsum.businesses DEFAULT VALUES;

CREATE FUNCTION oxsum.default_business_id() RETURNS uuid
    LANGUAGE sql STABLE AS 'SELECT business_id FROM oxsum.businesses WHERE singleton';

-- A customer's owner is never implicitly appointed business staff.
CREATE TABLE oxsum.business_staff (
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    user_id uuid NOT NULL REFERENCES oxsum.users(user_id),
    role text NOT NULL CHECK (role IN ('owner', 'admin', 'finance', 'support', 'viewer')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (business_id, user_id)
);

CREATE TABLE oxsum.customer_profiles (
    organization_id uuid PRIMARY KEY REFERENCES oxsum.organizations(organization_id),
    business_id uuid NOT NULL DEFAULT oxsum.default_business_id() REFERENCES oxsum.businesses(business_id),
    external_id text NOT NULL,
    contact_email text,
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'archived')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (business_id, external_id),
    UNIQUE (business_id, organization_id)
);
INSERT INTO oxsum.customer_profiles (organization_id, external_id)
    SELECT organization_id, organization_id::text FROM oxsum.organizations;

-- All organization creation paths (signup, invitations, seed, teams) retain the
-- same identity and acquire ownership in the creating transaction.
CREATE FUNCTION oxsum.assign_customer_business() RETURNS trigger AS $assign$
BEGIN
    INSERT INTO oxsum.customer_profiles (organization_id, external_id)
        VALUES (NEW.organization_id, NEW.organization_id::text);
    RETURN NEW;
END;
$assign$ LANGUAGE plpgsql;
CREATE TRIGGER organizations_assign_business AFTER INSERT ON oxsum.organizations
    FOR EACH ROW EXECUTE FUNCTION oxsum.assign_customer_business();

ALTER TABLE oxsum.channels ADD COLUMN business_id uuid
    NOT NULL DEFAULT oxsum.default_business_id() REFERENCES oxsum.businesses(business_id);
ALTER TABLE oxsum.subscription_plans ADD COLUMN business_id uuid
    NOT NULL DEFAULT oxsum.default_business_id() REFERENCES oxsum.businesses(business_id);
-- Price versions, aliases and credentials inherit channel ownership; plan
-- periods inherit plan ownership. Collection history inherits customer ownership.
CREATE INDEX channels_business ON oxsum.channels (business_id, channel_id);
CREATE INDEX subscription_plans_business ON oxsum.subscription_plans (business_id, plan_id);

CREATE TABLE oxsum.business_migrations (
    version text PRIMARY KEY,
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    customers bigint NOT NULL,
    channels bigint NOT NULL,
    plans bigint NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO oxsum.business_migrations (version, business_id, customers, channels, plans)
    SELECT '0072_business_ownership', oxsum.default_business_id(),
        (SELECT count(*) FROM oxsum.customer_profiles),
        (SELECT count(*) FROM oxsum.channels),
        (SELECT count(*) FROM oxsum.subscription_plans);

CREATE FUNCTION oxsum.business_migrations_are_immutable() RETURNS trigger AS $immutable$
BEGIN
    RAISE EXCEPTION 'business migration receipts are immutable';
END;
$immutable$ LANGUAGE plpgsql;
CREATE TRIGGER business_migrations_immutable BEFORE UPDATE OR DELETE ON oxsum.business_migrations
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
