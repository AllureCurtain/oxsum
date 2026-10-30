-- Issue #440: team-level budgets between members and the organization credit
-- line.
--
-- A `team` is a named subgroup of members under one organization, carrying its
-- own `budget_limit_minor` — the middle layer the budget hierarchy missed:
-- key → member → **team** → organization credit line. The cap is enforced
-- inside `hold_for_key`'s locked window the way the member budget is: the
-- team's committed spend is summed from the ledger's own postings across every
-- key minted by the team's members (`team_committed_in`), never a side counter.
CREATE TABLE oxsum.teams (
    team_id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id  uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    name             text        NOT NULL,
    -- Caps the team's committed spend — settled charges plus outstanding
    -- holds summed over every key minted by the team's members. NULL carries
    -- no cap; 0 caps it at nothing, like a membership's `budget_limit_minor`.
    budget_limit_minor bigint    CHECK (budget_limit_minor >= 0),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, name)
);

-- `memberships.team_id` assigns a member to at most one team. NULL means
-- unassigned — no team cap applies to the member's keys. SET NULL rather than
-- cascading: dissolving a team must not drop the memberships it grouped; the
-- members simply become uncapped at the team layer again.
ALTER TABLE oxsum.memberships
    ADD COLUMN team_id uuid REFERENCES oxsum.teams (team_id) ON DELETE SET NULL;

-- The committed spend the hold path sums is grouped by who minted each key:
-- resolving a team's member set reads `team_id`, and `member_committed_in`
-- joins `api_keys.created_by`, so the index covers both directions of the
-- lookup inside the locked hold window.
CREATE INDEX memberships_team_id ON oxsum.memberships (team_id) WHERE team_id IS NOT NULL;
