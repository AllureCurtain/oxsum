-- Invitation links: an owner or admin mints one, and the person holding it registers into
-- the organization. One row per link.
--
-- The link's token is `oxi-` plus 32 random bytes; the database keeps only its SHA-256 hash
-- (unique, so redemption is one index probe), for the same reason api_keys keeps only key
-- hashes: a dump of this table redeems nothing.
--
-- A link is valid seven days and usable once: `accepted_at` set means spent. There is no
-- revoke column in v1 — an unaccepted row is simply inert once `expires_at` passes.
--
-- Every statement here is schema-qualified, like the other migrations: the runner clears
-- `search_path`, so an unqualified name would fail loudly.

CREATE TABLE oxsum.invitations (
    invitation_id   uuid        PRIMARY KEY,
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    invited_by      uuid        NOT NULL REFERENCES oxsum.users (user_id) ON DELETE RESTRICT,
    -- SHA-256 of the link token, the only form the database ever holds.
    token_hash      bytea       NOT NULL UNIQUE,
    created_at      timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    accepted_at     timestamptz,
    accepted_by     uuid        REFERENCES oxsum.users (user_id) ON DELETE RESTRICT
);

-- One organization's invitations, for a future "pending links" list.
CREATE INDEX invitations_organization_id ON oxsum.invitations (organization_id);
