-- Existing operations may already have reached a switch that ignored the
-- idempotency header. Never assign them a fresh external identity on upgrade.
-- New writers explicitly opt in; the default stays false for older binaries.
ALTER TABLE oxsum.merchant_payment_bindings
    ADD COLUMN hyperswitch_body_id boolean NOT NULL DEFAULT false;
ALTER TABLE oxsum.refunds
    ADD COLUMN hyperswitch_body_id boolean NOT NULL DEFAULT false;

CREATE FUNCTION oxsum.protect_refund_request_identity() RETURNS trigger AS $guard$
BEGIN
    IF NEW.hyperswitch_body_id IS DISTINCT FROM OLD.hyperswitch_body_id THEN
        RAISE EXCEPTION 'refund provider request identity is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER refund_request_identity_immutable BEFORE UPDATE ON oxsum.refunds
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_refund_request_identity();

-- Retried unresolved events move behind untouched work in the bounded sweep.
CREATE INDEX merchant_events_recovery ON oxsum.merchant_event_inbox(updated_at,receipt_id)
    WHERE state IN ('pending','accepted');
