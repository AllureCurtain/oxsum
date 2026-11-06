-- Financial-facts reconciliation runs, exceptions and repairs (issue #613,
-- task T22).
--
-- `crates/core/src/reconcile.rs` already scans the projections against the
-- ledger and answers drift classes; what it cannot do is remember a run, name
-- one offending fact, or record what a human did about it. This migration is
-- the financial reconciliation's memory: `reconciliation_runs` is one pass
-- over a period's books, `reconciliation_exceptions` one disagreed fact a run
-- found, and `reconciliation_repairs` one corrective action a staff actor
-- took against an exception — an explicit financial action, never a balance
-- overwrite.
--
-- A run keys on `(kind, period_start, period_end)` so a replay re-runs
-- idempotently under the run's own identity rather than minting a second
-- history. `expected_totals`/`actual_totals` carry the eight-category figures
-- each side computed — the financial-facts read model's categories — so the
-- evidence of *what* disagreed is frozen beside the run, not re-derived on
-- every read. `state` walks `running → matched | exceptions | failed`; a
-- `matched` run found every check within tolerance.
--
-- An exception is a fact, never silent: `expected`/`actual` name the two
-- figures that disagreed, `source_ref` the row the run blamed (a usage row's
-- request, a journal's id, an order's id — `source_table` says which),
-- `state` walks `open → resolved | waived`. A `resolved` carries a
-- `resolution` string; a `waived` names why the drift is not a defect — an
-- unresolved `open` exception is the work list the console drains.
--
-- A repair is the audited write a staff actor runs against an open
-- exception: it records `before`/`after` snapshots and the `finance_audit`
-- row that authorized it (`audit_ref`). It never mutates a money column in
-- place — a repair writes the correcting entry, line or row the exception's
-- check then re-runs against, the same rule the journal void and the refund
-- resolutions already live by.

CREATE TABLE oxsum.reconciliation_runs (
    run_id           uuid PRIMARY KEY,
    -- What scheduled the pass: `daily` the leader-elected job, `period_close`
    -- a close-time pass, `manual` a staff-requested re-run.
    kind             text NOT NULL CHECK (kind IN ('daily', 'period_close', 'manual')),
    period_start     date NOT NULL,
    period_end       date NOT NULL,
    state            text NOT NULL CHECK (state IN ('running', 'matched', 'exceptions', 'failed')),
    -- The eight-category figures each side computed, frozen at run time:
    -- usage/rated/entitlement/funding/earned/collected/refund/receivable,
    -- as the financial-facts read model spells them. `expected` is what the
    -- projections claim, `actual` what the journals/statements/orders show.
    expected_totals  jsonb NOT NULL DEFAULT '{}'::jsonb,
    actual_totals    jsonb NOT NULL DEFAULT '{}'::jsonb,
    exception_count  bigint NOT NULL DEFAULT 0 CHECK (exception_count >= 0),
    -- Who asked: NULL on a scheduled pass, the staff user id on a manual run.
    requested_by     uuid REFERENCES oxsum.users(user_id),
    started_at       timestamptz NOT NULL DEFAULT now(),
    finished_at      timestamptz,
    last_error       text,
    created_at       timestamptz NOT NULL DEFAULT now()
);
-- One run per (kind, period): a replay re-runs under the same identity.
CREATE UNIQUE INDEX reconciliation_runs_period
    ON oxsum.reconciliation_runs (kind, period_start, period_end);
CREATE INDEX reconciliation_runs_recent ON oxsum.reconciliation_runs (started_at DESC);

