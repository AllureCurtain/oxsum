//! The async task relay's core half (issue #503): one row per submitted task,
//! the billing-side record of a long-running upstream job.
//!
//! A task is a turn on a minutes timescale: `POST /v1/tasks` freezes an upper
//! bound and relays `{baseUrl}/tasks`, a `poll-tasks` periodic pass asks
//! `{baseUrl}/tasks/{id}` on the caller's behalf, and the terminal answer
//! settles the hold — `succeeded` against the result's usage, `failed` and
//! `cancelled` at zero — and enqueues `task.succeeded`/`task.failed` to the
//! organization's subscribed webhook endpoints. The ledger side needs nothing
//! new: the hold is an ordinary `req-<task id>:hold` the same sweeper watches.
//!
//! The caller's `GET` is a row read, never a second upstream call — upstream
//! is asked only inside the poll pass, so a hundred clients polling one task
//! cost one upstream question each tick.

use sqlx::Row;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::billing::{MeteredSettlement, SettlementKind};
use crate::channels::price_row;
use crate::db::Db;
use crate::error::WalletError;
use crate::tenants::Tenants;
use crate::usage::{CountsProvenance, UsageRecord, UsageRow};
use crate::wallet::{entry_id_for, settlement_key_for};

/// A task's billing class — what the submission declared. `custom` covers
/// every kind the enum does not name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// An image render or edit.
    Image,
    /// A video generation.
    Video,
    /// A batch of upstream work.
    Batch,
    /// Anything else — the billing rules name their own dimensions.
    Custom,
}

impl TaskKind {
    /// The spelling `tasks.kind` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Batch => "batch",
            Self::Custom => "custom",
        }
    }

    /// Reads a stored kind back; an unknown one is a row this build cannot
    /// interpret and is refused rather than guessed.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] on an unknown kind.
    pub fn parse(stored: &str) -> Result<Self, WalletError> {
        Ok(match stored {
            "image" => Self::Image,
            "video" => Self::Video,
            "batch" => Self::Batch,
            "custom" => Self::Custom,
            other => {
                return Err(WalletError::InvalidInput(format!(
                    "unknown task kind {other:?}"
                )));
            }
        })
    }
}

/// Where a task is in its life — `submitted` and `running` are open,
/// everything else is terminal and means the hold already settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Written but not yet asked upstream (the submit relay failed after the
    /// row, or the first poll has not run).
    Submitted,
    /// Upstream accepted the job and reports it mid-flight.
    Running,
    /// Terminal: upstream reported success and the result's usage settled.
    Succeeded,
    /// Terminal: upstream reported failure; the hold released at zero.
    Failed,
    /// Terminal: the caller cancelled, or upstream did; the hold released.
    Cancelled,
}

impl TaskState {
    /// The spelling `tasks.state` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Reads a stored state back.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] on an unknown state.
    pub fn parse(stored: &str) -> Result<Self, WalletError> {
        Ok(match stored {
            "submitted" => Self::Submitted,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => {
                return Err(WalletError::InvalidInput(format!(
                    "unknown task state {other:?}"
                )));
            }
        })
    }

