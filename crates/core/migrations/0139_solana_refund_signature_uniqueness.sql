-- Issue #1209: one on-chain payout signature closes at most one refund, ever.
-- `solana_complete_refund` already serializes the claim inside the row lock —
-- the EXISTS probe on `meta->>'refundSignature'` runs `FOR UPDATE` on the
-- completing row — but two refunds racing on *different* deposit rows hold
-- different locks, so both probes can read "unused" before either UPDATE
-- lands. A partial unique index on the recorded signature makes the second
-- UPDATE fail 23505 even when the probe raced; the completing call maps it to
-- the same Conflict refusal the probe raises. `solana` rows only, and only
-- rows that actually recorded a signature — pending refunds carry no key.

CREATE UNIQUE INDEX deposits_solana_refund_signature
    ON oxsum.deposits ((meta->>'refundSignature'))
    WHERE rail = 'solana' AND meta ? 'refundSignature';
