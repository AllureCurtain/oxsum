-- Credit notes (issue #352, decision D12): a reasoned credit the operator
-- issues that moves no money. One row per issuance, naming the ledger entry
-- that carries it — the entry is the fact (Revenue debited, Wallet credited,
-- the reason as its description), this table is the lookup the admin surface
-- lists. A retried idempotency key replays the same entry, so the unique pair
-- is what makes a note double-post impossible.
--
-- A credit note is not a refund: no PSP is called, no deposit is reversed, no
-- cash moves. The refund and chargeback halves of D12 remain separate issues.

CREATE TABLE IF NOT EXISTS oxsum.credit_notes (
    credit_note_id  uuid        PRIMARY KEY,
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE CASCADE,
    entry_id        uuid        NOT NULL,
    amount_minor    bigint      NOT NULL CHECK (amount_minor > 0),
    reason          text        NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, entry_id)
);

-- The admin list reads one organization's notes, newest first.
CREATE INDEX IF NOT EXISTS credit_notes_org_idx
    ON oxsum.credit_notes (organization_id, created_at DESC);
