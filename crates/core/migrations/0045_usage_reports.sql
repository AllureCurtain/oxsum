-- End-user signed usage reports (issue #261): an organization's downstream
-- customer holds nothing to verify a bill against, so the org produces a
-- report — the subset of settlement entries attributed to one `end_user` over
-- a span — and commits it into its own ledger once via a marker entry.
--
-- `usage_reports` is one row per minted report: the descriptor the customer
-- reads plus the marker's ledger identity. `usage_report_rows` freezes the
-- shown subset itself — the entry ids and content hashes the report
-- committed, in log order — so a re-fetch serves the identical rows a mint
-- produced and never re-queries: a report cannot grow after the fact to
-- absorb usage the org omitted the first time. The marker's ledger-side
-- idempotency key (`usage-report/<endUser>/<from>/<to>`) makes one marker per
-- span ever, and the UNIQUE on (tenant, end_user, span) is the same rule on
-- the table side.

CREATE TABLE oxsum.usage_reports (
    report_id       uuid        NOT NULL,   -- the marker entry's own id
    organization_id uuid        NOT NULL REFERENCES oxsum.organizations (organization_id),
    tenant_id       text        NOT NULL,
    end_user        text        NOT NULL,
    from_date       date        NOT NULL,
    to_date         date        NOT NULL,
    entry_count     bigint      NOT NULL CHECK (entry_count >= 0),
    charged_total_minor bigint  NOT NULL,
    subset_hash     text        NOT NULL,   -- fold of the shown content hashes, oxsum/usage-report/v1
    anchor_log_index bigint     NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (report_id),
    UNIQUE (tenant_id, end_user, from_date, to_date)
);

COMMENT ON TABLE oxsum.usage_reports IS
    'one signed end-user usage report per (organization, endUser, span) — the descriptor the customer reads, bound to the ledger marker once (issue #261)';
COMMENT ON COLUMN oxsum.usage_reports.report_id IS
    'the marker entry''s id — GET /api/v1/entries/{id}/proof resolves it to the marker''s inclusion proof';
COMMENT ON COLUMN oxsum.usage_reports.charged_total_minor IS
    'the subset''s summed settled charge — a customer''s own records spot-check this';

CREATE TABLE oxsum.usage_report_rows (
    report_id       uuid    NOT NULL REFERENCES oxsum.usage_reports (report_id) ON DELETE CASCADE,
    seq             integer NOT NULL,   -- the row's position in log order
    entry_id        uuid    NOT NULL,
    content_hash    text    NOT NULL,   -- lowercase hex, as the Merkle log commits it
    log_index       bigint  NOT NULL,
    PRIMARY KEY (report_id, seq)
);

COMMENT ON TABLE oxsum.usage_report_rows IS
    'the frozen subset a usage report committed — the shown entries in log order, never re-queried after mint (issue #261)';
