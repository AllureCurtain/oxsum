-- Statements: the monthly billing document (roadmap P3-2, issue #124). One row per
-- organization per closed UTC month, itemizing what the period's settled usage was
-- charged — the document an organization owes against when its billing is
-- "borrow first, settle monthly".
--
-- The document lifecycle is `status`: draft → finalized. A draft is the operator's
-- working copy — regenerating rebuilds it from the usage rows; finalizing locks the
-- lines, snapshots the organization's payment terms into the due date, and pins the
-- ledger window the lines prove (`log_from_index`/`log_to_index` over the covered
-- settlement entries).
--
-- The payment lifecycle is `payment_status`: pending → paid, with overdue and
-- suspended as the unpaid standings. What a statement is owed is `credit_drawn_minor`
-- — the part of the period's charges the credit line carried — because usage drawn
-- from bonus and purchased pools was paid for already. `paid_minor` is derived
-- bookkeeping, not an allocation event log: credit-line repayments settle the oldest
-- open statement first (FIFO matching against the ledger's own credit history), and
-- `reconcile_statements` rewrites the column whenever the ledger moves, so a self-serve
-- top-up pays a statement without anyone recording it.
--
-- `overdue_at` is the timestamp of first answering past due — the flip is lazy: reads
-- and reconcile paths persist it, no scheduler is required for a document that only
-- needs to be right when looked at.

ALTER TABLE oxsum.organizations
    ADD COLUMN payment_terms_days int NOT NULL DEFAULT 30;

CREATE TABLE oxsum.statements (
    statement_id       uuid        PRIMARY KEY,
    organization_id    uuid        NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    -- The billed month, `YYYY-MM`, UTC.
    period             text        NOT NULL CHECK (period ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    status             text        NOT NULL CHECK (status IN ('draft', 'finalized')),
    payment_status     text        NOT NULL CHECK (payment_status IN ('pending', 'paid', 'overdue', 'suspended')),
    -- The period's settled usage charges, all pools alike.
    total_minor        bigint      NOT NULL CHECK (total_minor >= 0),
    -- The part of the period's charges the credit line carried: what the statement is owed.
    credit_drawn_minor bigint      NOT NULL CHECK (credit_drawn_minor >= 0),
    paid_minor         bigint      NOT NULL DEFAULT 0 CHECK (paid_minor >= 0),
    -- Snapshotted from the organization at finalization.
    payment_terms_days int         NOT NULL DEFAULT 30 CHECK (payment_terms_days >= 0),
    due_date           date,
    -- The ledger window the lines prove: min and max log index of the covered
    -- settlement entries, set at finalization.
    log_from_index     bigint,
    log_to_index       bigint,
    entry_count        bigint      NOT NULL DEFAULT 0,
    finalized_at       timestamptz,
    paid_at            timestamptz,
    overdue_at         timestamptz,
    suspended_at       timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, period)
);

CREATE TABLE oxsum.statement_lines (
    statement_id    uuid   NOT NULL REFERENCES oxsum.statements (statement_id) ON DELETE CASCADE,
    line_no         int    NOT NULL,
    channel         text   NOT NULL,
    model           text   NOT NULL,
    turns           bigint NOT NULL CHECK (turns >= 0),
    input_tokens    bigint NOT NULL DEFAULT 0,
    output_tokens   bigint NOT NULL DEFAULT 0,
    cached_tokens   bigint NOT NULL DEFAULT 0,
    reasoning_tokens bigint NOT NULL DEFAULT 0,
    amount_minor    bigint NOT NULL CHECK (amount_minor >= 0),
    PRIMARY KEY (statement_id, line_no)
);

-- The reads: an organization's history, newest period first, and the open set —
-- finalized rows a reconcile or the admin list walks by payment standing.
CREATE INDEX statements_organization_period ON oxsum.statements (organization_id, period DESC);
CREATE INDEX statements_open ON oxsum.statements (organization_id, period)
    WHERE status = 'finalized' AND payment_status <> 'paid';
