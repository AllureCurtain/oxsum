-- Issue #160 (roadmap P7-2): the admin audit log.
--
-- One append-only row per mutating `/api/v1/admin` call: what was done, to
-- what, with which fields (credentials redacted — a channel's apiKey never
-- enters `detail`), and the idempotency key the request carried, so an audit
-- can join the row to the ledger entry or replayed write it names.
--
-- The row is written after the mutation commits — the log may omit on a
-- crash, but it never describes a change that did not happen. The ledger
-- stays the proof for money; this table is the operator's index over every
-- admin write, including the ones no ledger entry describes.
CREATE TABLE oxsum.admin_audit (
    audit_id        uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    recorded_at     timestamptz NOT NULL DEFAULT now(),
    -- The mutating call, dotted: `channel.set`, `organization.adjust`,
    -- `tier.delete`, `discount.create`, `statement.finalize`, …
    action          text        NOT NULL,
    -- The single operator token is the only admin identity today; the column
    -- exists so a multi-identity admin surface never needs a rebuild.
    actor           text        NOT NULL DEFAULT 'operator',
    -- What the action acted on: a channel name, an organization id, a
    -- statement id — NULL when the call names nothing (a batch mint).
    target          text,
    -- The request fields safe to keep. Never a credential.
    detail          jsonb       NOT NULL DEFAULT '{}',
    -- The write's idempotency key, when it carried one.
    idempotency_key text
);

-- The read is newest-first pages; the id breaks the timestamp tie.
CREATE INDEX admin_audit_recorded ON oxsum.admin_audit (recorded_at DESC, audit_id DESC);
-- Narrowing to one action's rows is the audit's common question.
CREATE INDEX admin_audit_action ON oxsum.admin_audit (action, recorded_at DESC, audit_id DESC);
-- A keyed write's exact replay is the same logical event, so the same
-- (action, idempotency_key) audits once — the insert's ON CONFLICT absorbs it.
CREATE UNIQUE INDEX admin_audit_idempotent
    ON oxsum.admin_audit (action, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
