-- Sessions for web login: one row per login, authenticating its bearer by token hash.
--
-- The cookie value is `oxsess-` plus 32 random bytes; the database keeps only its SHA-256
-- hash (unique, so the lookup is one index probe), for the same reason api_keys keeps only
-- key hashes: a dump of this table authenticates nothing.
--
-- A session names a (user, organization) pair, and resolution joins memberships on it, so a
-- session stops authenticating the moment its membership is gone: the row cannot outlive the
-- authorization it names. Every statement here is schema-qualified, like the other
-- migrations: the runner clears `search_path`, so an unqualified name would fail loudly.

CREATE TABLE oxsum.sessions (
    session_id      uuid        PRIMARY KEY,
    user_id         uuid        NOT NULL REFERENCES oxsum.users (user_id) ON DELETE RESTRICT,
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    -- SHA-256 of the cookie value, the only form the database ever holds. Also the lookup:
    -- the unique index on this column is what authenticates a request.
    token_hash      bytea       NOT NULL UNIQUE,
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- Absolute expiry: thirty days after login, no sliding renewal in v1.
    expires_at      timestamptz NOT NULL,
    -- Last authenticated request; touched at most once per five minutes, so the column stays
    -- meaningful without a write on every request.
    last_used_at    timestamptz NOT NULL DEFAULT now(),
    revoked_at      timestamptz
);

-- One user's sessions, for a future "active sessions" list; the only other way this table
-- is read.
CREATE INDEX sessions_user_id ON oxsum.sessions (user_id);
