-- Deposits: the single record of money arriving (decision D2, issue #118). Every rail —
-- the manual top-up, a redemption code, later Stripe or a chain watcher — writes one row
-- here, so reconciliation reads one table instead of one per rail.
--
-- `rail` + `organization_id` + `payment_ref` is the idempotency anchor: the code id for
-- redemptions, the caller's idempotency key for manual top-ups, a charge id or
-- transaction hash for the rails to come. Keys are only ever scoped inside one
-- organization, so two organizations answering the same top-up key stay distinct. `amount_minor` is what the deposit should credit, `received_minor` what
-- the rail reports as paid — the same for today's rails; the gap is where an under- or
-- over-payment sits for a human, never auto-patched.
--
-- Status walks pending → confirmed → credited; reversed and expired are the ways out.
-- `entry_id` names the ledger entry that credited it, nullable until status = credited:
-- a deposit stuck below credited is a known-good state a retry or the reconciler resumes,
-- not a failure to hide.

CREATE TABLE oxsum.deposits (
    deposit_id      uuid        PRIMARY KEY,
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    rail            text        NOT NULL CHECK (rail IN ('manual', 'redemption', 'stripe', 'crypto-evm')),
    -- Unique inside (rail, organization) — the code id, the caller's idempotency key,
    -- a charge id: every reference is only ever scoped to its owner.
    payment_ref     text        NOT NULL,
    amount_minor    bigint      NOT NULL CHECK (amount_minor > 0),
    received_minor  bigint      CHECK (received_minor IS NULL OR received_minor >= 0),
    status          text        NOT NULL CHECK (status IN ('pending', 'confirmed', 'credited', 'reversed', 'expired')),
    -- The ledger entry that credited the wallet, filled when status reaches credited.
    entry_id        uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    -- Charge-shaped rails expire; a code or a manual top-up does not have to.
    expires_at      timestamptz,
    -- Rail-specific facts: a code's batch, an exchange-rate snapshot, a sender address.
    meta            jsonb       NOT NULL DEFAULT '{}'::jsonb,
    UNIQUE (rail, organization_id, payment_ref)
);

-- An organization's funding history, newest reads page by created_at.
CREATE INDEX deposits_organization_id ON oxsum.deposits (organization_id, created_at);

-- Redemption codes (the redemption rail's spendable side): the operator mints a batch,
-- the holder redeems once. Like invitation tokens and API keys, the table keeps only the
-- SHA-256 of the code — a dump redeems nothing — and the mint answer is the only place
-- the plaintext exists.

CREATE TABLE oxsum.redemption_codes (
    code_id      uuid        PRIMARY KEY,
    -- SHA-256 of the code, the only form the database ever holds.
    code_hash    bytea       NOT NULL UNIQUE,
    -- The mint batch, so one "generate 500" call is traceable as one group.
    batch_id     uuid        NOT NULL,
    amount_minor bigint      NOT NULL CHECK (amount_minor > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz,
    redeemed_at  timestamptz,
    redeemed_by  uuid        REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    -- The deposit this code's redemption opened; one code opens at most one.
    deposit_id   uuid        UNIQUE REFERENCES oxsum.deposits (deposit_id) ON DELETE RESTRICT
);

-- Listing one mint batch's codes and their state.
CREATE INDEX redemption_codes_batch_id ON oxsum.redemption_codes (batch_id);