    /// Whether the state is terminal — settled, no more polls.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// One task row as the relay keeps it.
#[derive(Debug, Clone)]
pub struct Task {
    /// oxsum's own id — the client polls and cancels by it.
    pub task_id: Uuid,
    /// The organization that submitted, for the poll's ownership check.
    pub organization_id: Uuid,
    /// The key that submitted — the usage record's payer.
    pub key_id: Option<Uuid>,
    /// The channel that accepted the submission.
    pub channel_id: Uuid,
    /// The channel's name — what the settlement and the upstream URL name.
    pub channel: String,
    /// Upstream's own id for the job.
    pub external_task_id: String,
    pub kind: TaskKind,
    pub state: TaskState,
    /// The hold's idempotency key — `req-<task id>:hold` — the settle derives from.
    pub hold_key: String,
    /// What the submission froze, in minor units.
    pub freeze_minor: i64,
    /// What the terminal settle charged; `None` while the task is open.
    pub charged_minor: Option<i64>,
    /// The public model name the task priced under.
    pub model: String,
    /// Upstream's answer body on success.
    pub result_json: Option<serde_json::Value>,
    /// The reported failure on `failed`/`cancelled`.
    pub error_json: Option<serde_json::Value>,
    /// The usage report the terminal answer carried.
    pub usage_json: Option<serde_json::Value>,
    pub submitted_at: OffsetDateTime,
    /// When the state went terminal.
    pub completed_at: Option<OffsetDateTime>,
}

/// A task's submission input, as the route hands it down.
#[derive(Debug)]
pub struct NewTask {
    /// oxsum's task id — minted by the route so the hold key derives before
    /// the row exists.
    pub task_id: Uuid,
    pub organization_id: Uuid,
    pub key_id: Option<Uuid>,
    pub channel_id: Uuid,
    /// The channel's name, denormalized onto the row so a settle never
    /// re-asks the directory.
    pub channel: String,
    /// Upstream's own id for the job.
    pub external_task_id: String,
    pub kind: TaskKind,
    /// `req-<task id>:hold`.
    pub hold_key: String,
    /// What the submission froze, in minor units.
    pub freeze_minor: i64,
    /// The public model name the task priced under.
    pub model: String,
}

const TASK_COLS: &str = "t.task_id, t.organization_id, t.key_id, t.channel_id, \
     c.name AS channel, t.external_task_id, t.kind, t.state, t.hold_key, \
     t.freeze_minor, t.charged_minor, t.model, t.result_json, t.error_json, \
     t.usage_json, t.submitted_at, t.completed_at";

const TASK_JOIN: &str = "FROM oxsum.tasks t JOIN oxsum.channels c USING (channel_id)";

fn task_from_row(row: &sqlx::postgres::PgRow) -> Result<Task, WalletError> {
    Ok(Task {
        task_id: row.try_get("task_id")?,
        organization_id: row.try_get("organization_id")?,
        key_id: row.try_get("key_id")?,
        channel_id: row.try_get("channel_id")?,
        channel: row.try_get("channel")?,
        external_task_id: row.try_get("external_task_id")?,
        kind: TaskKind::parse(&row.try_get::<String, _>("kind")?)?,
        state: TaskState::parse(&row.try_get::<String, _>("state")?)?,
        hold_key: row.try_get("hold_key")?,
        freeze_minor: row.try_get("freeze_minor")?,
        charged_minor: row.try_get("charged_minor")?,
        model: row.try_get("model")?,
        result_json: row.try_get("result_json")?,
        error_json: row.try_get("error_json")?,
        usage_json: row.try_get("usage_json")?,
        submitted_at: row.try_get("submitted_at")?,
        completed_at: row.try_get("completed_at")?,
    })
}

/// What the poll pass reads to reach the channel: the channel's base URL and
/// the pool's lead credential join the task row, so the pass makes no second
/// directory trip per task.
pub struct TaskPollTarget {
    /// The task row.
    pub task: Task,
    /// The channel's upstream base.
    pub base_url: String,
    /// The sealed upstream credential for the poll's `Authorization` — `None`
    /// when the channel's pool is empty or every credential is cooled.
    pub api_key_sealed: Option<String>,
}

/// The `task.succeeded` webhook event type.
pub const TASK_SUCCEEDED: &str = "task.succeeded";
/// The `task.failed` webhook event type.
pub const TASK_FAILED: &str = "task.failed";

impl Db {
    /// Writes the task row a submission creates — after the hold landed and
    /// upstream answered with `external_task_id`. The row goes in `submitted`;
    /// the first poll moves it.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn insert_task(&self, task: &NewTask) -> Result<Task, WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.tasks \
             (task_id, organization_id, key_id, channel_id, external_task_id, \
              kind, state, hold_key, freeze_minor, model) \
             VALUES ($1, $2, $3, $4, $5, $6, 'submitted', $7, $8, $9)",
        )
        .bind(task.task_id)
        .bind(task.organization_id)
        .bind(task.key_id)
        .bind(task.channel_id)
        .bind(&task.external_task_id)
        .bind(task.kind.as_str())
        .bind(&task.hold_key)
        .bind(task.freeze_minor)
        .bind(&task.model)
        .execute(self.pool())
        .await?;
        self.task(task.organization_id, task.task_id)
            .await?
            .ok_or_else(|| WalletError::InvalidInput("the task row was not written".into()))
    }

    /// Reads one task — the caller's poll's row, owned by `organization_id`.
    /// `None` covers both absent and foreign: a task's existence is not an
    /// oracle for another organization's keys.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn task(
        &self,
        organization_id: Uuid,
        task_id: Uuid,
    ) -> Result<Option<Task>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {TASK_COLS} {TASK_JOIN} \
             WHERE t.task_id = $1 AND t.organization_id = $2"
        ))
        .bind(task_id)
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| task_from_row(&row)).transpose()
    }

    /// The still-open tasks the poll pass asks upstream about, with the
    /// channel reach they need — base URL and the pool's lead sealed
    /// credential joined, so the pass makes no second trip per task.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn open_task_poll_targets(
        &self,
        limit: i64,
    ) -> Result<Vec<TaskPollTarget>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLS}, c.base_url, \
                    (SELECT cr.api_key_sealed FROM oxsum.channel_credentials cr \
                     WHERE cr.channel_id = t.channel_id AND cr.status = 'active' \
                     ORDER BY cr.position LIMIT 1) AS api_key_sealed \
             {TASK_JOIN} \
             WHERE t.state IN ('submitted', 'running') \
             ORDER BY t.submitted_at LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        let mut targets = Vec::with_capacity(rows.len());
        for row in &rows {
            targets.push(TaskPollTarget {
                task: task_from_row(row)?,
                base_url: row.try_get("base_url")?,
                api_key_sealed: row.try_get("api_key_sealed")?,
            });
        }
        Ok(targets)
    }

    /// The channel's upstream base URL — read at use time so a poll or cancel
    /// never carries a stale directory entry the row was written with.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn channel_base(&self, channel_id: Uuid) -> Result<String, WalletError> {
        sqlx::query_scalar("SELECT base_url FROM oxsum.channels WHERE channel_id = $1")
            .bind(channel_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| WalletError::InvalidInput("no such channel".into()))
    }

    /// One healthy credential for the task's channel, opened under the
    /// deployment secret — the poll and the cancel share the pick: the pool's
    /// first active credential in position order.
    ///
    /// # Errors
    ///
    /// [`WalletError::Misconfigured`] when the sealed credential cannot open;
    /// storage failures surface as [`WalletError`].
    pub async fn task_channel_key(
        &self,
        channel_id: Uuid,
        secret: &crate::channels::SecretKey,
    ) -> Result<Option<String>, WalletError> {
        let sealed: Option<String> = sqlx::query_scalar(
            "SELECT api_key_sealed FROM oxsum.channel_credentials \
             WHERE channel_id = $1 AND status = 'active' \
             ORDER BY position LIMIT 1",
        )
        .bind(channel_id)
        .fetch_optional(self.pool())
        .await?
        .flatten();
        sealed.map(|sealed| secret.open(&sealed)).transpose()
    }

    /// The tenant a hold's watch row names — the task-submit failure path's
    /// way back to the wallet a hold was taken on, when the caller's own
    /// organization is not the read it has.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn tenant_of_hold(&self, hold_key: &str) -> Result<Option<String>, WalletError> {
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.open_holds WHERE hold_key = $1")
            .bind(hold_key)
            .fetch_optional(self.pool())
            .await
            .map_err(WalletError::from)
    }

    /// The poll pass's write for a non-terminal state: `submitted` →
    /// `running`. A terminal state is never written through here —
    /// [`settle_task`] owns the transition that bills.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn note_task_running(&self, task_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.tasks SET state = 'running' \
             WHERE task_id = $1 AND state = 'submitted'",
        )
        .bind(task_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

/// What a terminal settle produces — the charge it recorded and the entry it
/// wrote, so the caller can report them.
pub struct TaskSettlement {
    /// The settled charge in minor units — zero on `failed`/`cancelled`.
    pub charged_minor: i64,
    /// The settlement's kind.
    pub kind: SettlementKind,
    /// The entry the settle wrote — the bill the charge lands on.
    pub settlement_id: Uuid,
}

/// What the terminal answer carried — everything `settle_task` needs beyond
/// the task row itself.
pub struct TaskOutcome {
    /// The terminal state upstream (or a cancel) reported.
    pub state: TaskState,
    /// The normalized usage the terminal answer reported, `None` when it did not.
    pub usage: Option<UsageRecord>,
    /// Upstream's answer body on success.
    pub result_json: Option<serde_json::Value>,
    /// The reported failure on `failed`/`cancelled`.
    pub error_json: Option<serde_json::Value>,
    /// The `usage` object verbatim, for the row's record.
    pub usage_json: Option<serde_json::Value>,
}

/// Settles a task's hold on its terminal state: `succeeded` prices the
/// result's usage against the channel's price (capped at the freeze when the
/// report overshoots it), `failed`/`cancelled` release at zero. The ledger
/// settle commits first — the settlement's idempotency key derives from the
/// hold, so a raced second settle conflicts rather than double-charges — then
/// the row's terminal update, the usage record and the webhook land in one
/// transaction, and the watch row clears last so the sweeper never reaps a
/// settled hold.
///
/// `usage` is the terminal answer's `usage` field already normalized, `None`
/// when upstream reported none — a reportless success prices a zero-count
/// record against the price's `cost_per_request`, the task-shaped equivalent
/// of a reportless turn's estimate.
///
/// # Errors
///
/// [`WalletError::Conflict`] when the hold was settled by another writer —
/// the sweeper, or a raced cancel; [`WalletError::InvalidInput`] when the
/// channel's price row is missing; storage failures surface as
/// [`WalletError`].
pub async fn settle_task(
    db: &Db,
    tenants: &Tenants,
    task: &Task,
    organization_id: Uuid,
    outcome: &TaskOutcome,
    on: Date,
) -> Result<TaskSettlement, WalletError> {
    debug_assert!(outcome.state.is_terminal());
    let tenant_id = crate::orgs::tenant_id_for(organization_id);
    let wallet = tenants.get(&tenant_id).await?;

    // The bill is priced at the version the hold froze under — the version
    // the watch row recorded at submit — not whatever is latest at settle
    // time: a task can run minutes past a price change and still owes the
    // rate it was quoted. The watch row is gone only when the sweeper already
    // resolved the hold, in which case this settle conflicts below anyway,
    // so a missing row falls back to latest rather than refusing the row's
    // terminal write.
    let pinned = db.open_hold(&task.hold_key).await?.map(|h| h.price_version);
    let priced = price_row(db.pool(), &task.channel, &task.model, pinned)
        .await?
        .ok_or_else(|| WalletError::InvalidInput(format!("no price for {}", task.model)))?;

    let discount_percent = db
        .discount_percent(organization_id, &task.model)
        .await?
        .map(i64::from);

    // The charge and the kind: succeeded bills the usage (a reportless
    // success prices a zero-count record, so `costPerRequest`-only prices
    // still charge their flat rate); failed/cancelled charge nothing — the
    // hold releases whole.
    let (kind, charged, settled_usage, itemized_lines, itemized_rule) = match outcome.state {
        TaskState::Succeeded => {
            let record = outcome.usage.clone().unwrap_or_default();
            let itemized = priced.price.itemize(&record, true)?;
            let cost = match discount_percent {
                Some(percent) => itemized.discounted_minor(percent)?,
                None => itemized.total_minor()?,
            };
            if cost > task.freeze_minor {
                (
                    SettlementKind::Capped,
                    task.freeze_minor,
                    record,
                    itemized.lines,
                    itemized.matched_rule,
                )
            } else {
                (
                    SettlementKind::Usage,
                    cost,
                    record,
                    itemized.lines,
                    itemized.matched_rule,
                )
            }
        }
        TaskState::Failed | TaskState::Cancelled => (
            SettlementKind::Released,
            0,
            UsageRecord::default(),
            Vec::new(),
            None,
        ),
        _ => unreachable!("settle_task is terminal-only"),
    };

    let request_id = task.task_id.to_string();
    let description = MeteredSettlement {
        request: &request_id,
        service: "tasks",
        channel: &task.channel,
        model: &task.model,
        price_version: priced.version,
        kind,
        usage: &settled_usage,
        lines: &itemized_lines,
        matched_rule: itemized_rule.as_ref(),
        discount_percent,
        charged,
        freeze: task.freeze_minor,
    }
    .description()?;

    match wallet
        .settle(&task.hold_key, &description, charged, on)
        .await
    {
        Ok(_) => {}
        Err(WalletError::Conflict(_)) | Err(WalletError::HoldNotFound(_)) => {
            // Settled already — the sweeper or a raced cancel. The task row
            // still takes its terminal state; the charge is whatever the
            // committed settlement recorded.
            return Err(WalletError::Conflict("task hold already settled".into()));
        }
        Err(error) => return Err(error),
    }

    let settlement_id = *entry_id_for(&settlement_key_for(&task.hold_key)).as_uuid();
    let available = wallet.available().await.ok();

    let row = UsageRow {
        request_id: request_id.clone(),
        tenant_id: tenant_id.clone(),
        key_id: task.key_id,
        mandate_id: None,
        model: task.model.clone(),
        channel: task.channel.clone(),
        price_version: priced.version,
        kind,
        entry_id: settlement_id,
        status: "settled".to_owned(),
        failure_reason: outcome
            .error_json
            .as_ref()
            .and_then(|e| e.get("message").and_then(|m| m.as_str()).map(str::to_owned)),
        usage: settled_usage.clone(),
        counts: match outcome.state {
            TaskState::Succeeded => CountsProvenance::Provider {
                input: settled_usage.input_tokens,
                output: settled_usage.output_tokens,
            },
            _ => CountsProvenance::Local {
                input: 0,
                output: Some(0),
            },
        },
        charged_minor: charged,
        freeze_minor: task.freeze_minor,
        upstream_cost_minor: None,
        upstream_attempts: 0,
    };
    // The ledger settle is committed; the usage row, the task's terminal
    // update and the webhook write land in one transaction so they agree or
    // roll back together. The watch row clears after, not inside — a crash
    // between them leaves the sweeper a settled hold to release, which the
    // ledger's derived key makes a no-op (issue #503).
    let mut tx = db.pool().begin().await?;
    sqlx::query(
        "UPDATE oxsum.tasks \
         SET state = $2, charged_minor = $3, result_json = $4, error_json = $5, \
             usage_json = $6, completed_at = now() \
         WHERE task_id = $1",
    )
    .bind(task.task_id)
    .bind(outcome.state.as_str())
    .bind(charged)
    .bind(&outcome.result_json)
    .bind(&outcome.error_json)
    .bind(&outcome.usage_json)
    .execute(&mut *tx)
    .await?;
    db.record_usage_in(&mut tx, &row, available).await?;
    if let Some(event) = match outcome.state {
        TaskState::Succeeded => Some(TASK_SUCCEEDED),
        TaskState::Failed => Some(TASK_FAILED),
        _ => None,
    } {
        Db::enqueue_event(
            &mut tx,
            organization_id,
            event,
            serde_json::json!({
                "taskId": task.task_id,
                "kind": task.kind.as_str(),
                "model": task.model,
                "chargedMinor": charged,
            }),
        )
        .await?;
    }
    tx.commit().await?;
    db.clear_open_hold(&task.hold_key).await?;

    Ok(TaskSettlement {
        charged_minor: charged,
        kind,
        settlement_id,
    })
}
