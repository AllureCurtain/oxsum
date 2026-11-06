# T10 — Durable request operation: implementation design

Issue #601. Branch `feat/durable-request-operation`. Migration `0086_request_operations.sql`.

## What T10 builds

A durable parent row for every billable turn — streaming, non-streaming, and task — that
records the operation's identity, authority, pinned terms, funding context and lifecycle,
plus one child row per upstream attempt carrying the observed usage. The gateway already
runs freeze → relay → settle; T10 writes that lifecycle into `oxsum.request_operations` /
`oxsum.request_attempts` so a crashed worker's turn is recoverable by inspection, an
in-flight turn can heartbeat its lease, and a replayed submission binds to the same
operation instead of double-billing.

`oxsum.open_holds` stays exactly as it is — the sweeper keeps reading it. The operation
layer is additive: a row in `request_operations` links to the same `hold_key` the watch
row uses, and a swept operation's hold releases through the existing `sweep_one` path.
Old `open_holds` rows with no operation keep sweeping until T12 migrates them.

## Schema — `crates/core/migrations/0086_request_operations.sql`

```sql
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
```

Registered in `crates/core/src/db.rs` `MIGRATIONS` after `0085_merchant_configuration`.
(No 0080–0084 exist in tree — those numbers are taken on parallel branches; the runner
is a list, not a counter, so 0086 is fine.)

## Core module — `crates/core/src/operations.rs`

```rust
/// One billable turn's durable parent.
pub struct RequestOperation {
    pub operation_id: Uuid,
    pub organization_id: Uuid,
    pub key_id: Option<Uuid>,
    pub surface: String,          // "chat" | "responses" | "messages" | "embeddings" | "rerank" | "task:image" …
    pub fingerprint: String,
    pub bound_minor: i64,
    pub hold_key: Option<String>,
    pub terms_version: Option<i64>,
    pub customer_id: Option<Uuid>,
    pub state: OperationState,
    pub lease_owner: Option<String>,
    pub leased_until: Option<OffsetDateTime>,
    pub charged_minor: Option<i64>,
    pub outcome_kind: Option<String>,
    pub model: String,
    pub channel: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
}

pub enum OperationState { Created, Leased, Running, Succeeded, Failed, Cancelled, TimedOut }
// as_str / parse / is_terminal like TaskState.

pub struct NewOperation {
    pub organization_id: Uuid,
    pub key_id: Option<Uuid>,
    pub surface: String,
    pub fingerprint: String,
    pub bound_minor: i64,
    pub model: String,
    pub customer_id: Option<Uuid>,
    pub terms_version: Option<i64>,
}

pub struct OperationAttempt {
    pub attempt_id: Uuid,
    pub operation_id: Uuid,
    pub channel: String,
    pub credential_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub status: AttemptStatus,   // Started | Answered | Refused | Unreachable
    pub usage_json: Option<serde_json::Value>,
    pub error: Option<String>,
    pub started_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
}
```

Db methods on `impl Db`:

- `insert_operation(&self, op: &NewOperation) -> Result<RequestOperation, WalletError>`
  — `INSERT … ON CONFLICT (organization_id, fingerprint) DO NOTHING RETURNING *`; when the
  conflict wins (a replay landed first), re-read the row and return it with a flag the
  caller reads as "this is a replay". Signature returns `(RequestOperation, bool /* fresh */)`.
- `operation(&self, organization_id: Uuid, operation_id: Uuid) -> Result<Option<…>>`
- `operation_for_key(&self, organization_id, key_id, operation_id)` — the consumption
  key's scoped read, like `task_for_key`.
- `list_operations(&self, organization_id, limit, before)` — admin/member list.
- `bind_hold(&self, operation_id, hold_key) -> Result<()>` — sets `hold_key`,
  `state = 'leased'`, `lease_owner`, `leased_until = now() + LEASE_SECS`. CAS: only
  where `state IN ('created')` — a second admission of the same operation never
  re-binds.
- `heartbeat(&self, operation_id, lease_owner, extend_secs) -> Result<bool>` —
  `UPDATE … SET leased_until = now() + $3, updated_at = now() WHERE operation_id = $1
  AND lease_owner = $2 AND state IN ('leased','running')`. `false` = lease lost.
- `note_running(&self, operation_id, lease_owner) -> Result<bool>` — `leased` → `running`,
  CAS on owner.
- `record_attempt(&self, operation_id, channel, credential_id, external_id, status,
  usage_json, error) -> Result<Uuid>` — insert one attempt row; the winning attempt's
  `usage_json` is what the settle prices from.
