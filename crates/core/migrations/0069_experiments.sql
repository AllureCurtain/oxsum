-- Experiments (issue #509): an A/B variant set a request may opt into — the
-- relay picks a variant by weight and serves its model, the settlement records
-- which ran, and a results read groups spend/usage by variant for offline
-- scoring. Scoring stays offline: oxsum reports the variants served, it does
-- not judge outputs or auto-promote a winner.
--
-- The variant set is a model chain's shape — `variants` is `[{model, weight}]`,
-- each naming a model the org could already call, so a variant resolves
-- through the same `servings_named` a plain request uses. The table is mutable
-- config (a binding an admin edits), not ledger — the same reason
-- `model_fallbacks` is its own table and `channel_prices` is append-only:
-- config changes, the ledger only ever accrues.
CREATE TABLE IF NOT EXISTS oxsum.experiments (
    -- The experiment's id — what `X-Oxsum-Experiment` and `experimentId` name.
    id          uuid        NOT NULL DEFAULT gen_random_uuid(),
    -- The organization it belongs to; a variant serves only that org's keys.
    org_id      uuid        NOT NULL,
    -- The operator's name for it, unique per org.
    name        text        NOT NULL,
    -- The variant set `[{model, weight}]`: resolved like a model chain's
    -- servings — each `model` a name the org could already call, `weight` the
    -- relative share the pick gives it. Validated at write time so a stored
    -- set is always servable.
    variants    jsonb       NOT NULL,
    -- 'open' serves variants; 'ended' stops serving — a request under it
    -- resolves the plain model instead.
    status      text        NOT NULL DEFAULT 'open'
                CHECK (status IN ('open', 'ended')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    ended_at    timestamptz,
    PRIMARY KEY (id),
    UNIQUE (org_id, name)
);
CREATE INDEX IF NOT EXISTS experiments_org ON oxsum.experiments (org_id);
