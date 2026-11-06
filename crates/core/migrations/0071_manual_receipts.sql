-- T01.a: durable, operator-authorized money-in intent. Historical manual deposits
-- remain unchanged; new references cannot credit two customers or two keys.
CREATE TABLE oxsum.manual_receipts (
    receipt_id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    reference text NOT NULL UNIQUE CHECK (length(reference) BETWEEN 1 AND 200),
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 1000),
    amount_minor bigint NOT NULL CHECK (amount_minor > 0),
    authorized_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, idempotency_key)
);

CREATE FUNCTION oxsum.manual_receipts_are_immutable() RETURNS trigger AS $immutable$
BEGIN
    RAISE EXCEPTION 'manual receipt terms are immutable';
END;
$immutable$ LANGUAGE plpgsql;
CREATE TRIGGER manual_receipts_immutable
    BEFORE UPDATE OR DELETE ON oxsum.manual_receipts
    FOR EACH ROW EXECUTE FUNCTION oxsum.manual_receipts_are_immutable();
