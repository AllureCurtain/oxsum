-- Credential pools (issue #444): one channel carries several upstream keys, so
-- a single account's 429 or quota wall fails over onto another credential of
-- the *same* channel — same provider, same models, same price — before the
-- request has to leave the channel for a differently-priced route.
--
-- The pool is routing configuration, not ledger data: which credential answered
-- a turn never enters the settlement description, because the settlement names
-- `(channel, version)` and swapping keys inside one channel changes neither the
-- price nor the protocol. `status`/`cooldown_until`/`last_error` are mutable
-- health state — the channel_credential analogue of a job heartbeat, edited in
-- place like `jobs.locked_until`, unlike the append-only `channel_prices`.
--
-- The backfill keeps every existing deployment serving: each channel's stored
-- `api_key_sealed` becomes the pool's first credential, then the old columns
-- retire. `bill_key_sealed`/`bill_key_last4` stay on the channel: the admin
-- billing credential reads the org's bill, one per channel, never relayed.
CREATE TABLE oxsum.channel_credentials (
    credential_id  uuid        PRIMARY KEY,
    channel_id     uuid        NOT NULL REFERENCES oxsum.channels (channel_id) ON DELETE CASCADE,
    -- The upstream credential, sealed with AES-256-GCM under OXSUM_SECRET_KEY:
    -- base64 of nonce || ciphertext, same shape the channel column carried.
    api_key_sealed text        NOT NULL,
    -- The trailing characters, so a list can recognize a key. Never enough to authenticate.
    api_key_last4  text        NOT NULL,
    -- The pick order inside the pool when nothing else ranks: lower tries first.
    position       integer     NOT NULL CHECK (position > 0),
    -- `disabled` is the operator's switch: kept on record, never picked.
    status         text        NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled')),
    -- The health overlay's cool-down: a credential that keeps refusing upstream
    -- is skipped until this passes, then let back without an operator touching it.
    cooldown_until timestamptz,
    -- What cooled it, for the admin list; observability, never read by the pick.
    last_error     text,
    -- The `usage` pick's input: when this credential last led an upstream call.
    -- NULL means never used, which is the strongest "least used" there is.
    last_used_at   timestamptz,
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (channel_id, position)
);

-- The pool lookup the gateway makes on every request: one channel's healthy
-- credentials in pick order.
CREATE INDEX channel_credentials_pool
    ON oxsum.channel_credentials (channel_id, status, position);

INSERT INTO oxsum.channel_credentials
    (credential_id, channel_id, api_key_sealed, api_key_last4, position)
SELECT gen_random_uuid(), channel_id, api_key_sealed, api_key_last4, 1
FROM oxsum.channels;

ALTER TABLE oxsum.channels
    DROP COLUMN api_key_sealed,
    DROP COLUMN api_key_last4;
