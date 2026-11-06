-- Merchant identity is stable across credential rotation. Test and live are
-- separate namespaces even when a provider uses the same account identifier.
CREATE TABLE oxsum.merchant_accounts (
    merchant_id uuid PRIMARY KEY,
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    rail text NOT NULL CHECK (rail IN ('hyperswitch', 'stripe', 'airwallex')),
    mode text NOT NULL CHECK (mode IN ('test', 'live')),
    account_id text NOT NULL CHECK (length(account_id) BETWEEN 1 AND 200),
    current_version_id uuid NOT NULL,
    enabled boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (business_id, name),
    UNIQUE (business_id, rail, mode, account_id)
);
CREATE TABLE oxsum.merchant_versions (
    version_id uuid PRIMARY KEY,
    merchant_id uuid NOT NULL REFERENCES oxsum.merchant_accounts(merchant_id),
    revision bigint NOT NULL CHECK (revision > 0),
    api_url text NOT NULL,
    publishable_key text,
    sdk_url text,
    checkout_url text,
    destination_policy jsonb NOT NULL,
    credentials_sealed text NOT NULL,
    source text NOT NULL CHECK (source IN ('owner', 'environment')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (merchant_id, version_id),
    UNIQUE (merchant_id, revision)
);
ALTER TABLE oxsum.merchant_accounts ADD CONSTRAINT merchant_current_version_owned
    FOREIGN KEY (merchant_id, current_version_id)
    REFERENCES oxsum.merchant_versions(merchant_id, version_id)
    DEFERRABLE INITIALLY DEFERRED;
CREATE TRIGGER merchant_versions_immutable BEFORE UPDATE OR DELETE
    ON oxsum.merchant_versions FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE FUNCTION oxsum.protect_merchant_identity() RETURNS trigger AS $guard$
BEGIN
    IF NEW.merchant_id IS DISTINCT FROM OLD.merchant_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.name IS DISTINCT FROM OLD.name
        OR NEW.rail IS DISTINCT FROM OLD.rail
        OR NEW.mode IS DISTINCT FROM OLD.mode
        OR NEW.account_id IS DISTINCT FROM OLD.account_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'merchant identity and business ownership are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER merchant_identity_immutable BEFORE UPDATE ON oxsum.merchant_accounts
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_merchant_identity();

-- Preserve the original response to a retry after later rotations/disablement.
CREATE TABLE oxsum.merchant_mutation_results (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    merchant_id uuid NOT NULL,
    version_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    enabled boolean NOT NULL,
    FOREIGN KEY (merchant_id, version_id)
        REFERENCES oxsum.merchant_versions(merchant_id, version_id)
);
CREATE TRIGGER merchant_mutation_results_immutable BEFORE UPDATE OR DELETE
    ON oxsum.merchant_mutation_results FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
