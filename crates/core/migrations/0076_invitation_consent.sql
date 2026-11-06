-- Existing team links keep their identities and expiry. New business invitations
-- reserve a future customer identity; no ownerless billing account is created.
ALTER TABLE oxsum.invitations
    ALTER COLUMN organization_id DROP NOT NULL,
    ADD COLUMN business_id uuid DEFAULT oxsum.default_business_id() REFERENCES oxsum.businesses(business_id),
    ADD COLUMN kind text NOT NULL DEFAULT 'team' CHECK (kind IN ('team','customer')),
    ADD COLUMN intended_email text,
    ADD COLUMN revoked_at timestamptz,
    ADD COLUMN pending_customer_id uuid UNIQUE,
    ADD COLUMN customer_name text,
    ADD COLUMN external_id text,
    ADD COLUMN creation_mutation_id uuid UNIQUE REFERENCES oxsum.business_mutations(mutation_id);
UPDATE oxsum.invitations i SET business_id=c.business_id
    FROM oxsum.customer_profiles c WHERE c.organization_id=i.organization_id;
ALTER TABLE oxsum.invitations ALTER COLUMN business_id SET NOT NULL;
ALTER TABLE oxsum.invitations ADD CONSTRAINT invitation_target CHECK (
    (kind='team' AND organization_id IS NOT NULL AND pending_customer_id IS NULL
        AND customer_name IS NULL AND external_id IS NULL)
    OR (kind='customer' AND pending_customer_id IS NOT NULL AND intended_email IS NOT NULL
        AND customer_name IS NOT NULL AND external_id IS NOT NULL
        AND (organization_id IS NULL OR organization_id=pending_customer_id))
);
CREATE INDEX invitations_business_external ON oxsum.invitations(business_id,external_id)
    WHERE kind='customer' AND accepted_at IS NULL AND revoked_at IS NULL;

-- Select the creation-time identity from the invitation in the same transaction.
-- Other signup/team/seed paths retain the original UUID-based external ID.
CREATE OR REPLACE FUNCTION oxsum.assign_customer_business() RETURNS trigger AS $assign$
BEGIN
    INSERT INTO oxsum.customer_profiles (organization_id, business_id, external_id, contact_email)
        SELECT NEW.organization_id, COALESCE(i.business_id,oxsum.default_business_id()),
            COALESCE(i.external_id,NEW.organization_id::text), i.intended_email
        FROM (SELECT 1) AS one LEFT JOIN oxsum.invitations i
            ON i.pending_customer_id=NEW.organization_id AND i.kind='customer';
    RETURN NEW;
END;
$assign$ LANGUAGE plpgsql;

-- Receipt and audit history are retained even when a membership is later removed.
CREATE OR REPLACE FUNCTION oxsum.protect_business_setup_audit() RETURNS trigger AS $audit$
BEGIN
    IF OLD.action LIKE 'business.%' OR OLD.action LIKE 'invitation.%' OR OLD.action = 'session.context' THEN
        RAISE EXCEPTION 'business and invitation security audit is immutable';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.action LIKE 'business.%' OR NEW.action LIKE 'invitation.%' OR NEW.action = 'session.context') THEN
        RAISE EXCEPTION 'business and invitation security audit is immutable';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$audit$ LANGUAGE plpgsql;
