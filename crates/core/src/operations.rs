//! The durable request operation (issue #601, roadmap T10): one row per billable
//! turn, whatever surface served it — chat, responses, messages, embeddings,
//! rerank, or the async task relay.
//!
//! The operation is the turn's identity across a crash. The gateway already runs
//! freeze → relay → settle; this table writes that lifecycle down: the operation
//! row is created once the freeze is known, binds the same `hold_key` the
//! sweeper's watch list holds, heartbeats while upstream streams, and goes
//! terminal on the same condition the settlement wrote under. A replay under the
//! same scoped fingerprint names the same row instead of taking a second hold,
//! and a crashed worker's operation is recoverable by inspection: its lease
//! lapses, the sweeper claims it, and the hold it bound releases through the
//! existing `sweep_one` path.
//!
//! `open_holds` stays the sweeper's watch list — the operation layer is
//! additive. A row in `request_operations` links to the watch row by `hold_key`,
//! never replaces it; watch rows written before this table existed keep sweeping
//! on the old criterion until T12 migrates them.
//!
//! `request_attempts` is the child table: one row per upstream attempt under an
//! operation. A failover records one attempt per route tried; only the winning
//! attempt's `usage_json` is what the settle prices from — the losers are the
//! failover audit.

use sqlx::Row;
use std::str::FromStr;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// How long a bound hold may go without a heartbeat before the sweeper claims
/// the operation. Streaming heartbeats every ~15 s — well under this — and a
/// task's poll pass is the heartbeat that matters on the minutes timescale.
pub const LEASE_SECS: i64 = 60;

/// The lease owner a sweeper claims a lapsed operation under. Fixed rather than
/// an instance id on purpose: a crashed sweeper's claims are distinguishable
/// from a live worker's, and a second sweeper racing the same row loses the
/// CAS — which is the point of claiming rather than locking.
pub const SWEEPER_OWNER: &str = "sweeper";

/// The `request_operations.surface` spelling a metered event's operation
/// carries (issue #604): the privileged service-credential intake, distinct
/// from every customer surface's name, so a coverage read can tell an internal
/// metered event from a customer-billable gateway turn.
pub const METERING_SURFACE: &str = "metering";

/// The `outcome_kind` a privileged metered event's operation closes under
/// (issue #604): `metered`, never `usage` — so a cost-allocation read over the
/// operation rows never confuses a staff-triggered internal event with the
/// customer-billable usage a real upstream attempt produced.
pub const METERED_OUTCOME: &str = "metered";

/// Where an operation is in its life — `created`, `leased` and `running` are
/// open, everything else is terminal and means the settle CAS already ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    /// Written at admission, before the hold bound.
    Created,
    /// The hold bound; a worker owns the turn.
    Leased,
    /// Upstream accepted — first answer bytes, or the task's first `running` poll.
    Running,
    /// Terminal: the turn settled against observed usage (or the degraded
    /// estimate, or a cap — every billed kind maps here).
    Succeeded,
    /// Terminal: the turn errored or released without billing.
    Failed,
    /// Terminal: the caller cancelled — a client disconnect's settle.
    Cancelled,
    /// Terminal: the lease lapsed and the sweeper claimed the operation — the
    /// same anomaly the `swept` settlement kind records.
    TimedOut,
}

impl OperationState {
    /// The spelling `request_operations.state` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Leased => "leased",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }

    /// Reads a stored state back; an unknown one is a row this build cannot
    /// interpret and is refused rather than guessed.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] on an unknown state.
    pub fn parse(stored: &str) -> Result<Self, WalletError> {
        stored.parse()
    }

    /// Whether the state is terminal — settled, no more heartbeats.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }
}

impl FromStr for OperationState {
    type Err = WalletError;

    fn from_str(stored: &str) -> Result<Self, Self::Err> {
        Ok(match stored {
            "created" => Self::Created,
            "leased" => Self::Leased,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "timed_out" => Self::TimedOut,
            other => {
                return Err(WalletError::InvalidInput(format!(
                    "unknown operation state {other:?}"
                )));
            }
        })
    }
}

/// An attempt's terminal marker — the route-level outcome, not the
/// operation's: a refused attempt fails over, an answered one's usage settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    /// Written when the route is tried; still open while upstream decides.
    Started,
    /// Upstream accepted and answered — the winning attempt.
    Answered,
    /// Upstream refused: a 4xx/429 the failover moved past.
    Refused,
    /// The route never reached upstream — connect refused, DNS, timeout.
    Unreachable,
}