- `complete_attempt(&self, attempt_id, status, usage_json, error) -> Result<()>`.
- `settle_operation(&self, operation_id, lease_owner, outcome_state, charged_minor,
  outcome_kind) -> Result<SettleOutcome, WalletError>` — terminal CAS: `UPDATE … SET
  state = $3, charged_minor = $4, outcome_kind = $5, completed_at = now() WHERE
  operation_id = $1 AND (lease_owner = $2 OR state IN ('created','leased','running'))
  AND state NOT IN ('succeeded','failed','cancelled','timed_out')`. Returns
  `Settled`, `AlreadySettled`, or `LeaseLost`.
- `operations_past_lease(&self, limit) -> Result<Vec<RequestOperation>>` — the
  sweeper's working set: `state IN ('created','leased','running') AND leased_until <
  now()` (or `state = 'created' AND created_at < now() - LEASE_SECS` for a row whose
  hold never bound).
- `claim_operation(&self, operation_id, new_owner, lease_secs) -> Result<bool>` —
  `UPDATE … SET lease_owner = $2, leased_until = now() + $3, state = 'leased' WHERE
  operation_id = $1 AND state IN ('created','leased','running') AND (leased_until <
  now() OR leased_until IS NULL)`. `false` = someone else got there — the CAS race's
  loser.

`LEASE_SECS: i64 = 60` — how long a bound hold may go without a heartbeat before the
sweeper claims the operation. Streaming heartbeats every ~15s (well under this).

## Gateway adoption — `crates/server/src/gateway/mod.rs` + `relay.rs` + `tasks.rs`

In `gateway()` (the shared handler):

1. Compute `bound_fingerprint` as today — that becomes the operation's `fingerprint`.
2. `insert_operation` with the surface name (`format!("{surface:?}").to_lowercase()` —
   map `OpenAi`→`chat`, `Responses`→`responses`, `Anthropic`→`messages`, `Embeddings`
   →`embeddings`, `Rerank`→`rerank`, `Gemini{..}`→`chat` — actually use
   `surface.upstream_path()`'s first segment; simplest correct: a small
   `surface.operation_name()` helper added next to `gen_ai_operation`). `bound_minor` is
   not yet known at this point — pass 0 and update at freeze time via a
   `note_bound(operation_id, bound_minor)` Db call, or create the operation after the
   freeze is computed (cleaner: inside `run`, once `freeze` is known but before
   `note_open_hold`). **Choose the second**: the operation row is created in `run`
   right before `note_open_hold`, with `bound_minor = freeze`. This keeps `bound_minor`
   honest and avoids a create-then-update window.
3. `bind_hold(operation_id, &hold_key, lease_owner)` where `hold_key` is the existing
   `req-<request_id>:hold`; `lease_owner` is the process's instance id — mint one
   `Uuid` in `AppState::new` (call it `instance_id`) and thread it through.
4. On each upstream attempt in the loop, `record_attempt` with the channel name and
   credential id; on refusal/unreachable, `complete_attempt` with the error.
5. In `Plan::write` (the settle path), before `wallet.settle`: `complete_attempt` for
   the winning attempt with `usage_json` = the serialized `UsageRecord` (or the
   estimate block for `Charge::Estimated`), then `settle_operation(operation_id,
   lease_owner, state, charged, kind.as_str())` mapping
   `SettlementKind`→`OperationState`: `Usage|Estimated|Capped|Unpriced` → `Succeeded`;
   `ClientCancelled` → `Cancelled`; `UpstreamError|UpstreamUnreachable|Swept|Released`
   → `Failed`; `Refused` → `Failed`. `heartbeat` every ~15s during a stream —
   simplest correct implementation: a `tokio::spawn`ed heartbeat task in
   `relay::stream` that calls `heartbeat` on an interval until the turn settles.
   Heartbeat failure is logged, never fatal.

For the task relay (`tasks.rs::submit`): same shape — `insert_operation` with surface
`task:<kind>`, `bind_hold`, `record_attempt` for the submit relay, `settle_operation`
inside `settle_task` after `wallet.settle`. `note_running` when the poll pass first
sees `running`.

`AppState` gains `instance_id: String` — a `Uuid::new_v4().to_string()` minted once.

## Recovery — sweeper reads `request_operations`

In `holds.rs::sweep_stale_holds`, *after* the existing `stale_open_holds` loop, a second
pass: `operations_past_lease`. For each: `claim_operation` (CAS — `false` means another
worker claimed it, skip), then drive the existing `sweep_one` on the operation's
`hold_key` — which settles the ledger hold at 0 with `SettlementKind::Swept` and clears
the watch row — then `settle_operation` to `timed_out` with `charged_minor = 0` and
`outcome_kind = "swept"`. A `claim_operation` false means a live worker still owns it
or it already went terminal — leave it.

The `open_holds` path is untouched: rows written before this table existed keep
sweeping on the old criterion (`opened_at < older_than`), which is the backward-compat
half the spec asks for.

## openapi.yaml — contract first

Bump `info.version` (0.110.0 → 0.111.0) and add to `x-consumption-cache`-style changelog
description. Add paths:

- `GET /api/v1/operations` — member-scoped list of the caller's own organization's
  operations (session auth), newest first, cursor-paged. Returns `{operations: […], nextCursor}`.
- `GET /api/v1/operations/{operationId}` — read one operation with its attempts.
- `GET /api/v1/admin/operations` — operator's all-orgs read (admin token), filtered by
  `organizationId`.
- `POST /v1/operations/{operationId}/cancel` — consumption-key scoped cancel for an
  open operation (mirrors task cancel semantics: best-effort upstream cancel where
  applicable, then release). *Defer if it complicates the PR — the spec's tests need
  lease race and replay, not a public cancel. Decide: include the GETs, omit the
  POST cancel for now — the member reads are the surface worth exposing, the cancel
  belongs with T12's coordinated settlement.*

Schemas: `RequestOperation`, `OperationAttempt`, `OperationListPage`. Fields camelCase
per convention.

## Tests — `crates/core/tests/operations.rs` + server additions

Core suite (needs `DATABASE_URL`, `oxsum_testkit::suite_db_url("operations")`, world
fixture like `jobs.rs`):

1. `an_operation_replays_under_the_same_fingerprint` — insert → insert again same
   fingerprint → same `operation_id`, `fresh = false`.
2. `a_changed_body_fingerprint_conflicts` — different fingerprint → different row (the
   409 mapping lives at the route layer; the Db test asserts the insert succeeds as a
   *new* operation).
3. `the_lease_cas_lets_one_worker_win` — two `claim_operation` calls on a lapsed row:
   first `true`, second `false`.
4. `a_heartbeat_extends_the_lease` — heartbeat → `leased_until` moved forward; wrong
   owner → `false`.
5. `settle_operation_is_terminal_once` — settle `succeeded` → second settle returns
   `AlreadySettled`.
6. `an_operation_past_its_lease_is_claimable` — force `leased_until` into the past via
   SQL, `operations_past_lease` lists it, `claim_operation` succeeds.
7. `the_winning_attempt_carries_the_usage` — record two attempts, complete the second
   with `usage_json`, read back — only one `answered`.
8. `an_old_open_hold_sweeps_without_an_operation` — seed `open_holds` directly (no
   operation), run `sweep_stale_holds`, assert the hold releases (backward compat).

Server suite additions — extend `crates/server/tests/tasks.rs` and a new
`crates/server/tests/operations.rs`:
9. `a_submit_writes_an_operation` — POST /v1/tasks → row in `request_operations` with
   `state` open and `hold_key` bound.
10. `a_settled_task_completes_its_operation` — task reaches terminal → operation is
    terminal with `charged_minor`.
11. `a_replayed_submit_replays_the_operation` — same Idempotency-Key + body → same
    `taskId` and same `operation_id`.
12. `a_chat_turn_writes_attempts_and_settles` — streaming turn against the stub
    upstream → operation `succeeded`, one `answered` attempt with `usage_json`.

## Docs

- `TODO.md`: mark `#601` in progress → done with PR number.
- `docs/api.md`: a short section on the operations read surface.
- `docs/architecture.md`: `crates/core/src/operations.rs` in the module map; a
  paragraph on operation-as-durable-parent beside the hold-sweeper paragraph.
- `docs/decisions.md`: entry "the operation is the durable parent of every billable
  turn" — why a second table beside `open_holds` (the watch row is the sweeper's
  working set, the operation is the turn's identity and recovery record), why lease
  + CAS rather than a lock column, why `request_attempts` is separate (one winner
  settles; losers are the failover audit).
- `docs/user-guide.md`: nothing customer-visible yet — the read surface is the only
  new API shape; mention under billing/requests if the operations list reads like a
  page. Minimal.
- `docs/proofs.md`: untouched — the bill's proof still comes from the ledger
  settlement, not the operation row.

## Commit plan

1. `feat: request_operations and request_attempts schema (migration 0086)`
2. `feat: operations core — create/lease/heartbeat/cas/settle`
3. `feat: gateway adopts the durable operation parent`
4. `feat: sweeper recovers lapsed operations`
5. `test: operation replay, lease race, and settlement`
6. `docs: contract, architecture and decisions for the operation layer`
