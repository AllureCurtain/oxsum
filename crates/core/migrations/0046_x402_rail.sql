-- The x402 deposit rail (issue #264): a caller presents a signed payment
-- authorization and the rail credits the deposit through the same
-- pending → confirmed → credited walk every rail shares. The `rail` CHECK
-- widens to admit 'x402' — the name is self-documenting beside 'manual',
-- 'redemption' and 'stripe', and reuses the 'crypto-evm' slot's intent while
-- naming the authorization shape rather than a chain. The seam is the
-- verifier: the signed nonce lands in `payment_ref` as the idempotency anchor,
-- and the payer identity lands in `meta`.

ALTER TABLE oxsum.deposits
    DROP CONSTRAINT deposits_rail_check,
    ADD CONSTRAINT deposits_rail_check
        CHECK (rail IN ('manual', 'redemption', 'stripe', 'crypto-evm', 'x402'));
