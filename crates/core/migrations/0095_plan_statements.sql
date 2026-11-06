-- Customer credit, plan statements and overdue handling (issue #612, task T21).
--
-- `oxsum.statements` already carries the monthly *usage* statement — the
-- credit-line receivable for metered spend (issues #124, #258). What T20's
-- `invoice_journals` bills per plan period is a different receivable on a
-- different cadence, and this migration gives it its customer-visible face:
-- `oxsum.plan_statements`, one row per `(customer_plan_id, period_index)`,
-- written inside the same close transaction `journal_closing_period_in` posts
-- the journal. The journal is the accounting; the statement is what the
-- customer sees — its lines restate the journal's in customer vocabulary
-- (`planFee`, `usage`, `discount`, `refundCredit`, `disputeChargeback`,
-- `paymentApplied`), and its `due_at` is the payment terms the receivable
-- carries.
--
-- The lifecycle is `state`: `open → issued → paid | past_due | void`, with
-- `past_due → issued` the staff release-review's one backward step. `open`
-- exists only inside the close transaction, like the journal's `draft`. A
-- `payment_applied` line reduces `total_minor − paid_minor` to zero and walks
-- `issued → paid`; a partial payment leaves `issued` with `paid_minor > 0`.
--
-- `customer_plans` gains the credit and dunning terms the admission and the
-- overdue sweep consult: `net_terms_days` the statement's `due_at` counts from
-- (default 14), `grace_days` the window past `due_at` a `past_due` statement
-- still serves under (default 7), and `credit_limit_minor` the cap on
-- wallet-side overage admission — NULL keeps the spend uncapped, the behavior
-- every existing binding already runs.
--
-- `paid_minor` is derived bookkeeping like the usage statement's: a payment
-- reconciles oldest open statement first, so the figure is what the wrapping
-- journal's `settled_minor` already carries read from the customer's side.

ALTER TABLE oxsum.customer_plans
    ADD COLUMN net_terms_days int NOT NULL DEFAULT 14
        CHECK (net_terms_days >= 0);
ALTER TABLE oxsum.customer_plans
    ADD COLUMN grace_days int NOT NULL DEFAULT 7
        CHECK (grace_days >= 0);
ALTER TABLE oxsum.customer_plans
    ADD COLUMN credit_limit_minor bigint
        CHECK (credit_limit_minor IS NULL OR credit_limit_minor > 0);

-- `dunning_suspended_at` marks which suspension the plan-statement sweep
-- placed — the same shape `organizations.dunning_suspended_at` (issue #258)
-- already proves: the plan's D06 lifecycle (`past_due` etc.) stays what it is;
-- suspension is the admission gate the hold path reads, not a lifecycle state.
-- The sweep stamps it past grace; the payment reconcile clears it — the same
-- "who set it lifts it" rule the org sweep honors.
ALTER TABLE oxsum.customer_plans
    ADD COLUMN dunning_suspended_at timestamptz;

CREATE TABLE oxsum.plan_statements (
    statement_id      uuid PRIMARY KEY,
    -- NULL allowed: an org may carry statements without a live plan binding —
    -- a wallet-only statement a dispute chargeback opens when no plan stands.
    customer_plan_id  uuid REFERENCES oxsum.customer_plans(customer_plan_id) ON DELETE RESTRICT,
    organization_id   uuid NOT NULL REFERENCES oxsum.organizations(organization_id) ON DELETE RESTRICT,
    business_id       uuid NOT NULL,
    -- The period this statement bills — the `(customer_plan_id, period_index)`
    -- pair the journal it wraps already keys on.
    period_index      integer CHECK (period_index >= 0),
    period_start      timestamptz,
    period_end        timestamptz,
    -- The invoice journal whose receivable this statement surfaces.
    journal_id        uuid REFERENCES oxsum.invoice_journals(journal_id) ON DELETE RESTRICT,
    state             text NOT NULL CHECK (state IN ('open','issued','paid','past_due','void')),
    -- The lines' sum — what the customer owes.
    total_minor       bigint NOT NULL,
    -- What reconciled payments already covered.
    paid_minor        bigint NOT NULL DEFAULT 0 CHECK (paid_minor >= 0),
    currency          text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    -- `issued_at + net_terms` — when the receivable falls due.
    issued_at         timestamptz,
    due_at            timestamptz,
    paid_at           timestamptz,
    -- First answer past due — the overdue sweep's stamp, or a re-read's.
    past_due_at       timestamptz,
    voided_at         timestamptz,
    void_reason       text,
    -- A void-and-reissue correction's successor.
    replaces_statement_id uuid REFERENCES oxsum.plan_statements(statement_id) ON DELETE RESTRICT,
    -- The order whose payment completed the settlement — the dispute link.
    paid_order_id     uuid REFERENCES oxsum.payment_orders(order_id) ON DELETE RESTRICT,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
-- At most one live statement per plan period: a `void` frees the slot for the
-- reissue that names it in `replaces_statement_id`.
CREATE UNIQUE INDEX plan_statements_period
    ON oxsum.plan_statements (customer_plan_id, period_index)
    WHERE state <> 'void' AND customer_plan_id IS NOT NULL;
CREATE INDEX plan_statements_customer ON oxsum.plan_statements (organization_id, created_at);
CREATE INDEX plan_statements_business ON oxsum.plan_statements (business_id, state, created_at);
-- The overdue work list: issued rows whose due date the sweep drains.
CREATE INDEX plan_statements_open ON oxsum.plan_statements (due_at)
    WHERE state = 'issued';

CREATE TABLE oxsum.plan_statement_lines (
    line_id           uuid PRIMARY KEY,
    statement_id      uuid NOT NULL REFERENCES oxsum.plan_statements(statement_id) ON DELETE RESTRICT,
    -- Customer vocabulary: the journal's `base_fee`→`plan_fee`,
    -- `usage_overage`→`usage`, `discount`→`discount`/`refund_credit`,
    -- `promo_credit`→`discount`, plus the payment and dispute lines only a
    -- statement carries.
    kind              text NOT NULL CHECK (kind IN (
        'plan_fee','usage','discount','refund_credit','dispute_chargeback',
        'payment_applied')),
    -- Signed: charges positive, credits negative. `total_minor` is their sum.
    amount_minor      bigint NOT NULL,
    description       text NOT NULL,
    -- The fact the line came from: the journal line id, the journal id, a
    -- refund or order id, a dispute id.
    source_ref        uuid,
    created_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX plan_statement_lines_statement ON oxsum.plan_statement_lines (statement_id);
CREATE INDEX plan_statement_lines_source ON oxsum.plan_statement_lines (source_ref)
    WHERE source_ref IS NOT NULL;

-- A written line never changes and never deletes — corrections append, the
-- same rule the journal lines live by.
CREATE FUNCTION oxsum.protect_plan_statement_line() RETURNS trigger AS $guard$
BEGIN
    RAISE EXCEPTION 'statement lines are immutable once written';
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER plan_statement_lines_protected BEFORE UPDATE OR DELETE
    ON oxsum.plan_statement_lines FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_statement_line();

-- The statement's terms are immutable once issued — identity, customer,
-- period, journal and currency never move. What may move is the state machine
-- plus the figures the receivable's life updates:
--   open     → issued | void          (close writes open then issues; a draft
--                                      may be dropped)
--   issued   → paid | past_due | void (settlement, the overdue sweep, void)
--   past_due → paid | issued | void   (a payment clears, release-review
--                                      restores, or staff voids)
--   paid     → void                   (staff void)
-- `total_minor` is the lines' sum and may move only on a live statement and
-- only to that exact figure — the refund/dispute corrections' rule. A `void`
-- row is immutable outright.
CREATE FUNCTION oxsum.protect_plan_statement() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'statement history is retained'; END IF;
    IF NEW.statement_id IS DISTINCT FROM OLD.statement_id
        OR NEW.customer_plan_id IS DISTINCT FROM OLD.customer_plan_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.period_index IS DISTINCT FROM OLD.period_index
        OR NEW.journal_id IS DISTINCT FROM OLD.journal_id
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.replaces_statement_id IS DISTINCT FROM OLD.replaces_statement_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'statement identity and terms are immutable';
    END IF;
    -- `total_minor` tracks the lines' sum on a live statement: the refund or
    -- dispute correction appends a line then lands the figure — the trigger
    -- sees the committed lines, so writers append before they update.
    IF NEW.total_minor IS DISTINCT FROM OLD.total_minor THEN
        IF OLD.state = 'void' THEN
            RAISE EXCEPTION 'a settled statement cannot be re-priced';
        END IF;
        IF NEW.total_minor IS DISTINCT FROM (
            SELECT COALESCE(SUM(amount_minor),0)::bigint
            FROM oxsum.plan_statement_lines WHERE statement_id = NEW.statement_id
        ) THEN
            RAISE EXCEPTION 'total_minor must equal the statement lines sum';
        END IF;
    END IF;
    IF NEW.state IS DISTINCT FROM OLD.state THEN
        IF NOT (
            (OLD.state='open'     AND NEW.state IN ('issued','void')) OR
            (OLD.state='issued'   AND NEW.state IN ('paid','past_due','void')) OR
            (OLD.state='past_due' AND NEW.state IN ('paid','issued','void')) OR
            (OLD.state='paid'     AND NEW.state IN ('void'))
        ) THEN
            RAISE EXCEPTION 'invalid statement state transition % -> %', OLD.state, NEW.state;
        END IF;
    END IF;
    IF OLD.state='void' THEN
        RAISE EXCEPTION 'a void statement is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER plan_statements_protected BEFORE UPDATE OR DELETE
    ON oxsum.plan_statements FOR EACH ROW EXECUTE FUNCTION oxsum.protect_plan_statement();

-- The release-review is a finance action like the journal void and the refund
-- resolutions: audited under the same `finance_audit` action set, widened for
-- it.
ALTER TABLE oxsum.finance_audit
    DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit
    ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
        'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
        'journal.void', 'statement.release'));
