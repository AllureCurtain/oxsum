-- Auto-suspend on overdue statements (issue #258): the grace an organization
-- gets past a finalized statement's due date before the line is cut.
--
-- `dunning_grace_days` is per-org because payment terms already are: an org on
-- net-30 and an org on net-7 deserve different dunning windows. NULL keeps the
-- existing behavior — no automatic suspension — so the column defaults to off
-- for every organization that already exists, and an admin opts in per org.
--
-- `dunning_suspended_at` marks which suspension the sweep itself placed: the
-- same daily pass reinstates an organization once nothing unpaid stands past
-- due, but only when it set the flag — an operator's manual suspension through
-- `set_suspended` stays until the operator lifts it.
--
-- The dunning sweep runs inside the `statements` periodic job: overdue flips
-- happen there already, and suspending the organization is the same job's
-- natural next step — no new kind, no new schedule.

ALTER TABLE oxsum.organizations
    ADD COLUMN dunning_grace_days int;

-- A non-negative grace: 0 suspends the day after the due date, NULL disables
-- the automation entirely. Negative days make no sense as a grace period.
ALTER TABLE oxsum.organizations
    ADD CONSTRAINT organizations_dunning_grace_days_nonneg
    CHECK (dunning_grace_days IS NULL OR dunning_grace_days >= 0);

ALTER TABLE oxsum.organizations
    ADD COLUMN dunning_suspended_at timestamptz;
