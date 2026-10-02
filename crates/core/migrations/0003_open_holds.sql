-- The hold sweeper's watch list (issue #13): one row per gateway hold that has been taken
-- and not yet settled.
--
-- The ledger stays the source of truth — a hold's amount and whether it is settled are read from
-- the hold entry, and the settlement entry's idempotency key is derived from the hold's key, so a
-- late settlement and the sweeper cannot both take effect. This table is only how the sweeper
-- *finds* stale holds: the pending layer says how much is held, not by which request or since
-- when, and paging every tenant's log on every pass would cost O(the log) each time. A row whose
-- hold is gone — the process died between writing the row and taking the hold, say — is deleted
-- without a ledger write, so a disagreement always resolves toward the ledger.
--
-- product.md's full `requests` table (item 5) supersedes this watch list when it arrives; the
-- sweeper reads from there then, and this table goes away.

CREATE TABLE oxsum.open_holds (
    -- `req-<request id>:hold`: the idempotency key the hold was taken under, and what the
    -- settlement names. One row per hold, so the primary key is the lookup the gateway uses
    -- when the turn settles.
    hold_key      text        PRIMARY KEY,
    -- The organization whose ledger holds the freeze, as its ledger tenant id.
    tenant_id     text        NOT NULL,
    -- From the `x-oxsum-request-id` header: what the caller quotes when asking about the bill.
    request_id    text        NOT NULL,
    model         text        NOT NULL,
    channel       text        NOT NULL,
    -- The price version in force when the turn started, so the swept record says what priced it.
    price_version bigint      NOT NULL,
    -- Minor units per million tokens at that version: the swept settlement reuses the
    -- settlement record's shape, which carries the prices beside the (zero) counts.
    input_price   bigint      NOT NULL,
    output_price  bigint      NOT NULL,
    -- What was frozen, in minor units: the swept record's `freeze`.
    freeze_minor  bigint      NOT NULL CHECK (freeze_minor > 0),
    opened_at     timestamptz NOT NULL DEFAULT now()
);

-- The sweeper's query: the rows older than the hold timeout.
CREATE INDEX open_holds_opened_at ON oxsum.open_holds (opened_at);
