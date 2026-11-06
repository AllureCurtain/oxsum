-- Promotional lots and prepaid bundles (issue #609).
--
-- A promo lot is a grant of credit with a provenance and an optional expiry.
-- The spendable money itself lives in the ledger's `Equity:Promo` pool — a lot
-- row is the projection that says which grant still funds how much of it, so a
-- hold can pin the exact lots that back it and an expiry can name the amount
-- it burned. `remaining_minor` counts only the *unreserved* part: a hold's
-- admission decrements it inside the same transaction that posts the ledger's
-- pending debit, and `promo_draws` records which lots that hold pinned.
--
-- A lot is written once at its source event — a grant, a bundle fulfillment or
-- the legacy-bonus migration — and never edited afterwards: `amount_minor`,
-- `source`, `source_ref`, `expires_at` and `created_at` are immutable. Only
-- `remaining_minor` moves (draws down, releases back, expiry/cancel to zero)
-- and `closed_at`/`close_reason` land once, together.

CREATE TABLE IF NOT EXISTS oxsum.promo_lots (
    lot_id          uuid        PRIMARY KEY,
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE CASCADE,
    -- Where the lot came from: an operator/admin grant, a bundle fulfillment's
    -- promo leg, or the one-time migration of the aggregate bonus pool.
    source          text        NOT NULL CHECK (source IN ('grant', 'admin', 'bundle_promo', 'legacy_migration')),
    -- The source event's own identity — the idempotency anchor: an order's
    -- fulfillment id for `bundle_promo`, the grant key for `admin`/`grant`,
    -- the tenant id for `legacy_migration`. A replayed source event finds its
    -- standing lot through the unique pair and grants nothing twice.
    source_ref      text        NOT NULL CHECK (length(source_ref) BETWEEN 1 AND 200),
    -- The lot's currency is the ledger's own credit unit; the column rides for
    -- symmetry with the order tables and so a billed-out figure can never be
    -- misread as a display amount.
    currency        text        NOT NULL DEFAULT 'USD' CHECK (currency ~ '^[A-Z]{3}$'),
    -- What the grant posted, immutable: `debit Adjustments / credit Promo`.
    amount_minor    bigint      NOT NULL CHECK (amount_minor > 0),
    -- The unreserved part only. Draws decrement it inside the hold's
    -- transaction; a released share returns to it when the lot is still open,
    -- or burns to `Equity:PromoExpiry` when the lot already expired or was
    -- cancelled — never back to a refundable pool.
    remaining_minor bigint      NOT NULL CHECK (remaining_minor >= 0),
    -- NULL means the lot never expires.
    expires_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- Terminal close that was not an expiry: a refund's cancellation, or the
    -- expiry pass's own mark. `close_reason` is 'expired' or 'cancelled';
    -- an exhausted lot (drawn to zero) carries none — its close is arithmetic.
    closed_at       timestamptz,
    close_reason    text        CHECK (close_reason IN ('expired', 'cancelled')),
    UNIQUE (organization_id, source, source_ref)
);
-- The expiry pass's work list and the admission allocation both walk open
-- lots in expiry order: soonest-expiring money burns first, never-expiring last.
CREATE INDEX IF NOT EXISTS promo_lots_draw_order
    ON oxsum.promo_lots (organization_id, expires_at ASC NULLS LAST, lot_id)
    WHERE closed_at IS NULL AND remaining_minor > 0;

-- Lot terms are immutable once written; only the lifecycle columns move.
CREATE OR REPLACE FUNCTION oxsum.protect_promo_lot() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'promo lot history is retained'; END IF;
    IF NEW.lot_id IS DISTINCT FROM OLD.lot_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.source IS DISTINCT FROM OLD.source
        OR NEW.source_ref IS DISTINCT FROM OLD.source_ref
        OR NEW.amount_minor IS DISTINCT FROM OLD.amount_minor
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.expires_at IS DISTINCT FROM OLD.expires_at
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        -- remaining_minor only ever moves down, or back up by exactly a draw's
        -- own release — the trigger cannot see the draw, so it holds the two
        -- hard invariants: it never exceeds the grant and never goes negative.
        OR NEW.remaining_minor < 0 OR NEW.remaining_minor > OLD.amount_minor
        -- A close cannot be reopened or rewritten.
        OR (OLD.closed_at IS NOT NULL AND
            (NEW.closed_at IS DISTINCT FROM OLD.closed_at
             OR NEW.close_reason IS DISTINCT FROM OLD.close_reason)) THEN
        RAISE EXCEPTION 'promo lot terms and terminal state are immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER promo_lots_protected BEFORE UPDATE OR DELETE
    ON oxsum.promo_lots FOR EACH ROW EXECUTE FUNCTION oxsum.protect_promo_lot();

-- Which lots a hold pinned at admission. The ledger entry's pending debit on
-- `Equity:Promo` covers `SUM(amount_minor)` across the hold's rows; settlement
-- walks them in the same deterministic order the allocation took, so the
-- debits the charge writes land on the lots the hold named — never re-scanned.
-- `charged_minor` + `released_minor` are filled in once, together, inside the
-- settlement transaction; their sum then equals `amount_minor` exactly.
CREATE TABLE IF NOT EXISTS oxsum.promo_draws (
    hold_key        text        NOT NULL,
    -- The ledger schema the hold lives in — the wallet facade routes on it.
    tenant_id       text        NOT NULL,
    lot_id          uuid        NOT NULL REFERENCES oxsum.promo_lots (lot_id),
    -- The share of the hold's freeze this lot covers, pinned at admission.
    amount_minor    bigint      NOT NULL CHECK (amount_minor > 0),
    -- Settled once: what of the reservation the charge consumed, and what
    -- returned to the lot (or burned to `Equity:PromoExpiry` for a closed lot).
    charged_minor   bigint      CHECK (charged_minor >= 0),
    released_minor  bigint      CHECK (released_minor >= 0),
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (hold_key, lot_id)
);
-- Settlement looks the hold's draws up by key; expiry/cancel walks open draws
-- of a lot to know what is still reserved.
CREATE INDEX IF NOT EXISTS promo_draws_lot_open
    ON oxsum.promo_draws (lot_id)
    WHERE charged_minor IS NULL;

CREATE OR REPLACE FUNCTION oxsum.protect_promo_draw() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'promo draw history is retained'; END IF;
    IF NEW.hold_key IS DISTINCT FROM OLD.hold_key
        OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
        OR NEW.lot_id IS DISTINCT FROM OLD.lot_id
        OR NEW.amount_minor IS DISTINCT FROM OLD.amount_minor
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        -- The settle pair lands once, together, and must then sum to the
        -- reservation exactly: before it both columns are NULL, after it they
        -- are the two parts of amount_minor and can never move again.
        OR (OLD.charged_minor IS NOT NULL)
        OR (OLD.released_minor IS NOT NULL)
        OR (NEW.charged_minor IS NULL) <> (NEW.released_minor IS NULL)
        OR (NEW.charged_minor IS NOT NULL
            AND NEW.charged_minor + NEW.released_minor <> OLD.amount_minor) THEN
        RAISE EXCEPTION 'promo draw terms are immutable once settled';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER promo_draws_protected BEFORE UPDATE OR DELETE
    ON oxsum.promo_draws FOR EACH ROW EXECUTE FUNCTION oxsum.protect_promo_draw();
