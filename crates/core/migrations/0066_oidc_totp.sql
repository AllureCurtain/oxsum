-- OIDC login and TOTP second factor (issue #504): three additions.
--
-- `oxsum.oauth_states` gains the PKCE fields an OIDC `code+PKCE` flow needs.
-- The GitHub flow predates PKCE and leaves both NULL; an OIDC authorize mints
-- `pkce_verifier` (the server-held half of the S256 pair — only the challenge
-- ever leaves the process, so the verifier is what the token exchange must
-- prove it still holds) and `nonce` (echoed back in the `id_token`, checked
-- against the row, so a token minted for a different login cannot be replayed
-- into this callback). Both ride the state row because both are single-use
-- exactly when the state is.
ALTER TABLE oxsum.oauth_states
    ADD COLUMN IF NOT EXISTS pkce_verifier text,
    ADD COLUMN IF NOT EXISTS nonce        text;

-- A user's second factor: the TOTP shared secret, sealed under the deployment's
-- OXSUM_SECRET_KEY the way `channel_credentials.api_key_sealed` seals an
-- upstream key — a dump of this table authenticates no code. `confirmed_at`
-- distinguishes "enrolled but never proved" from "in force": only a confirmed
-- secret gates the login. `backup_code_hashes` are the single-use recovery
-- codes' SHA-256s — stored hashed like a session token, removed on use.
CREATE TABLE IF NOT EXISTS oxsum.user_totp (
    user_id            uuid        PRIMARY KEY REFERENCES oxsum.users (user_id) ON DELETE CASCADE,
    secret_sealed      text        NOT NULL,
    confirmed_at       timestamptz,
    backup_code_hashes bytea[]     NOT NULL DEFAULT '{}',
    created_at         timestamptz NOT NULL DEFAULT now()
);

-- The login's second leg's credential: after a password verifies for a
-- TOTP-enabled user, the session is withheld behind this challenge rather than
-- minted — the row carries who proved the password, is single-use and
-- short-lived, and counts its wrong-code attempts so a probe cannot brute the
-- six digits inside the challenge's life.
CREATE TABLE IF NOT EXISTS oxsum.totp_challenges (
    challenge_hash bytea        PRIMARY KEY,
    user_id        uuid        NOT NULL REFERENCES oxsum.users (user_id) ON DELETE CASCADE,
    attempts       integer     NOT NULL DEFAULT 0,
    expires_at     timestamptz NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- When the login's second factor was satisfied on this session — NULL for a
-- password-or-OAuth-only session, set for one minted through the TOTP leg.
-- v1 records it; a route that needs a recent second factor re-checks the
-- timestamp against its own freshness bound (the gate itself is a later issue).
ALTER TABLE oxsum.sessions
    ADD COLUMN IF NOT EXISTS totp_verified_at timestamptz;
