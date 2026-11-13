-- The Solana rail's inbound-webhook dedup (issue #1150): one row per provider
-- transaction signature the `POST /api/v1/webhooks/solana` receiver has already
-- scheduled a re-verify for. A Helius delivery may arrive twice — the provider
-- retries until it hears a 2xx — and a replayed push must never re-run the
-- findReference + validateTransfer walk for a signature already handled.
--
-- `signature` is the chain's own idempotency anchor: the same transaction
-- signature a `credit_solana_deposit` writes into `deposits.facilitator_tx_hash`
-- when the credit lands. Recording it here before the verify walks means the
-- first delivery claims the work and every replay acknowledges without a second
-- chain round-trip — the same shape `oxsum.payment_events` gives the card rails.
--
-- `matched` counts the pending intents the push's account keys resolved to; it
-- is observational only, so an operator reading the row sees whether the push
-- named a live intent or a transaction the rail had no stake in. Rows age out
-- under the retention job's deliveries window.

CREATE TABLE IF NOT EXISTS oxsum.solana_webhook_deliveries (
    signature text PRIMARY KEY,
    matched integer NOT NULL DEFAULT 0,
    received_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS solana_webhook_deliveries_received_at_idx
    ON oxsum.solana_webhook_deliveries (received_at);
