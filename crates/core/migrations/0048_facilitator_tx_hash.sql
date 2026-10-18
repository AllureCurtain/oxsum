-- The facilitator-delegated x402 rail (issue #282): a settled payment's
-- on-chain transaction hash lands on the deposit row for ops traceability —
-- the "what did the chain say" column beside `payment_ref`'s idempotency
-- anchor. NULL on every non-facilitator rail and on the Ed25519 stand-in,
-- which settles nothing on any chain.

ALTER TABLE oxsum.deposits
    ADD COLUMN facilitator_tx_hash text;
