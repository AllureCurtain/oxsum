-- The Hyperswitch deposit rail (issue #319): the switch in front of the
-- processors. `POST /api/v1/org/topups/hyperswitch` opens a `pending` deposit
-- keyed on the caller's `idempotencyKey`, asks the switch for a payment and
-- answers the payment id, client secret and publishable key the browser SDK
-- needs; the HMAC-verified webhook then walks `pending → confirmed → credited`
-- under the ledger key `hyperswitch:<payment_id>`. The `rail` CHECK widens to
-- admit 'hyperswitch' — the row, the walk and the idempotency anchor are the
-- same ones every rail shares.

ALTER TABLE oxsum.deposits
    DROP CONSTRAINT deposits_rail_check,
    ADD CONSTRAINT deposits_rail_check
        CHECK (rail IN ('manual', 'redemption', 'stripe', 'crypto-evm', 'x402', 'airwallex', 'hyperswitch'));
