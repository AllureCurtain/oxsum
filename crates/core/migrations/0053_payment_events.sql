-- Webhook idempotency for the payment rails (issue #319): one row per provider
-- event the deployment has processed, keyed by the provider's own event id.
--
-- The deposit's status already makes `payment_succeeded` cheap to replay (only
-- a `pending` row moves, and the ledger append replays under its payment id),
-- but an event that arrives twice after the deposit reached `credited` would
-- still walk the generic path. Recording `event_id` before acting gives every
-- rail, and every future event type, one anchor: the handler acts only when its
-- insert landed.
--
-- `payload` keeps the event as received for 30 days (the retention job's
-- deliveries window is the operator's starting point), so a disputed event can
-- be read back without asking the provider.

CREATE TABLE IF NOT EXISTS oxsum.payment_events (
    event_id text PRIMARY KEY,
    rail text NOT NULL,
    event_type text NOT NULL,
    payload jsonb NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS payment_events_received_at_idx
    ON oxsum.payment_events (received_at);
