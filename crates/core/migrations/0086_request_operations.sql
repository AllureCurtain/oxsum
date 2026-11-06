-- The durable request operation (issue #601, roadmap T10): one row per billable
-- turn, whatever surface served it — chat, responses, messages, embeddings,
-- rerank, or the async task relay. The operation is the turn's identity across
-- a crash: its idempotency fingerprint scopes a replay, its lease the sweeper
-- reads to tell "still alive but slow" from "dead", and its funding context
-- links the hold the ledger already watches.
--
-- `open_holds` is unchanged: it stays the sweeper's watch list, and an
-- operation whose hold lands still writes the watch row the sweeper reads.
-- Rows in `open_holds` that predate this table keep sweeping until T12
-- migrates them (docs/decisions.md, "the operation is the durable parent").

CREATE TABLE oxsum.request_operations (
    -- oxsum's own id — the request id the caller already quotes, minted at
    -- admission like today's `x-oxsum-request-id`. Primary key so a replay
    -- names the row directly.
    operation_id     uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The organization whose ledger holds the freeze.
    organization_id  uuid         NOT NULL REFERENCES oxsum.organizations (organization_id) ON DELETE RESTRICT,
    -- The API key that admitted, for attribution — the usage record's key.
    -- NULL where the caller is a service credential (metering) rather than a
    -- consumption key.
    key_id           uuid         REFERENCES oxsum.api_keys (key_id) ON DELETE RESTRICT,
    -- Which client surface served the turn: `chat`, `responses`, `messages`,
    -- `embeddings`, `rerank`, or `task:<kind>` for the task relay's kinds.
    surface          text         NOT NULL,
    -- The scoped idempotency fingerprint: the credential + surface + resource +
    -- body digest the gateway already computes (`credential_fingerprint`), so a
    -- retry under the same Idempotency-Key names the same operation instead of
    -- taking a second hold. Unique per organization — sibling keys' replays
    -- collide on the credential half and never read each other's row.
    fingerprint      text         NOT NULL,
    -- The request's declared upper bound — what the freeze prices — in minor
    -- units. Set at admission, before the hold; the settlement never charges
    -- past it.
    bound_minor      bigint       NOT NULL CHECK (bound_minor >= 0),
    -- The funding context: the hold key `req-<request id>:hold` the ledger
    -- watches. NULL while the operation is admitted but the hold not yet
    -- taken — the window where admission refused before money moved.
    hold_key         text,
    -- The terms snapshot digest: the price version(s) the turn pinned at
    -- admission, as the settlement's `price_version` records it. T08's sale
    -- book supersedes this with a real terms digest; until then the price
    -- version is the pin the bill proves against.
    terms_version    bigint,
    -- The customer the operation serves, when the business model names one —
    -- the customer profile's id. NULL on the self-hosted deployment's own
    -- organization-scoped serving.
    customer_id      uuid,
    -- Lifecycle: `created` at admission, `leased` while a worker owns it,
    -- `running` once upstream accepted, then one of the terminal states.
    state            text         NOT NULL DEFAULT 'created'
                     CHECK (state IN ('created','leased','running','succeeded','failed','cancelled','timed_out')),
    -- Lease fencing: the worker that owns the operation, and until when. A
    -- heartbeat extends `leased_until`; a sweeper claims a row whose lease
    -- lapsed. `lease_owner` is the worker's instance id — a uuid per process —
    -- so a crashed worker's operations are distinguishable from a live one's.
    lease_owner      text,
    leased_until     timestamptz,
    -- What the terminal settle charged; NULL while open.
    charged_minor    bigint       CHECK (charged_minor IS NULL OR charged_minor >= 0),
    -- The terminal cause, when the row carries one: the settlement kind's
    -- spelling, so `timed_out` operations and `swept` holds join the same
    -- anomaly surface.
    outcome_kind     text,
    -- Attribution the usage row carries, denormalized so a settled operation
    -- never re-asks the directory.
    model            text         NOT NULL,
    channel          text,
    created_at       timestamptz  NOT NULL DEFAULT now(),
    updated_at       timestamptz  NOT NULL DEFAULT now(),
    completed_at     timestamptz
);

-- The replay lookup: a retry under the same fingerprint names the same
-- operation. Scoped to the organization because the fingerprint already
-- binds the credential — two organizations' namespaces stay disjoint.
CREATE UNIQUE INDEX request_operations_fingerprint
    ON oxsum.request_operations (organization_id, fingerprint);

-- The sweeper's working set: open operations whose lease lapsed.
CREATE INDEX request_operations_leased
    ON oxsum.request_operations (leased_until)
    WHERE state IN ('created', 'leased', 'running');

-- One organization's list, newest first — the admin surface reads it.
CREATE INDEX request_operations_org
    ON oxsum.request_operations (organization_id, created_at DESC);

-- One row per upstream attempt under an operation. A failover records one
-- attempt per route tried; only the winning attempt's observed usage settles.
CREATE TABLE oxsum.request_attempts (
    attempt_id       uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
    operation_id     uuid         NOT NULL REFERENCES oxsum.request_operations (operation_id) ON DELETE RESTRICT,
    -- Which route served the attempt: the channel's name, denormalized.
    channel          text         NOT NULL,
    -- The credential's id inside the channel's pool — the pick the routing
    -- layer made, recorded so a bill names which account answered.
    credential_id    uuid,
    -- The upstream's own request/task id, when it gave one back — the task
    -- relay's `external_task_id`, or a chat turn's upstream `id` when it
    -- carries one. NULL while upstream never accepted.
    external_id      text,
    -- `started`, `answered`, `refused`, `unreachable` — the attempt's terminal
    -- marker, not the operation's.
    status           text         NOT NULL DEFAULT 'started'
                     CHECK (status IN ('started','answered','refused','unreachable')),
    -- What upstream's terminal answer reported — the `usage` object verbatim,
    -- or the explicit estimate block the degraded settle priced from. NULL
    -- while the attempt is open or when it reported nothing.
    usage_json       jsonb,
    -- The refusal's message, bounded — for the operation's outcome_reason.
    error            text,
    started_at       timestamptz  NOT NULL DEFAULT now(),
    completed_at     timestamptz
);

CREATE INDEX request_attempts_operation
    ON oxsum.request_attempts (operation_id, started_at);
