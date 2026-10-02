-- oxsum's own tables: users, organizations, memberships and API keys.
--
-- They live in the `oxsum` schema, outside the ledger schemas (one schema per organization,
-- managed by doubleentry's migrate). Every statement here and every runtime query names the
-- table schema-qualified: the ledger pins `search_path` per transaction because its 55
-- vendored query sites cannot be rewritten, oxsum's own handful of sites can simply say where
-- they mean. The migration runner clears `search_path` for exactly this reason, so an
-- unqualified name here fails loudly instead of landing in `public`.

CREATE TABLE oxsum.users (
    user_id          uuid        PRIMARY KEY,
    email            text        NOT NULL,
    email_normalized text        NOT NULL UNIQUE,
    password_hash    text        NOT NULL,
    platform_admin   boolean     NOT NULL DEFAULT false,
    created_at       timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE oxsum.organizations (
    organization_id uuid        PRIMARY KEY,
    name            text        NOT NULL,
    -- The ledger's tenant id, so the ledger schema is ledger_<tenant_id>.
    tenant_id       text        NOT NULL UNIQUE,
    -- product.md: a personal organization flips to team when its first member joins.
    -- Carried from the start, so that flip never needs a migration.
    kind            text        NOT NULL CHECK (kind IN ('personal', 'team')),
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE oxsum.memberships (
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    user_id         uuid        NOT NULL REFERENCES oxsum.users (user_id) ON DELETE RESTRICT,
    role            text        NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, user_id)
);

CREATE TABLE oxsum.api_keys (
    key_id          uuid        PRIMARY KEY,
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    -- Optional, for the user's own recognition of a key.
    name            text,
    -- Leading characters of the secret, shown in lists. Never a secret on its own.
    prefix          text        NOT NULL,
    -- SHA-256 of the secret, the only form the database ever holds. Also the lookup:
    -- the unique index on this column is what authenticates a request.
    secret_hash     bytea       NOT NULL UNIQUE,
    -- Who minted it, for product.md's "members manage their own, admins manage all".
    -- Nullable: a key can outlive the membership that created it; and it is set to null
    -- rather than cascading a delete into credentials.
    created_by      uuid        REFERENCES oxsum.users (user_id) ON DELETE SET NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz,
    revoked_at      timestamptz
);

-- Listing one organization's keys, the only other way api_keys is read.
CREATE INDEX api_keys_organization_id ON oxsum.api_keys (organization_id);