impl AttemptStatus {
    /// The spelling `request_attempts.status` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Answered => "answered",
            Self::Refused => "refused",
            Self::Unreachable => "unreachable",
        }
    }

    /// Reads a stored status back.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] on an unknown status.
    pub fn parse(stored: &str) -> Result<Self, WalletError> {
        stored.parse()
    }
}

impl FromStr for AttemptStatus {
    type Err = WalletError;

    fn from_str(stored: &str) -> Result<Self, Self::Err> {
        Ok(match stored {
            "started" => Self::Started,
            "answered" => Self::Answered,
            "refused" => Self::Refused,
            "unreachable" => Self::Unreachable,
            other => {
                return Err(WalletError::InvalidInput(format!(
                    "unknown attempt status {other:?}"
                )));
            }
        })
    }
}

/// One billable turn's durable parent, as `oxsum.request_operations` holds it.
#[derive(Debug, Clone)]
pub struct RequestOperation {
    /// oxsum's own id — the request id the caller already quotes.
    pub operation_id: Uuid,
    /// The organization whose ledger holds the freeze.
    pub organization_id: Uuid,
    /// The consumption key that admitted; `None` on a service-credential turn.
    pub key_id: Option<Uuid>,
    /// Which surface served the turn: `chat`, `responses`, `messages`,
    /// `embeddings`, `rerank`, or `task:<kind>`.
    pub surface: String,
    /// The scoped idempotency fingerprint — `credential_fingerprint` over the
    /// credential, surface, resource and body. A retry names the same row.
    pub fingerprint: String,
    /// The declared upper bound the freeze priced, in minor units.
    pub bound_minor: i64,
    /// The hold key `req-<request id>:hold` the ledger watches; `None` while
    /// admitted-but-not-held.
    pub hold_key: Option<String>,
    /// The price version the turn pinned at admission.
    pub terms_version: Option<i64>,
    /// The customer the operation serves, when the business model names one.
    pub customer_id: Option<Uuid>,
    pub state: OperationState,
    /// The worker that owns the lease — a per-process instance id, or
    /// [`SWEEPER_OWNER`] on a claimed row.
    pub lease_owner: Option<String>,
    /// When the current lease lapses; a sweeper may claim the row past it.
    pub leased_until: Option<OffsetDateTime>,
    /// What the terminal settle charged; `None` while open.
    pub charged_minor: Option<i64>,
    /// The settlement kind's spelling, when the row carries one.
    pub outcome_kind: Option<String>,
    pub model: String,
    /// The channel the turn served under — the primary route at admission,
    /// rewritten by a failover's `reroute` only on the settlement, not here:
    /// the operation names where the turn started.
    pub channel: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
}

/// What an admission writes: everything the operation row needs before the
/// hold binds — the rest fills in through [`Db::bind_hold`] and the lifecycle
/// calls.
#[derive(Debug)]
pub struct NewOperation {
    /// oxsum's operation id — minted by the caller so the request id and the
    /// operation id are the same figure.
    pub operation_id: Uuid,
    pub organization_id: Uuid,
    pub key_id: Option<Uuid>,
    /// The surface spelling, e.g. `chat` or `task:image`.
    pub surface: String,
    pub fingerprint: String,
    /// The freeze the turn admitted under, in minor units.
    pub bound_minor: i64,
    pub model: String,
    /// The primary route's channel — where the turn starts.
    pub channel: Option<String>,
    pub customer_id: Option<Uuid>,
    /// The price version the turn pinned — the primary route's.
    pub terms_version: Option<i64>,
}

/// One upstream attempt under an operation — the failover audit and the
/// winning attempt's usage record.
#[derive(Debug, Clone)]
pub struct OperationAttempt {
    pub attempt_id: Uuid,
    pub operation_id: Uuid,
    /// The channel's name, denormalized.
    pub channel: String,
    /// The credential the routing layer picked inside the channel's pool.
    pub credential_id: Option<Uuid>,
    /// Upstream's own request/task id, when it gave one back.
    pub external_id: Option<String>,
    pub status: AttemptStatus,
    /// What upstream's terminal answer reported — the `usage` object verbatim,
    /// or the estimate block the degraded settle priced from.
    pub usage_json: Option<serde_json::Value>,
    /// The refusal's message, bounded.
    pub error: Option<String>,
    pub started_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
}

