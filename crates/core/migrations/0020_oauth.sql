-- OAuth login (issue #152, roadmap P6-2): two tables. `oauth_states` carries the
-- CSRF state between the authorize redirect and the callback — minted per
-- attempt, ten-minute-lived, and consumed in the same statement that checks it
-- live, so a callback's state spends once ever. `oauth_accounts` links a
-- provider identity to a user: the provider's own user id is the key, and the
-- email the provider verified is kept only as provenance — the link is what
-- future logins resolve by, not the email, which the provider may change.
CREATE TABLE oxsum.oauth_states (
    state_hash  bytea       PRIMARY KEY,
    provider    text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz
);

CREATE TABLE oxsum.oauth_accounts (
    provider            text        NOT NULL,
    provider_user_id    text        NOT NULL,
    user_id             uuid        NOT NULL REFERENCES oxsum.users (user_id) ON DELETE CASCADE,
    email               text        NOT NULL,
    created_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, provider_user_id)
);

CREATE INDEX oauth_accounts_user ON oxsum.oauth_accounts (user_id);
