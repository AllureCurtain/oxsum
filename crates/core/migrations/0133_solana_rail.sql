-- The Solana deposit rail (issue #1117): a top-up paid in native SOL or the
-- configured SPL token to the deployment's deposit address, credited after
-- on-chain verification (the Solana Pay findReference + validateTransfer
-- recipe). `payment_ref` anchors on the per-intent `reference` account; the
-- transaction signature lands in `facilitator_tx_hash`. The `rail` CHECK
-- widens to admit 'solana' — the row, the walk and the idempotency anchor are
-- the same ones every rail shares.

ALTER TABLE oxsum.deposits
    DROP CONSTRAINT deposits_rail_check,
    ADD CONSTRAINT deposits_rail_check
        CHECK (rail IN ('manual', 'redemption', 'stripe', 'crypto-evm', 'x402', 'airwallex', 'hyperswitch', 'solana'));
