-- An expected payment is durable before any external request. T15 orders can
-- use their own UUID; the legacy adapter uses its existing deposit UUID.
CREATE TABLE oxsum.merchant_payment_bindings (
    payment_id uuid PRIMARY KEY,
    merchant_id uuid NOT NULL,
    version_id uuid NOT NULL,
    organization_id uuid NOT NULL REFERENCES oxsum.customer_profiles(organization_id),
    amount_minor bigint NOT NULL CHECK (amount_minor > 0),
    currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    purpose text NOT NULL CHECK (purpose IN ('order', 'legacyDeposit')),
    legacy_unscoped_key boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (merchant_id, version_id)
        REFERENCES oxsum.merchant_versions(merchant_id, version_id)
);
CREATE TRIGGER merchant_payment_bindings_immutable BEFORE UPDATE OR DELETE
    ON oxsum.merchant_payment_bindings FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

-- A signed early callback can establish the same reference that the later
-- synchronous response confirms. One provider payment cannot fund two orders.
CREATE TABLE oxsum.merchant_payment_refs (
    payment_id uuid PRIMARY KEY REFERENCES oxsum.merchant_payment_bindings(payment_id),
    merchant_id uuid NOT NULL REFERENCES oxsum.merchant_accounts(merchant_id),
    provider_payment_id text NOT NULL CHECK (length(provider_payment_id) BETWEEN 1 AND 200),
    UNIQUE (merchant_id, provider_payment_id)
);
CREATE TRIGGER merchant_payment_refs_immutable BEFORE UPDATE OR DELETE
    ON oxsum.merchant_payment_refs FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TABLE oxsum.merchant_event_inbox (
    receipt_id uuid PRIMARY KEY,
    merchant_id uuid NOT NULL,
    version_id uuid NOT NULL,
    event_id text NOT NULL CHECK (length(event_id) BETWEEN 1 AND 200),
    payload_digest bytea NOT NULL CHECK (octet_length(payload_digest)=32),
    normalized jsonb NOT NULL,
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','accepted','applied','rejected','ignored')),
    reason text,
    received_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (merchant_id, event_id),
    FOREIGN KEY (merchant_id, version_id)
        REFERENCES oxsum.merchant_versions(merchant_id, version_id)
);
CREATE INDEX merchant_events_pending ON oxsum.merchant_event_inbox(state,received_at);
CREATE FUNCTION oxsum.protect_merchant_event() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'merchant event history is retained'; END IF;
    IF NEW.receipt_id IS DISTINCT FROM OLD.receipt_id
        OR NEW.merchant_id IS DISTINCT FROM OLD.merchant_id
        OR NEW.version_id IS DISTINCT FROM OLD.version_id
        OR NEW.event_id IS DISTINCT FROM OLD.event_id
        OR NEW.payload_digest IS DISTINCT FROM OLD.payload_digest
        OR NEW.normalized IS DISTINCT FROM OLD.normalized
        OR NEW.received_at IS DISTINCT FROM OLD.received_at
        OR (OLD.state IN ('applied','rejected','ignored') AND NEW.state IS DISTINCT FROM OLD.state)
        OR (OLD.state='accepted' AND NEW.state NOT IN ('accepted','applied')) THEN
        RAISE EXCEPTION 'merchant event identity and terminal states are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER merchant_events_protected BEFORE UPDATE OR DELETE
    ON oxsum.merchant_event_inbox FOR EACH ROW EXECUTE FUNCTION oxsum.protect_merchant_event();

-- Explicit environment rail mapping. Bootstrap never selects a payload's ID,
-- and does not change the version when environment values later drift.
CREATE TABLE oxsum.merchant_environment_bootstrap (
    rail text PRIMARY KEY CHECK (rail IN ('hyperswitch','stripe','airwallex')),
    merchant_id uuid NOT NULL,
    version_id uuid NOT NULL,
    FOREIGN KEY (merchant_id, version_id)
        REFERENCES oxsum.merchant_versions(merchant_id, version_id)
);
CREATE TRIGGER merchant_environment_bootstrap_immutable BEFORE UPDATE OR DELETE
    ON oxsum.merchant_environment_bootstrap FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
