-- The Airwallex deposit rail (issue #285): a second fiat acquirer beside
-- Stripe. `POST /api/v1/topups/airwallex` opens a `pending` deposit keyed on
-- the caller's `idempotencyKey`, asks Airwallex for a PaymentIntent and
-- answers the hosted-payment-page URL the payer's browser follows; the
-- signature-verified webhook then walks `pending → confirmed → credited`
-- under the ledger key `airwallex:<intent_id>`. The `rail` CHECK widens to
-- admit 'airwallex' — the row, the walk and the idempotency anchor are the
-- same ones every rail shares.

ALTER TABLE oxsum.deposits
    DROP CONSTRAINT deposits_rail_check,
    ADD CONSTRAINT deposits_rail_check
        CHECK (rail IN ('manual', 'redemption', 'stripe', 'crypto-evm', 'x402', 'airwallex'));
