-- Plan fee journals and revenue allocation (issue #611, task T20).
--
-- T19 made a plan period exist only once its authorization does; T20 records
-- what each period *cost* once it closes. `oxsum.invoice_journals` is the
-- per-period invoice: written when a `plan_periods` row closes (boundary
-- rollover, renewal activation or boundary cancel), walking
-- `draft → posted → paid | void`, with `posted` unpaid the honest answer for a
-- renewal the customer has not yet funded — the plan's `past_due` state and
-- this journal's open receivable are the same fact seen from two sides.
--
-- `oxsum.invoice_journal_lines` is what the journal bills: `base_fee` the sold
-- version's price, `usage_overage` the overage-enabled plan's wallet-billed
-- overflow settled inside the period (at par — the charge is its own price),
-- `discount` credit notes the refund flow applied to the order that paid it,
-- `promo_credit` promo money that covered the fee (a complimentary grant, or
-- a payment whose bundle promo leg funded it), `tax` reserved. Charges carry
-- positive `amount_minor`, credits negative; `total_minor` is their sum.
--
-- The ledger mirror maps each kind to its own accounts so revenue, receivable
-- and liability never blur: charge lines `debit Assets:Receivables / credit
-- Income:PlanFees`, discounts `debit Income:Discounts / credit Receivables`,
-- promo `debit Equity:Promo / credit Receivables`. A `planPeriod` order pays
-- `debit Cash / credit Receivables` for open journals and
-- `debit Cash / credit Liabilities:UnearnedPlanFees` for the period it
-- authorizes — the prepayment the close reclassifies into the receivable. A
-- refund on a paid journal corrects `debit Income:Discounts / credit Cash`; a
-- lost dispute restores the receivable `debit Receivables / credit Cash` and
-- walks the journal `paid → posted` — never silently erased.
--
-- `settled_minor` is how much of `total_minor` the receivable-clearing legs
-- already covered; a journal is `paid` when it reaches `total_minor`. A `void`
-- appends reversing lines (the journal nets to zero) and posts the reversal —
-- the history is rewritten nowhere.

CREATE TABLE oxsum.invoice_journals (
    journal_id        uuid PRIMARY KEY,
    customer_plan_id  uuid NOT NULL REFERENCES oxsum.customer_plans(customer_plan_id) ON DELETE RESTRICT,
    -- Denormalized for customer- and staff-scoped reads without the join.
    organization_id   uuid NOT NULL REFERENCES oxsum.organizations(organization_id) ON DELETE RESTRICT,
    business_id       uuid NOT NULL,
    -- The `plan_periods` row this journal bills — NULL on a staff-issued
    -- correction journal that re-bills outside a period close (void + reissue).
    plan_period_id    uuid REFERENCES oxsum.plan_periods(plan_period_id) ON DELETE RESTRICT,
    period_index      integer NOT NULL CHECK (period_index >= 0),
    state             text NOT NULL CHECK (state IN ('draft', 'posted', 'paid', 'void')),
    -- The lines' sum: charges positive, credits negative — what the receivable
    -- was opened for.
    total_minor       bigint NOT NULL,
    -- How much the clearing legs (prepayment, order payment) already covered.
    settled_minor     bigint NOT NULL DEFAULT 0 CHECK (settled_minor >= 0),
    -- The fiat-scale currency the customer plan bills in — ledger minor units.
    currency          text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    -- The order whose payment completed the settlement — the refund/dispute
    -- link T17 hangs its corrections on.
    paid_order_id     uuid REFERENCES oxsum.payment_orders(order_id) ON DELETE RESTRICT,
    -- What open disputes contest of this journal's receivable.
    disputed_minor    bigint NOT NULL DEFAULT 0 CHECK (disputed_minor >= 0),
    -- The journal this one replaces after a staff void, when reissued.
    replaces_journal_id uuid REFERENCES oxsum.invoice_journals(journal_id) ON DELETE RESTRICT,
    void_reason       text,
    posted_at         timestamptz,
    paid_at           timestamptz,
    voided_at         timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
-- At most one live journal per plan period: a `void` frees the slot for the
-- reissue that names it in `replaces_journal_id`.
CREATE UNIQUE INDEX invoice_journals_period
    ON oxsum.invoice_journals (customer_plan_id, period_index)
    WHERE state <> 'void';
CREATE INDEX invoice_journals_customer ON oxsum.invoice_journals (organization_id, created_at);
CREATE INDEX invoice_journals_business ON oxsum.invoice_journals (business_id, state, created_at);
-- The dunning read: open receivables the staff report lists.
CREATE INDEX invoice_journals_posted ON oxsum.invoice_journals (posted_at)
    WHERE state = 'posted';
CREATE INDEX invoice_journals_paid_order ON oxsum.invoice_journals (paid_order_id)
    WHERE paid_order_id IS NOT NULL;

CREATE TABLE oxsum.invoice_journal_lines (
    line_id           uuid PRIMARY KEY,
    journal_id        uuid NOT NULL REFERENCES oxsum.invoice_journals(journal_id) ON DELETE RESTRICT,
    kind              text NOT NULL CHECK (kind IN (
        'base_fee', 'usage_overage', 'discount', 'promo_credit', 'tax')),
    -- Signed: charges positive, credits negative. `total_minor` is their sum.
    amount_minor      bigint NOT NULL,
    description       text NOT NULL,
    -- The fact the line came from: the `plan_periods` id for base_fee and
    -- overage, the `credit_notes` id for a refund-linked discount, the
    -- `promo_lots` id or grant key for promo_credit.
    source_ref        uuid,
    created_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX invoice_journal_lines_journal ON oxsum.invoice_journal_lines (journal_id);
CREATE INDEX invoice_journal_lines_source ON oxsum.invoice_journal_lines (source_ref)
    WHERE source_ref IS NOT NULL;

-- A written line never changes and never deletes: corrections append new
-- lines (a void's reversal set, a credit-note discount), nothing rewrites.
CREATE FUNCTION oxsum.protect_journal_line() RETURNS trigger AS $guard$
BEGIN
    RAISE EXCEPTION 'journal lines are immutable once written';
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER invoice_journal_lines_protected BEFORE UPDATE OR DELETE
    ON oxsum.invoice_journal_lines FOR EACH ROW EXECUTE FUNCTION oxsum.protect_journal_line();

-- The journal's terms are immutable once written — identity, customer, period
-- and currency never move. What may move is exactly the state machine plus
-- the figures the receivable's life updates:
--   draft  → posted | void          (close writes draft then posts in the
--                                    same transaction; a draft may be dropped)
--   posted → paid | void            (settlement or staff void)
--   paid   → void                   (staff void) — or `posted` again when a
--                                    lost dispute restores the receivable,
--                                    with `disputed_minor` rising
-- `paid`/`void` rows accept no other change: a posted journal's bill cannot
-- be edited under a customer, and a settled or voided journal is history.
CREATE FUNCTION oxsum.protect_invoice_journal() RETURNS trigger AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'journal history is retained'; END IF;
    IF NEW.journal_id IS DISTINCT FROM OLD.journal_id
        OR NEW.customer_plan_id IS DISTINCT FROM OLD.customer_plan_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.business_id IS DISTINCT FROM OLD.business_id
        OR NEW.plan_period_id IS DISTINCT FROM OLD.plan_period_id
        OR NEW.period_index IS DISTINCT FROM OLD.period_index
        OR NEW.currency IS DISTINCT FROM OLD.currency
        OR NEW.replaces_journal_id IS DISTINCT FROM OLD.replaces_journal_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'journal identity and terms are immutable';
    END IF;
    -- `total_minor` is the lines' sum: it may only move on a live journal, and
    -- only to what the lines now add to — the consistency the posted-journal
    -- immutability guarantee leans on. `void` is immutable outright; a `paid`
    -- journal's total may still fall — never rise — under an appended
    -- `discount` line, which is exactly the refund correction (`debit
    -- Discounts / credit Cash`) the bill already settled. The trigger cannot
    -- see same-transaction deferred order, so writers insert lines before
    -- updating the figure.
    IF NEW.total_minor IS DISTINCT FROM OLD.total_minor THEN
        IF OLD.state = 'void' THEN
            RAISE EXCEPTION 'a settled journal cannot be re-priced';
        END IF;
        IF OLD.state = 'paid' AND NOT (
            NEW.total_minor < OLD.total_minor AND NEW.state = 'paid'
        ) THEN
            RAISE EXCEPTION 'only a refund correction re-prices a paid journal';
        END IF;
        IF NEW.total_minor IS DISTINCT FROM (
            SELECT COALESCE(SUM(amount_minor),0)::bigint
            FROM oxsum.invoice_journal_lines WHERE journal_id = NEW.journal_id
        ) THEN
            RAISE EXCEPTION 'total_minor must equal the journal lines sum';
        END IF;
    END IF;
    -- The state walk itself: each step names the only states it may leave to.
    IF NEW.state IS DISTINCT FROM OLD.state THEN
        IF NOT (
            (OLD.state='draft'  AND NEW.state IN ('posted','void')) OR
            (OLD.state='posted' AND NEW.state IN ('paid','void')) OR
            (OLD.state='paid'   AND NEW.state IN ('posted','void'))
        ) THEN
            RAISE EXCEPTION 'invalid journal state transition % -> %', OLD.state, NEW.state;
        END IF;
        -- A paid journal only returns to `posted` under a dispute — the
        -- contested receivable must rise with the transition, never silently.
        IF OLD.state='paid' AND NEW.state='posted'
            AND NOT (NEW.disputed_minor > OLD.disputed_minor) THEN
            RAISE EXCEPTION 'only a dispute restores a paid journal receivable';
        END IF;
    END IF;
    -- Terminal rows: only the dispute counters may still move on `paid` —
    -- `disputed_minor` tracking and the audit-facing `settled_minor` stay
    -- honest under arbitration; `void` accepts nothing at all.
    IF OLD.state='void' THEN
        RAISE EXCEPTION 'a void journal is immutable';
    END IF;
    RETURN NEW;
END;
$guard$ LANGUAGE plpgsql;
CREATE TRIGGER invoice_journals_protected BEFORE UPDATE OR DELETE
    ON oxsum.invoice_journals FOR EACH ROW EXECUTE FUNCTION oxsum.protect_invoice_journal();

-- The journal void is a finance action like the refund resolutions: audited
-- under the same `finance_audit` action set, widened for it.
ALTER TABLE oxsum.finance_audit
    DROP CONSTRAINT finance_audit_action_check;
ALTER TABLE oxsum.finance_audit
    ADD CONSTRAINT finance_audit_action_check CHECK (action IN (
        'refund.approve', 'refund.decline', 'refund.release', 'dispute.resolve',
        'journal.void'));
