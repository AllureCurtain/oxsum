-- Budget-consumption visibility (issue #232): a percentage dimension for the
-- spend a limit allows, and events when a threshold crosses.
--
-- `budget_thresholds` arms which consumption percentages an endpoint wants
-- `budget.threshold_crossed` deliveries for — the `balance_low_minor` pattern
-- at the percentage dimension: absent or empty means off, and the list is the
-- caller's choice (50/80/95 is the shape the issue cites, not a constant).
ALTER TABLE oxsum.webhook_endpoints
    ADD COLUMN budget_thresholds smallint[] NOT NULL DEFAULT '{}';

-- The once-per-(endpoint, scope, window, threshold) latch: a crossing writes
-- exactly one row, so a second settle past the same line enqueues nothing and
-- a replayed usage row refires nothing — the latch inside `record_usage`'s
-- transaction is the at-most-once guarantee `balance_low_sent` gives, keyed
-- instead of boolean so every threshold is its own latch.
--
-- `scope` is which limit the percentage measures: `key` (a key's
-- spendLimitMinor, windowed by its budgetDuration), `member` (a membership's
-- budgetLimitMinor across the member's keys), `organization` (the credit
-- line). `window_start` is the budget window's period start; cumulative
-- scopes latch on the epoch, since a cumulative threshold once crossed stays
-- crossed until the limit itself is re-set — a constraint PATCH clears the
-- scope's rows, re-arming the way `budget_forecast_sent_for` resets.
CREATE TABLE oxsum.budget_threshold_fires (
    endpoint_id       uuid        NOT NULL REFERENCES oxsum.webhook_endpoints (endpoint_id) ON DELETE CASCADE,
    scope             text        NOT NULL CHECK (scope IN ('organization', 'member', 'key')),
    scope_id          uuid        NOT NULL,
    window_start      date        NOT NULL,
    threshold_percent smallint    NOT NULL CHECK (threshold_percent BETWEEN 1 AND 100),
    fired_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (endpoint_id, scope, scope_id, window_start, threshold_percent)
);
