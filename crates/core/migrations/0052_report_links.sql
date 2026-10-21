-- Share links for end-user usage reports (issue #300): the downstream customer
-- holds no account, so the link is the capability. The token is 32 random bytes
-- behind an `oxb-` mark, and the table keeps only its SHA-256 — a dump of
-- `oxsum.report_links` opens nothing.
--
-- `scope` names what the link resolves to: `report` pins one minted usage
-- report (the customer's bill for one span), `end_user` names one downstream
-- customer and lists the reports the organization has issued them. The
-- plaintext token is answered once at creation; `revoked_at` and `expires_at`
-- are the two ways a link stops working, and `last_used_at` is the operator's
-- signal that a customer actually opened the bill.

CREATE TABLE oxsum.report_links (
    link_id         uuid        PRIMARY KEY,
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id),
    scope           text        NOT NULL CHECK (scope IN ('report', 'end_user')),
    report_id       uuid        REFERENCES oxsum.usage_reports (report_id) ON DELETE CASCADE,
    end_user        text        NOT NULL,
    label           text,
    token_hash      bytea       NOT NULL UNIQUE,
    created_by      uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    revoked_at      timestamptz,
    last_used_at    timestamptz,
    CONSTRAINT report_links_scope_target CHECK (
        (scope = 'report' AND report_id IS NOT NULL)
        OR (scope = 'end_user' AND report_id IS NULL)
    )
);

COMMENT ON TABLE oxsum.report_links IS
    'capability links a downstream customer opens without an account — the token is stored as SHA-256 only, expires, and can be revoked (issue #300)';
COMMENT ON COLUMN oxsum.report_links.scope IS
    'report = one minted usage report; end_user = the reports issued to one downstream customer';
COMMENT ON COLUMN oxsum.report_links.token_hash IS
    'SHA-256 of the `oxb-` token; the plaintext exists only in the create response';
COMMENT ON COLUMN oxsum.report_links.last_used_at IS
    'when the link last resolved — the operator''s signal that the customer opened the bill';

CREATE INDEX report_links_organization ON oxsum.report_links (organization_id, created_at DESC);
