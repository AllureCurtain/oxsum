-- Request-level idempotency on the gateway (issue #132, roadmap P3-5):
-- a client retry must never produce a second hold or charge. An
-- `Idempotency-Key` header on `POST /v1/chat/completions` claims one
-- row per (organization, key); a retry with the same fingerprint replays
-- the stored answer, a different fingerprint is refused, and a retry
-- landing while the first turn runs is refused.
--
-- `response_status`/`response_body` hold the replayable answer once the
-- turn is over — the stored body for a non-streamed turn, the settled
-- receipt for a streamed one. Both stay NULL while the turn is
-- `in_flight`. `expires_at` bounds the record's life at 24 hours; cleanup
-- is the jobs layer's (P8-1), and until then an expired row is released
-- lazily on the next claim under the same key.

CREATE TABLE oxsum.idempotency_records (
    organization_id uuid NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE CASCADE,
    idempotency_key text NOT NULL,
    request_fingerprint text NOT NULL,
    request_id text NOT NULL,
    status text NOT NULL CHECK (status IN ('in_flight', 'completed')),
    response_status integer,
    response_body jsonb,
    claimed_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (organization_id, idempotency_key)
);

-- The settled receipt completes a record by the request id the turn was
-- billed under, which is not the claim key itself.
CREATE INDEX idempotency_records_request_id ON oxsum.idempotency_records (request_id);
