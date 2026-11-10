-- One source-reference namespace for manual top-ups and staff monthly payments.
-- AFTER INSERT triggers protect older writers too, without changing old receipt
-- keys or interfering with exact ON CONFLICT DO NOTHING retries.
CREATE TABLE oxsum.payment_receipt_references (
    reference text PRIMARY KEY,
    source_kind text NOT NULL CHECK (source_kind IN ('manual','manual_order','monthly','historical_conflict')),
    source_id uuid UNIQUE,
    CHECK ((source_kind = 'historical_conflict') = (source_id IS NULL))
);
-- Freeze source inserts until backfill and old-writer claim triggers are installed.
LOCK TABLE oxsum.manual_receipts, oxsum.payment_orders IN SHARE ROW EXCLUSIVE MODE;
-- Old paths had separate uniqueness domains. Preserve conflicting history and
-- reserve its reference without choosing a winning source or moving old cash.
INSERT INTO oxsum.payment_receipt_references(reference,source_kind,source_id)
    SELECT reference, CASE WHEN count(*)=1 THEN min(kind) ELSE 'historical_conflict' END,
        CASE WHEN count(*)=1 THEN min(id::text)::uuid ELSE NULL END
    FROM (
        SELECT reference,'manual' AS kind,receipt_id AS id FROM oxsum.manual_receipts
        UNION ALL
        SELECT evidence->>'reference','manual_order',order_id FROM oxsum.payment_orders
            WHERE source='manual' AND evidence->>'reference' IS NOT NULL
    ) sources GROUP BY reference;
CREATE TABLE oxsum.monthly_repayments (
    mutation_id uuid PRIMARY KEY REFERENCES oxsum.business_mutations(mutation_id),
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    statement_id uuid NOT NULL REFERENCES oxsum.statements(statement_id),
    amount_minor bigint NOT NULL CHECK (amount_minor > 0 AND amount_minor <= 9007199254740991),
    reference text NOT NULL CHECK (length(reference) BETWEEN 1 AND 200),
    entry_id uuid NOT NULL,
    content_hash text NOT NULL CHECK (content_hash ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE FUNCTION oxsum.claim_payment_receipt_reference() RETURNS trigger AS $$
BEGIN
    IF TG_TABLE_NAME = 'manual_receipts' THEN
        INSERT INTO oxsum.payment_receipt_references VALUES(NEW.reference,'manual',NEW.receipt_id);
    ELSIF TG_TABLE_NAME = 'payment_orders' THEN
        IF NEW.source='manual' AND NEW.evidence->>'reference' IS NOT NULL THEN
            INSERT INTO oxsum.payment_receipt_references VALUES(NEW.evidence->>'reference','manual_order',NEW.order_id);
        END IF;
    ELSE
        INSERT INTO oxsum.payment_receipt_references VALUES(NEW.reference,'monthly',NEW.mutation_id);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER manual_receipt_reference AFTER INSERT ON oxsum.manual_receipts
    FOR EACH ROW EXECUTE FUNCTION oxsum.claim_payment_receipt_reference();
CREATE TRIGGER manual_order_receipt_reference AFTER INSERT ON oxsum.payment_orders
    FOR EACH ROW EXECUTE FUNCTION oxsum.claim_payment_receipt_reference();
CREATE TRIGGER monthly_receipt_reference AFTER INSERT ON oxsum.monthly_repayments
    FOR EACH ROW EXECUTE FUNCTION oxsum.claim_payment_receipt_reference();
CREATE TRIGGER payment_receipt_references_immutable BEFORE UPDATE OR DELETE ON oxsum.payment_receipt_references
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
CREATE TRIGGER monthly_repayments_immutable BEFORE UPDATE OR DELETE ON oxsum.monthly_repayments
    FOR EACH ROW EXECUTE FUNCTION oxsum.business_migrations_are_immutable();