CREATE TABLE oxsum.reconciliation_exceptions (
    exception_id     uuid PRIMARY KEY,
    run_id           uuid NOT NULL REFERENCES oxsum.reconciliation_runs(run_id) ON DELETE RESTRICT,
    -- The customer organization the disagreed fact belongs to; NULL on a
    -- business-scope check (a provider total, a journal sum with no customer).
    organization_id  uuid REFERENCES oxsum.organizations(organization_id) ON DELETE RESTRICT,
    -- Which check failed. `unmatched_usage` a usage row no journal/statement
    -- covers; `unmatched_journal` a journal line no usage backs; `unmatched_payment`
    -- a paid order no receivable or wallet booked; `amount_drift` the same fact
    -- carried by two sources at different figures; `missing_cost` a settled turn
    -- with no cost basis where one was expected; `double_entry` a conservation
    -- invariant that does not hold.
    kind             text NOT NULL CHECK (kind IN (
        'unmatched_usage', 'unmatched_journal', 'unmatched_payment',
        'amount_drift', 'missing_cost', 'double_entry')),
    -- The two figures that disagreed, minor units; NULL where the check has no
    -- figure on one side (an unmatched row).
    expected_minor   bigint,
    actual_minor     bigint,
    -- The blamed row: `source_table` names the projection ('usage_records',
    -- 'invoice_journals', 'payment_orders', 'plan_statements', 'ledger'), and
    -- `source_ref` the identifier inside it (request id, journal id, order id,
    -- a ledger entry id spelled as text).
    source_table     text,
    source_ref       text,
    -- Free-text evidence the run captured — the check's own diff summary.
    detail           text,
    state            text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'resolved', 'waived')),
    -- Who closed it and what they said: `resolved` carries the corrective
    -- summary, `waived` why the drift is not a defect. `resolution` is required
    -- on either close — an exception never goes silent.
    resolved_by      uuid REFERENCES oxsum.users(user_id),
    resolution       text,
    resolved_at      timestamptz,
    created_at       timestamptz NOT NULL DEFAULT now(),
    CHECK (state = 'open' OR resolution IS NOT NULL)
);
CREATE INDEX reconciliation_exceptions_run ON oxsum.reconciliation_exceptions (run_id);
CREATE INDEX reconciliation_exceptions_open ON oxsum.reconciliation_exceptions (state, organization_id)
    WHERE state = 'open';
-- The same fact re-flagged by a later run does not mint a second open row:
-- dedupe on the blamed source while one is open.
CREATE UNIQUE INDEX reconciliation_exceptions_source_open
    ON oxsum.reconciliation_exceptions (kind, source_table, source_ref)
    WHERE state = 'open' AND source_ref IS NOT NULL;

CREATE TABLE oxsum.reconciliation_repairs (
    repair_id        uuid PRIMARY KEY,
    exception_id     uuid NOT NULL REFERENCES oxsum.reconciliation_exceptions(exception_id) ON DELETE RESTRICT,
    -- The staff actor who authorized the correction.
    actor_user_id    uuid NOT NULL REFERENCES oxsum.users(user_id),
    -- The corrective action taken: `reclassify_cost` rewrites a usage row's
    -- `cost_basis`/`upstream_cost_minor` with a before/after snapshot;
    -- `adjust_journal` appends a correcting journal line; `link_payment`
    -- binds an unmatched order to the receivable it settled; `mark_duplicate`
    -- records that a flagged row is a second write of one fact.
    action           text NOT NULL CHECK (action IN (
        'reclassify_cost', 'adjust_journal', 'link_payment', 'mark_duplicate')),
    -- The snapshots the action moved between — the row's money/basis fields
    -- before and after, so the audit reads what changed without re-deriving.
    before           jsonb NOT NULL DEFAULT '{}'::jsonb,
    after            jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- The finance-audit row authorizing the write.
    audit_ref        uuid REFERENCES oxsum.finance_audit(audit_id) DEFERRABLE INITIALLY DEFERRED,
    reason           text NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX reconciliation_repairs_exception ON oxsum.reconciliation_repairs (exception_id);
-- A written repair never changes: the correcting action is itself history.
CREATE TRIGGER reconciliation_repairs_immutable BEFORE UPDATE OR DELETE
    ON oxsum.reconciliation_repairs FOR EACH ROW
    EXECUTE FUNCTION oxsum.business_migrations_are_immutable();

-- The exception close and the repair are finance actions like the journal
-- void and the refund resolutions: audited under the same `finance_audit`
-- action set, widened for them.
ALTER TABLE oxsum.finance_audit
    DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit
    ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
        'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
        'journal.void', 'statement.release',
        'reconciliation.resolve', 'reconciliation.waive', 'reconciliation.repair'));
