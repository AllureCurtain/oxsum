-- The metering API's credential (issue #172, roadmap P8-4): a bearer the
-- deployer's own services present on `/api/v1/metering/*`. It belongs to no
-- organization — the request names the organization it meters for — which is
-- what keeps an organization from ever reporting its own usage.
--
-- The secret exists once, in the mint answer; the table keeps its SHA-256 hash
-- and a display prefix, the same discipline `api_keys` follows, so a dump of
-- `oxsum.service_credentials` authenticates nothing.

CREATE TABLE oxsum.service_credentials (
    credential_id uuid PRIMARY KEY,
    name        text,
    -- `oxs-svc-` plus the secret's first hex characters, for recognizing the
    -- credential in a list.
    prefix      text NOT NULL,
    secret_hash bytea NOT NULL UNIQUE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at  timestamptz
);
