-- Model aliases (issue #286): a public name that resolves to one channel's
-- upstream model, so a deployment can serve `gpt-4o` under its own public name
-- (or two upstream models behind one alias over time) without callers learning
-- the upstream names.
--
-- `alias` is the primary key because uniqueness is the point: one name resolves
-- to one route set, so a name already claimed — by an alias on any channel —
-- refuses a second claim. `channel_id` pins the alias to the channel that
-- serves it: an alias is a rename, not a second routing layer, so it never
-- fails over across channels on its own. `upstream_model` is the name the
-- channel's upstream actually answers.
CREATE TABLE oxsum.model_aliases (
    -- The public name callers write; unique across channels.
    alias           text        PRIMARY KEY,
    -- The channel that serves it.
    channel_id      uuid        NOT NULL REFERENCES oxsum.channels (channel_id),
    -- The upstream model name the channel relays.
    upstream_model  text        NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX model_aliases_channel ON oxsum.model_aliases (channel_id);
