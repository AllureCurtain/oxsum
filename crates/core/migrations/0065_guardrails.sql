-- Guardrail pipeline (issue #502): the org-configured policies the gateway's
-- pre_call / post_call hooks evaluate, and the bindings that attach them to a
-- channel or a key. A policy is a named, versioned rule the org writes — a
-- keyword list, the built-in PII scan, or an external moderation endpoint the
-- callout posts the request to. A binding says which of them a channel or a
-- key runs: `required_policies` on `oxsum.api_keys` is the key-side half of
-- the same idea (a key's own requirement travels on its row, where the mandate
-- subset check and PATCH already live).
--
-- `config` is opaque to the schema — each kind's own shape, versioned by the
-- row it sits in rather than by the table: `keyword` holds `{patterns:[{re,
-- action}]}`, `pii` holds `{}` (the patterns are the built-in set, not config),
-- `moderation` holds `{endpoint, timeout_ms?}`. A moderation policy's bearer
-- rides on its own row as `secret_sealed` — the same AES-256-GCM-under-
-- OXSUM_SECRET_KEY shape `channel_credentials.api_key_sealed` carries, sealed
-- through the same `SecretKey`. It cannot live in `channel_credentials`:
-- a credential there belongs to a channel's upstream pool (`channel_id NOT
-- NULL`), which is routing state, and a moderation endpoint is not a channel.
CREATE TABLE IF NOT EXISTS oxsum.guardrail_policies (
    name          text PRIMARY KEY
                  CHECK (name ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    kind          text NOT NULL
                  CHECK (kind IN ('keyword', 'pii', 'moderation')),
    config        jsonb NOT NULL,
    secret_sealed text,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Which policies a channel or key runs. `target` is the channel's name or the
-- key's `key_id::text`; `policies` is the ordered set of policy names the
-- scope requires — validated to resolve on write so a dangling name can never
-- silently unguard a turn.
CREATE TABLE IF NOT EXISTS oxsum.guardrail_bindings (
    scope       text NOT NULL CHECK (scope IN ('channel', 'key')),
    target      text NOT NULL,
    policies    text[] NOT NULL,
    PRIMARY KEY (scope, target)
);

-- The key-side requirement, beside the other key constraints: a key minted or
-- PATCHed with `requiredPolicies` demands those policies run on its turns. A
-- mandate may only *narrow* the list — it can require a subset, never add one
-- the key does not already name.
ALTER TABLE oxsum.api_keys
    ADD COLUMN IF NOT EXISTS required_policies text[] DEFAULT NULL;

-- The mandate-side half: a mandate's own requirement, minted as a subset of
-- its key's `required_policies`. Stored on the row rather than the signed
-- blob — the blob's canonical bytes are the signature, and a mandate's
-- policies are server-side state the delegate never presents. jsonb like
-- `models`, the mandate's other narrowing field.
ALTER TABLE oxsum.mandates
    ADD COLUMN IF NOT EXISTS required_policies jsonb DEFAULT NULL;
