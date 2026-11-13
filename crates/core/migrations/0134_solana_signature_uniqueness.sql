-- Issue #1124: one on-chain signature may credit at most one deposit, ever.
-- The deposits idempotency anchor (rail, organization_id, payment_ref) is
-- per-organization and the ledger credit key solana:<signature> is scoped to
-- the tenant ledger, so a transaction carrying two references could pay one
-- transfer yet credit two organizations in full. A partial unique index on
-- (rail, facilitator_tx_hash) — only over rows that actually recorded a
-- signature — makes the second intent naming an already-used signature fail
-- the UPDATE, never double-credit. x402 rows are excluded by the rail filter
-- (its facilitator_tx_hash names an x402 settlement, not a Solana signature).

CREATE UNIQUE INDEX deposits_solana_tx_hash
    ON oxsum.deposits (rail, facilitator_tx_hash)
    WHERE rail = 'solana' AND facilitator_tx_hash IS NOT NULL;
