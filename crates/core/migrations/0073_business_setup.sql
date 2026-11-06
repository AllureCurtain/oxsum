-- Deployment-authorized, email-bound initial ownership. Only token hashes persist.
CREATE TABLE oxsum.business_setup_tokens (
    setup_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id uuid NOT NULL REFERENCES oxsum.businesses(business_id),
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 200),
    owner_email text NOT NULL,
    token_hash bytea NOT NULL UNIQUE,
    expires_at timestamptz NOT NULL DEFAULT now() + interval '30 minutes',
    accepted_at timestamptz,
    accepted_by uuid REFERENCES oxsum.users(user_id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (business_id, idempotency_key),
    CHECK ((accepted_at IS NULL) = (accepted_by IS NULL))
);
CREATE INDEX business_setup_pending ON oxsum.business_setup_tokens (business_id, expires_at)
    WHERE accepted_at IS NULL;

-- Setup authorizations and ownership consent are durable security evidence.
CREATE FUNCTION oxsum.protect_business_setup_audit() RETURNS trigger AS $audit$
BEGIN
    IF OLD.action IN ('business.setup.authorized', 'business.initialized') THEN
        RAISE EXCEPTION 'business setup audit is immutable';
    END IF;
    IF TG_OP = 'UPDATE' AND NEW.action IN ('business.setup.authorized', 'business.initialized') THEN
        RAISE EXCEPTION 'business setup audit is immutable';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$audit$ LANGUAGE plpgsql;
CREATE TRIGGER admin_audit_protect_setup BEFORE UPDATE OR DELETE ON oxsum.admin_audit
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_business_setup_audit();
