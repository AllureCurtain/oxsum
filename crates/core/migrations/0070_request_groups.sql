-- Request groups (issue #508): a key/channel scoping dimension for team-level
-- isolation. A group belongs to one organization and names a set of channels;
-- a key assigned to a group may only reach that group's channels, so an
-- operator can split traffic ("team X's requests run the paid OpenAI channel,
-- team Y's run the cheaper Ollama mirror") without a second organization.
--
-- These tables are mutable config, not ledger: a group's channel set edits
-- with admin writes and a key's assignment moves, the same shape `teams` and
-- `model_fallbacks` took. The gateway reads the key's group channels at
-- admission and restricts the serving candidates to them, so a group change
-- takes effect on the next request — no version pinned to a hold, because a
-- hold names a channel, never a group.
CREATE TABLE IF NOT EXISTS oxsum.groups (
    id          uuid        NOT NULL DEFAULT gen_random_uuid(),
    -- The organization the group belongs to: a group is single-org, and a key
    -- may only join a group of its own organization.
    org_id      uuid        NOT NULL REFERENCES oxsum.organizations(organization_id) ON DELETE RESTRICT,
    name        text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (org_id, name),
    UNIQUE (org_id, id)
);

-- The channels a group may serve through. A channel outside the set is not a
-- candidate for a grouped key's request.
CREATE TABLE IF NOT EXISTS oxsum.group_channels (
    group_id    uuid        NOT NULL REFERENCES oxsum.groups(id) ON DELETE CASCADE,
    channel_id  uuid        NOT NULL REFERENCES oxsum.channels(channel_id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, channel_id)
);

CREATE INDEX IF NOT EXISTS group_channels_ch ON oxsum.group_channels (channel_id);

-- Restrictive composite FK keeps assignment inside the key's organization.
-- Removing a group must never silently broaden a key's access.
ALTER TABLE oxsum.api_keys ADD COLUMN IF NOT EXISTS group_id uuid;
ALTER TABLE oxsum.api_keys ADD CONSTRAINT api_keys_org_group_fk
    FOREIGN KEY (organization_id, group_id) REFERENCES oxsum.groups(org_id, id) ON DELETE RESTRICT;
CREATE INDEX IF NOT EXISTS api_keys_group ON oxsum.api_keys (group_id) WHERE group_id IS NOT NULL;
