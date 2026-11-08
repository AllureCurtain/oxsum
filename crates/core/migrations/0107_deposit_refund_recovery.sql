-- A local reversal and an external refund are independent facts. Keep the
-- original deposit metadata while giving every new external attempt one ID.
CREATE TABLE oxsum.deposit_refunds (
    deposit_id uuid PRIMARY KEY REFERENCES oxsum.deposits(deposit_id),
    request_id uuid NOT NULL UNIQUE,
    organization_id uuid NOT NULL REFERENCES oxsum.organizations(organization_id),
    merchant_id uuid,
    version_id uuid,
    provider_payment_id text,
    amount_minor bigint NOT NULL CHECK (amount_minor >= 0),
    currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    body_identity boolean NOT NULL DEFAULT false,
    provider_refund_id text,
    reason text NOT NULL,
    state text NOT NULL DEFAULT 'unknown' CHECK (state IN ('unknown','pending','succeeded','failed')),
    attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 5),
    next_attempt_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((merchant_id IS NULL) = (version_id IS NULL)),
    FOREIGN KEY (merchant_id,version_id) REFERENCES oxsum.merchant_versions(merchant_id,version_id)
);
CREATE INDEX deposit_refunds_due ON oxsum.deposit_refunds(next_attempt_at)
    WHERE state IN ('unknown','pending');

-- An old HTTP success was not authenticated terminal evidence. Preserve it in
-- deposit.meta and import a conservative recovery record. No historical row
-- gains a new external create identity; only known, pinned handles can be read.
INSERT INTO oxsum.deposit_refunds
    (deposit_id,request_id,organization_id,merchant_id,version_id,provider_payment_id,
     amount_minor,currency,provider_refund_id,reason,next_attempt_at)
SELECT d.deposit_id,d.deposit_id,d.organization_id,b.merchant_id,b.version_id,
       r.provider_payment_id,COALESCE(d.received_minor,d.amount_minor),COALESCE(b.currency,'USD'),
       NULLIF(d.meta->>'hyperswitchRefundId',''),'Historical reversal: reconcile retained evidence',
       CASE WHEN b.payment_id IS NOT NULL AND r.provider_payment_id IS NOT NULL
                 AND NULLIF(d.meta->>'hyperswitchRefundId','') IS NOT NULL
                 AND b.amount_minor=COALESCE(d.received_minor,d.amount_minor) AND b.amount_minor>0
                 AND b.organization_id=d.organization_id THEN now() ELSE NULL END
FROM oxsum.deposits d
LEFT JOIN oxsum.merchant_payment_bindings b ON b.payment_id=d.deposit_id AND b.purpose='legacyDeposit'
LEFT JOIN oxsum.merchant_payment_refs r ON r.payment_id=d.deposit_id
WHERE d.rail='hyperswitch' AND d.status='reversed';

CREATE FUNCTION oxsum.protect_deposit_refund() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'deposit refund history is immutable'; END IF;
    IF ROW(NEW.deposit_id,NEW.request_id,NEW.organization_id,NEW.merchant_id,NEW.version_id,
           NEW.provider_payment_id,NEW.amount_minor,NEW.currency,NEW.body_identity,NEW.reason,NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.deposit_id,OLD.request_id,OLD.organization_id,OLD.merchant_id,OLD.version_id,
           OLD.provider_payment_id,OLD.amount_minor,OLD.currency,OLD.body_identity,OLD.reason,OLD.created_at)
       OR (OLD.provider_refund_id IS NOT NULL AND NEW.provider_refund_id IS DISTINCT FROM OLD.provider_refund_id)
       OR NEW.attempt_count < OLD.attempt_count
       OR (OLD.state='succeeded' AND NEW.state<>'succeeded')
       OR (OLD.state='failed' AND NEW.state NOT IN ('failed','succeeded')) THEN
        RAISE EXCEPTION 'deposit refund identity and terminal result are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER deposit_refunds_protected BEFORE UPDATE OR DELETE ON oxsum.deposit_refunds
    FOR EACH ROW EXECUTE FUNCTION oxsum.protect_deposit_refund();