/// What a terminal settle CAS answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationSettle {
    /// The settle landed: the row is terminal under this caller's outcome.
    Settled,
    /// The row was already terminal — a raced double-settle loses here, which
    /// is `Drop`-settles-once staying honest, not an error.
    AlreadySettled,
    /// The row is open but someone else owns it: a live worker's heartbeat
    /// beat this writer, or a sweeper claimed the lapsed lease first.
    LeaseLost,
}

const OPERATION_COLS: &str = "operation_id, organization_id, key_id, surface, \
     fingerprint, bound_minor, hold_key, terms_version, customer_id, state, \
     lease_owner, leased_until, charged_minor, outcome_kind, model, channel, \
     created_at, updated_at, completed_at";

fn operation_from_row(row: &sqlx::postgres::PgRow) -> Result<RequestOperation, WalletError> {
    Ok(RequestOperation {
        operation_id: row.try_get("operation_id")?,
        organization_id: row.try_get("organization_id")?,
        key_id: row.try_get("key_id")?,
        surface: row.try_get("surface")?,
        fingerprint: row.try_get("fingerprint")?,
        bound_minor: row.try_get("bound_minor")?,
        hold_key: row.try_get("hold_key")?,
        terms_version: row.try_get("terms_version")?,
        customer_id: row.try_get("customer_id")?,
        state: OperationState::parse(&row.try_get::<String, _>("state")?)?,
        lease_owner: row.try_get("lease_owner")?,
        leased_until: row.try_get("leased_until")?,
        charged_minor: row.try_get("charged_minor")?,
        outcome_kind: row.try_get("outcome_kind")?,
        model: row.try_get("model")?,
        channel: row.try_get("channel")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        completed_at: row.try_get("completed_at")?,
    })
}

fn attempt_from_row(row: &sqlx::postgres::PgRow) -> Result<OperationAttempt, WalletError> {
    Ok(OperationAttempt {
        attempt_id: row.try_get("attempt_id")?,
        operation_id: row.try_get("operation_id")?,
        channel: row.try_get("channel")?,
        credential_id: row.try_get("credential_id")?,
        external_id: row.try_get("external_id")?,
        status: AttemptStatus::parse(&row.try_get::<String, _>("status")?)?,
        usage_json: row.try_get("usage_json")?,
        error: row.try_get("error")?,
        started_at: row.try_get("started_at")?,
        completed_at: row.try_get("completed_at")?,
    })
}

