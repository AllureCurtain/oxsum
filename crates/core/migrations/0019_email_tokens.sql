-- Email verification and password reset (issue #150, roadmap P6-1): one token
-- table serves both mails. The pattern is the invitation's — the token the mail
-- carries is `oxt-` plus 32 random bytes; the table keeps only its SHA-256, so
-- a dump of `oxsum.email_tokens` verifies nothing and resets nothing.
--
-- `purpose` gates consumption (`verify` or `reset`): a token minted for one
-- mail does not serve the other. `used_at` set means spent — consumption marks
-- it in the same statement that checks it is live, so a token redeems once
-- ever. Expiry is checked on consume; there is no sweeper until the P8-1 jobs
-- layer.
CREATE TABLE oxsum.email_tokens (
    token_id    uuid        PRIMARY KEY,
    user_id     uuid        NOT NULL REFERENCES oxsum.users (user_id) ON DELETE CASCADE,
    purpose     text        NOT NULL CHECK (purpose IN ('verify', 'reset')),
    token_hash  bytea       NOT NULL UNIQUE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz
);

CREATE INDEX email_tokens_user_purpose ON oxsum.email_tokens (user_id, purpose, created_at);

-- `NULL` means the address has not been verified through a mailed token.
ALTER TABLE oxsum.users ADD COLUMN email_verified_at timestamptz;
