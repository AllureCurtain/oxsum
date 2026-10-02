-- Channels and their prices: what the gateway relays to, and what it charges by.
--
-- Both tables are oxsum's own and live in the `oxsum` schema like the rest of them; 0001_identity.sql
-- explains why every statement and query here names its schema.
--
-- Prices are append-only, and the trigger below is what makes that a rule rather than a convention:
-- changing a price inserts the next version, and nothing — not the admin API, not a psql session —
-- can rewrite or delete a version. A bill that says it was priced by version 2 therefore says
-- something the database cannot take back (docs/product.md, "Channels and prices").

CREATE TABLE oxsum.channels (
    channel_id     uuid        PRIMARY KEY,
    name           text        NOT NULL UNIQUE,
    -- Where the OpenAI-compatible surface lives, without a trailing slash.
    base_url       text        NOT NULL,
    -- The upstream credential, sealed with AES-256-GCM under OXSUM_SECRET_KEY: base64 of
    -- nonce || ciphertext. The database never holds the key itself, and a dump of this table
    -- cannot call upstream.
    api_key_sealed text        NOT NULL,
    -- The trailing characters, so a list can recognize a key. Never enough to authenticate.
    api_key_last4  text        NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE oxsum.channel_prices (
    channel_id               uuid        NOT NULL REFERENCES oxsum.channels (channel_id) ON DELETE RESTRICT,
    model                    text        NOT NULL,
    -- One per change, from 1: the version in force when a request starts is the version that
    -- prices it, so history is the table rather than a column that moves.
    version                  integer     NOT NULL CHECK (version > 0),
    input_price_per_million  bigint      NOT NULL CHECK (input_price_per_million >= 0),
    output_price_per_million bigint      NOT NULL CHECK (output_price_per_million >= 0),
    max_output_tokens        bigint      NOT NULL CHECK (max_output_tokens > 0),
    created_at               timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (channel_id, model, version)
);

-- The current price of a model: the lookup the gateway makes for every request.
CREATE INDEX channel_prices_current ON oxsum.channel_prices (channel_id, model, version DESC);

CREATE FUNCTION oxsum.channel_prices_are_append_only() RETURNS trigger AS $append_only$
BEGIN
    RAISE EXCEPTION 'channel_prices is append-only: a price change adds a version';
END;
$append_only$ LANGUAGE plpgsql;

CREATE TRIGGER channel_prices_append_only
    BEFORE UPDATE OR DELETE ON oxsum.channel_prices
    FOR EACH ROW EXECUTE FUNCTION oxsum.channel_prices_are_append_only();
