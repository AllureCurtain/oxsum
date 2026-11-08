-- A zero-priced turn still owns a durable concurrency reservation. Null keeps
-- old watch rows distinct: those rows alone never proved successful admission.
ALTER TABLE oxsum.open_holds DROP CONSTRAINT open_holds_freeze_minor_check;
ALTER TABLE oxsum.open_holds ADD CONSTRAINT open_holds_freeze_minor_check
    CHECK (freeze_minor >= 0);
ALTER TABLE oxsum.open_holds ADD COLUMN wallet_minor bigint
    CHECK (wallet_minor IS NULL OR (wallet_minor >= 0 AND wallet_minor <= freeze_minor));
CREATE INDEX open_holds_zero_wallet_key ON oxsum.open_holds(tenant_id,key_id)
    WHERE wallet_minor=0;