impl Db {
    /// Writes the operation row an admission creates, once the freeze is known
    /// but before the hold binds. `ON CONFLICT (organization_id, fingerprint)
    /// DO NOTHING` makes the scoped fingerprint the replay lookup: a retry
    /// under the same idempotency claim names the same row rather than
    /// writing a second one. The returned pair carries the row plus `fresh` —
    /// `false` means the insert lost to a replay that landed first, and the
    /// caller should bind itself to the existing row rather than start a new
    /// turn's billing under it.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn insert_operation(
        &self,
        operation: &NewOperation,
    ) -> Result<(RequestOperation, bool), WalletError> {
        let inserted = sqlx::query(&format!(
            "INSERT INTO oxsum.request_operations \
             (operation_id, organization_id, key_id, surface, fingerprint, \
              bound_minor, model, channel, customer_id, terms_version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (organization_id, fingerprint) DO NOTHING \
             RETURNING {OPERATION_COLS}"
        ))
        .bind(operation.operation_id)
        .bind(operation.organization_id)
        .bind(operation.key_id)
        .bind(&operation.surface)
        .bind(&operation.fingerprint)
        .bind(operation.bound_minor)
        .bind(&operation.model)
        .bind(&operation.channel)
        .bind(operation.customer_id)
        .bind(operation.terms_version)
        .fetch_optional(self.pool())
        .await?;
        if let Some(row) = inserted {
            return Ok((operation_from_row(&row)?, true));
        }
        // The conflict won: a replay landed first, or the same fingerprint was
        // written before. Re-read the winner's row and hand it back marked
        // not-fresh — the caller binds to it rather than starting over.
        let row = sqlx::query(&format!(
            "SELECT {OPERATION_COLS} FROM oxsum.request_operations \
             WHERE organization_id = $1 AND fingerprint = $2"
        ))
        .bind(operation.organization_id)
        .bind(&operation.fingerprint)
        .fetch_one(self.pool())
        .await?;
        Ok((operation_from_row(&row)?, false))
    }

    /// Looks an operation up by its scoped fingerprint — the replay anchor the
    /// metering surface uses (issue #604): a metered event's `hold_key` is the
    /// operation's fingerprint, so the settle names the row the hold wrote
    /// rather than carrying an id it never saw. `None` covers absent and
    /// foreign alike — an operation's existence is not an oracle across
    /// organizations.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn operation_by_fingerprint(
        &self,
        organization_id: Uuid,
        fingerprint: &str,
    ) -> Result<Option<RequestOperation>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {OPERATION_COLS} FROM oxsum.request_operations \
             WHERE organization_id = $1 AND fingerprint = $2"
        ))
        .bind(organization_id)
        .bind(fingerprint)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| operation_from_row(&row)).transpose()
    }

    /// Reads one operation — the caller's detail read, owned by
    /// `organization_id`. `None` covers both absent and foreign: an
    /// operation's existence is not an oracle for another organization's
    /// keys.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn operation(
        &self,
        organization_id: Uuid,
        operation_id: Uuid,
    ) -> Result<Option<RequestOperation>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {OPERATION_COLS} FROM oxsum.request_operations \
             WHERE operation_id = $1 AND organization_id = $2"
        ))
        .bind(operation_id)
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| operation_from_row(&row)).transpose()
    }

    /// The consumption key's scoped read: only the submitting key's own
    /// operation answers, like [`Db::task_for_key`]. A session's read is
    /// [`Db::operation`] — a session may name any operation in its
    /// organization; a key may name only its own.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn operation_for_key(
        &self,
        organization_id: Uuid,
        key_id: Uuid,
        operation_id: Uuid,
    ) -> Result<Option<RequestOperation>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {OPERATION_COLS} FROM oxsum.request_operations \
             WHERE operation_id = $1 AND organization_id = $2 AND key_id = $3"
        ))
        .bind(operation_id)
        .bind(organization_id)
        .bind(key_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| operation_from_row(&row)).transpose()
    }

    /// One organization's operations, newest first — the member list and the
    /// admin's filtered read share it. `organization_id` is the scope:
    /// `Some` narrows to one org, `None` reads every org (the admin
    /// endpoint). `key_id` narrows to one key's own operations — the
    /// consumption credential's scoped read; `state` filters the lifecycle.
    /// `after` is the keyset `(created_at, operation_id)` the previous page
    /// ended at, the same codec the request-log page uses.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn list_operations(
        &self,
        organization_id: Option<Uuid>,
        key_id: Option<Uuid>,
        state: Option<&str>,
        limit: i64,
        after: Option<(OffsetDateTime, Uuid)>,
    ) -> Result<Vec<RequestOperation>, WalletError> {
        let (after_at, after_id) = after
            .map(|(at, id)| (Some(at), Some(id)))
            .unwrap_or((None, None));
        let rows = sqlx::query(&format!(
            "SELECT {OPERATION_COLS} FROM oxsum.request_operations \
             WHERE ($1::uuid IS NULL OR organization_id = $1) \
               AND ($5::uuid IS NULL OR key_id = $5) \
               AND ($6::text IS NULL OR state = $6) \
               AND ($2::timestamptz IS NULL OR (created_at, operation_id) < ($2, $3)) \
             ORDER BY created_at DESC, operation_id DESC \
             LIMIT $4"
        ))
        .bind(organization_id)
        .bind(after_at)
        .bind(after_id)
        .bind(limit)
        .bind(key_id)
        .bind(state)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(operation_from_row).collect()
    }

    /// Binds the operation to the hold it froze under: sets `hold_key`, moves
    /// `created` → `leased`, stamps the worker's lease. CAS on `state =
    /// 'created'` — a second admission of the same operation never re-binds,
    /// which is the replay path staying honest: a replayed submit's `bind_hold`
    /// on an already-leased row answers `false`, and the caller reads that as
    /// "the operation belongs to a turn already in flight".
    ///
    /// `lease_owner` is the worker's instance id — a uuid per process — so a
    /// crashed worker's operations are distinguishable from a live one's.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn bind_hold(
        &self,
        operation_id: Uuid,
        hold_key: &str,
        lease_owner: &str,
    ) -> Result<bool, WalletError> {
        let bound = sqlx::query_scalar::<_, uuid::Uuid>(
            "UPDATE oxsum.request_operations \
             SET hold_key = $2, state = 'leased', lease_owner = $3, \
                 leased_until = now() + ($4 || ' seconds')::interval, updated_at = now() \
             WHERE operation_id = $1 AND state = 'created' \
             RETURNING operation_id",
        )
        .bind(operation_id)
        .bind(hold_key)
        .bind(lease_owner)
        .bind(LEASE_SECS)
        .fetch_optional(self.pool())
        .await?;
        Ok(bound.is_some())
    }

    /// Extends the operation's lease while a worker still owns it — the
    /// streaming heartbeat. CAS on the owner: `false` means the lease was
    /// lost — the sweeper claimed the row, or the operation went terminal —
    /// and the heartbeat loop should stop.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn heartbeat(
        &self,
        operation_id: Uuid,
        lease_owner: &str,
        extend_secs: i64,
    ) -> Result<bool, WalletError> {
        let kept = sqlx::query_scalar::<_, uuid::Uuid>(
            "UPDATE oxsum.request_operations \
             SET leased_until = now() + ($3 || ' seconds')::interval, updated_at = now() \
             WHERE operation_id = $1 AND lease_owner = $2 \
               AND state IN ('leased', 'running') \
             RETURNING operation_id",
        )
        .bind(operation_id)
        .bind(lease_owner)
        .bind(extend_secs)
        .fetch_optional(self.pool())
        .await?;
        Ok(kept.is_some())
    }

    /// `leased` → `running`: the first upstream byte, or the task's first
    /// `running` poll. CAS on the owner — a worker that lost its lease does
    /// not advance a row it no longer owns.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn note_running(
        &self,
        operation_id: Uuid,
        lease_owner: &str,
    ) -> Result<bool, WalletError> {
        let ran = sqlx::query_scalar::<_, uuid::Uuid>(
            "UPDATE oxsum.request_operations \
             SET state = 'running', updated_at = now() \
             WHERE operation_id = $1 AND lease_owner = $2 AND state = 'leased' \
             RETURNING operation_id",
        )
        .bind(operation_id)
        .bind(lease_owner)
        .fetch_optional(self.pool())
        .await?;
        Ok(ran.is_some())
    }

    /// Writes one attempt row: the route tried, the credential picked, and
    /// upstream's own id when it gave one back. Status starts `started`; a
    /// refusal or unreachable completes the row through
    /// [`Db::complete_attempt`]. Returns the attempt's id, which the winning
    /// attempt's completion names.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_attempt(
        &self,
        operation_id: Uuid,
        channel: &str,
        credential_id: Option<Uuid>,
        external_id: Option<&str>,
    ) -> Result<Uuid, WalletError> {
        sqlx::query_scalar::<_, uuid::Uuid>(
            "INSERT INTO oxsum.request_attempts \
             (operation_id, channel, credential_id, external_id) \
             VALUES ($1, $2, $3, $4) \
             RETURNING attempt_id",
        )
        .bind(operation_id)
        .bind(channel)
        .bind(credential_id)
        .bind(external_id)
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    /// Completes an attempt: the terminal status, the usage it reported when
    /// it answered, or the refusal's message when it did not. The winning
    /// attempt's `usage_json` is what the settle prices from; a refused
    /// attempt's `error` is the failover audit.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn complete_attempt(
        &self,
        attempt_id: Uuid,
        status: AttemptStatus,
        usage_json: Option<&serde_json::Value>,
        error: Option<&str>,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.request_attempts \
             SET status = $2, usage_json = $3, error = left($4, 500), completed_at = now() \
             WHERE attempt_id = $1",
        )
        .bind(attempt_id)
        .bind(status.as_str())
        .bind(usage_json)
        .bind(error)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// [`complete_attempt`](Self::complete_attempt) inside the caller's
    /// transaction (issue #602): the attempt's terminal row commits with the
    /// settlement's ledger entry and the operation's close, so a settle that
    /// rolls back leaves no half-written usage evidence.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn complete_attempt_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        attempt_id: Uuid,
        status: AttemptStatus,
        usage_json: Option<&serde_json::Value>,
        error: Option<&str>,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.request_attempts \
             SET status = $2, usage_json = $3, error = left($4, 500), completed_at = now() \
             WHERE attempt_id = $1",
        )
        .bind(attempt_id)
        .bind(status.as_str())
        .bind(usage_json)
        .bind(error)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Stamps upstream's own request/task id onto an attempt row, once the
    /// body it answered names one — the chat completion's `id`, or the task
    /// relay's `external_task_id`. `WHERE external_id IS NULL` keeps the
    /// first stamp: a patch-up field never overwrites.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn note_attempt_external(
        &self,
        attempt_id: Uuid,
        external_id: &str,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.request_attempts SET external_id = $2 \
             WHERE attempt_id = $1 AND external_id IS NULL",
        )
        .bind(attempt_id)
        .bind(external_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// One operation's attempts, oldest first — the failover audit the detail
    /// read serves.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn operation_attempts(
        &self,
        operation_id: Uuid,
    ) -> Result<Vec<OperationAttempt>, WalletError> {
        let rows = sqlx::query(
            "SELECT attempt_id, operation_id, channel, credential_id, external_id, \
                    status, usage_json, error, started_at, completed_at \
             FROM oxsum.request_attempts \
             WHERE operation_id = $1 ORDER BY started_at",
        )
        .bind(operation_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(attempt_from_row).collect()
    }

    /// The terminal CAS: `UPDATE … SET state = $3, charged_minor = $4,
    /// outcome_kind = $5, completed_at = now()` where the row is still open
    /// and either this caller still owns the lease or the lease is open to
    /// claim. `AlreadySettled` is the raced double-settle — `Drop`-settles-once
    /// and a replayed settle lose here, which is honest, not an error.
    /// `LeaseLost` means a live worker or a sweeper claimed the row first:
    /// the operation's terminal write belongs to whoever holds it now.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settle_operation(
        &self,
        operation_id: Uuid,
        lease_owner: &str,
        outcome_state: OperationState,
        charged_minor: i64,
        outcome_kind: &str,
    ) -> Result<OperationSettle, WalletError> {
        debug_assert!(outcome_state.is_terminal());
        let touched = sqlx::query_scalar::<_, uuid::Uuid>(
            "UPDATE oxsum.request_operations \
             SET state = $3, charged_minor = $4, outcome_kind = $5, \
                 completed_at = now(), updated_at = now() \
             WHERE operation_id = $1 \
               AND (lease_owner = $2 OR state IN ('created','leased','running')) \
               AND state NOT IN ('succeeded','failed','cancelled','timed_out') \
             RETURNING operation_id",
        )
        .bind(operation_id)
        .bind(lease_owner)
        .bind(outcome_state.as_str())
        .bind(charged_minor)
        .bind(outcome_kind)
        .fetch_optional(self.pool())
        .await?;
        if touched.is_some() {
            return Ok(OperationSettle::Settled);
        }
        // Distinguish "already terminal" from "open but someone else's": the
        // first is a raced double-settle, the second a lost lease.
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state FROM oxsum.request_operations WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_optional(self.pool())
        .await?;
        match state.as_deref().map(OperationState::parse) {
            Some(Ok(state)) if state.is_terminal() => Ok(OperationSettle::AlreadySettled),
            Some(Ok(_)) => Ok(OperationSettle::LeaseLost),
            // A missing row or an unreadable state: the settle raced a delete
            // that cannot happen under RESTRICT, or a state this build does
            // not know — both read as lost rather than settled.
            _ => Ok(OperationSettle::LeaseLost),
        }
    }

    /// [`settle_operation`](Self::settle_operation) inside the caller's
    /// transaction (issue #602, D05): the operation's terminal row commits with
    /// the settlement's ledger entry, so the two can never disagree — a settle
    /// whose transaction rolls back leaves the operation open for the sweeper,
    /// not half-closed over money that never moved.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settle_operation_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        operation_id: Uuid,
        lease_owner: &str,
        outcome_state: OperationState,
        charged_minor: i64,
        outcome_kind: &str,
    ) -> Result<OperationSettle, WalletError> {
        debug_assert!(outcome_state.is_terminal());
        let touched = sqlx::query_scalar::<_, uuid::Uuid>(
            "UPDATE oxsum.request_operations \
             SET state = $3, charged_minor = $4, outcome_kind = $5, \
                 completed_at = now(), updated_at = now() \
             WHERE operation_id = $1 \
               AND (lease_owner = $2 OR state IN ('created','leased','running')) \
               AND state NOT IN ('succeeded','failed','cancelled','timed_out') \
             RETURNING operation_id",
        )
        .bind(operation_id)
        .bind(lease_owner)
        .bind(outcome_state.as_str())
        .bind(charged_minor)
        .bind(outcome_kind)
        .fetch_optional(&mut **tx)
        .await?;
        if touched.is_some() {
            return Ok(OperationSettle::Settled);
        }
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state FROM oxsum.request_operations WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_optional(&mut **tx)
        .await?;
        match state.as_deref().map(OperationState::parse) {
            Some(Ok(state)) if state.is_terminal() => Ok(OperationSettle::AlreadySettled),
            Some(Ok(_)) => Ok(OperationSettle::LeaseLost),
            _ => Ok(OperationSettle::LeaseLost),
        }
    }

    /// Whether the organization still has open operations created at or
    /// before `before` — the watermark a day-close waits on (issue #603, D05):
    /// an operation still `created`/`leased`/`running` can still settle a day
    /// leaf, so the anchor pass defers the organization's day rather than seal
    /// a map the settle path may still change.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn operations_open_before(
        &self,
        organization_id: Uuid,
        before: OffsetDateTime,
    ) -> Result<i64, WalletError> {
        sqlx::query_scalar(
            "SELECT count(*)::bigint FROM oxsum.request_operations \
             WHERE organization_id = $1 AND created_at <= $2 \
               AND state IN ('created','leased','running')",
        )
        .bind(organization_id)
        .bind(before)
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    /// The sweeper's working set: open operations whose lease lapsed, plus a
    /// `created` row whose hold never bound and whose admission is older than
    /// the lease — the window where the process died between writing the
    /// operation and binding the hold. Bounded per pass so a backlog drains
    /// over ticks.
    ///
    /// `metering` operations are excluded (issue #604): a metered event binds
    /// no heartbeating worker, so the 60-second gateway lease would claim a
    /// hold that is legitimately open while the reporting service holds it.
    /// Its lifecycle rides the `open_holds` sweep instead — the same watch row
    /// its bound `hold_key` already names — and `sweep_one` closes the
    /// operation when it releases the stale metered hold.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn operations_past_lease(
        &self,
        limit: i64,
    ) -> Result<Vec<RequestOperation>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {OPERATION_COLS} FROM oxsum.request_operations \
             WHERE surface <> $3 \
               AND ((state IN ('leased', 'running') AND leased_until < now()) \
                OR (state = 'created' AND created_at < now() - ($1 || ' seconds')::interval)) \
             ORDER BY created_at LIMIT $2"
        ))
        .bind(LEASE_SECS)
        .bind(limit)
        .bind(METERING_SURFACE)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(operation_from_row).collect()
    }

    /// Claims a lapsed operation for the sweeper: `false` is the CAS race's
    /// loser — another worker claimed the row, or it went terminal between
    /// the listing and this update. The claimed row re-leases under
    /// `new_owner` so the next pass does not re-list it while this sweep is
    /// still working it.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn claim_operation(
        &self,
        operation_id: Uuid,
        new_owner: &str,
        lease_secs: i64,
    ) -> Result<bool, WalletError> {
        let claimed = sqlx::query_scalar::<_, uuid::Uuid>(
            "UPDATE oxsum.request_operations \
             SET lease_owner = $2, \
                 leased_until = now() + ($3 || ' seconds')::interval, \
                 state = CASE WHEN state = 'created' THEN 'created' ELSE 'leased' END, \
                 updated_at = now() \
             WHERE operation_id = $1 \
               AND state IN ('created', 'leased', 'running') \
               AND (leased_until < now() OR leased_until IS NULL) \
             RETURNING operation_id",
        )
        .bind(operation_id)
        .bind(new_owner)
        .bind(lease_secs)
        .fetch_optional(self.pool())
        .await?;
        Ok(claimed.is_some())
    }
}
