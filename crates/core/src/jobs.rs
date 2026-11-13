//! The periodic-work table (issue #166, roadmap P8-1).
//!
//! `oxsum.jobs` holds one row per scheduled run of a periodic kind. The worker
//! in the binary claims due rows with `FOR UPDATE SKIP LOCKED` — the same shape
//! `webhook_deliveries`' queue already proves — dispatches by kind, and marks the
//! run `done` beside the next occurrence's row, so the chain survives a restart:
//! a process that dies between finishing a run and enqueueing the next is healed
//! by startup, which enqueues whatever kind has nothing due.
//!
//! A run that fails retries on a backoff until the attempt budget runs out, then
//! lands `dead` — the reconciliation report's `jobs_dead` class is the operator
//! surface for it, and the worker re-enqueues the kind after a cooldown rather
//! than leaving a transient failure to hold a periodic chain forever.
//!
//! `webhook_deliveries` deliberately stays its own queue: it is a user-visible
//! surface with per-endpoint state, not an internal job.

use std::str::FromStr;

use doubleentry::storage::postgres::PostgresError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use time::{Date, Duration, OffsetDateTime};
use uuid::Uuid;

use crate::error::WalletError;

use crate::{Db, StatementPeriod};

/// The periodic kinds, as `oxsum.jobs.kind` spells them.
pub mod kinds {
    /// The stale-hold pass `sweep_stale_holds` ran on an in-process interval.
    pub const SWEEP_HOLDS: &str = "sweep-holds";
    /// One `deliver_due` pass over the webhook queue.
    pub const DELIVER_WEBHOOKS: &str = "deliver-webhooks";
    /// The scheduled `Db::reconcile` scan — drift the on-demand endpoint also reads.
    pub const RECONCILE: &str = "reconcile";
    /// Promptly recover accepted merchant events and fulfill paid orders.
    pub const PAYMENT_FULFILLMENT: &str = "payment-fulfillment";
    /// The month that just ended: `close_month`, its draft statements, and the
    /// overdue flip — the period's aggregation job.
    pub const STATEMENTS: &str = "statements";
    /// The retention spec's batched cleanup.
    pub const RETENTION: &str = "retention";
    /// The upstream price-catalog probe and drift diff (issue #222).
    pub const SYNC_UPSTREAM_PRICES: &str = "sync-upstream-prices";
    /// The daily provider-bill reconciliation (issue #283): pull each channel's
    /// billed cost for yesterday, record it against the day's Σ
    /// `upstream_cost_minor`, and flag the drift over the alert threshold.
    pub const UPSTREAM_BILL_SYNC: &str = "upstream-bill-sync";
    /// The external witness anchoring pass (issue #231): publish each
    /// organization's latest signed head to the configured witnesses and store
    /// the cosigned notes they answer.
    pub const WITNESS_CHECKPOINTS: &str = "witness-checkpoints";
    /// The daily usage-anchor pass (issue #338): commit yesterday's sorted-map
    /// root over every customer's day-log head into each organization's ledger.
    pub const USAGE_ANCHORS: &str = "usage-anchors";
    /// The subscription rollover pass (issue #496): every active subscription
    /// whose period ended is rolled forward, and a lapsed never-reset plan is
    /// `expired`. Leader-required — a second runner would roll the same period
    /// twice inside one organization.
    pub const SUBSCRIPTION_ROLLOVERS: &str = "subscription-rollovers";
    /// Recognize completed UTC service days for versioned prepaid plans.
    pub const PLAN_RECOGNITION: &str = "plan-recognition";
    /// The task-relay's upstream status pass (issue #503): ask each open
    /// task's channel for the job's state and settle the hold on the terminal
    /// answer. Unleadered — the pass is per-task, and two workers polling the
    /// same task race on the settlement's derived key, which only one can win.
    pub const POLL_TASKS: &str = "poll-tasks";
    /// The promo-lot expiry pass (issue #609): every open lot whose
    /// `expires_at` passed burns its unreserved remainder to
    /// `Equity:PromoExpiry` and closes `expired`. Leader-required — a second
    /// runner would walk the same due list at once, and although the per-lot
    /// `FOR UPDATE` makes the overlap harmless, one pass is the honest
    /// cadence.
    pub const EXPIRE_LOTS: &str = "expire-lots";
    /// The refund retry pass (issue #608): re-issue `provider_pending`
    /// refunds whose `next_retry_at` is due, releasing the reservation when
    /// the attempt budget runs out. Unleadered — the refund row's own lock
    /// and the provider idempotency key (the refund id) serialize two
    /// workers on one refund, so a second pass is a wasted call, never a
    /// second refund.
    pub const REFUND_RETRIES: &str = "refund-retries";
    /// The plan-statement overdue sweep (issue #612, T21): flips `issued`
    /// plan statements whose `due_at` passed to `past_due`, and past the
    /// plan's grace stamps `dunning_suspended_at` so wallet-side admission
    /// refuses. Leader-required — a second runner would walk the same due
    /// list at once.
    pub const STATEMENT_PAST_DUE: &str = "statement-past-due";
    /// The financial-facts reconciliation pass (issue #613, T22): diffs the
    /// eight categories per organization against the journals, statements and
    /// orders over the period that just closed, writes a `reconciliation_runs`
    /// row and one `reconciliation_exceptions` per disagreed fact.
    /// Leader-required — a second runner would re-open the same exceptions
    /// under a second run id.
    pub const RECONCILE_FINANCIAL: &str = "reconcile-financial";
    /// A manual month-close the operator queued through the admin surface
    /// (`POST /admin/closings`). The payload carries `{month}`; the pass seals
    /// that month across every organization's ledger off the request path, so
    /// a large deployment's close is a claimed run, not a request that outlives
    /// its timeout. The run writes its progress to the row's `result` as it
    /// walks, which `GET /admin/closings/jobs/{jobId}` answers (issue #1090).
    /// The kind holds one pending or running run at a time, whatever its
    /// month. Leader-required — a second runner would seal the same
    /// organizations at once.
    pub const CLOSE_MONTH: &str = "close-month";
    /// The x402 credit-recovery pass (issue #1089): retries the wallet credit
    /// for deposits whose chain transfer committed but whose `top_up` failed
    /// (`meta.creditError`). Unleadered — each row is claimed `FOR UPDATE` and
    /// the retry replays under the same `x402:<nonce>` key, so a second
    /// runner's attempt is a replay, never a second credit.
    pub const RECOVER_DEPOSIT_CREDITS: &str = "recover-deposit-credits";
    /// The Solana deposit-poll pass (issue #1117): walks the rail's `pending`
    /// intents, runs the Solana Pay findReference + validateTransfer check
    /// against the chain, credits the confirmed ones and expires the lapsed
    /// ones. Unleadered — each deposit is claimed `FOR UPDATE` and the credit
    /// replays under `solana:<signature>`, so two workers racing one intent
    /// produce a wasted RPC call, never a second credit.
    pub const SOLANA_DEPOSITS: &str = "solana-deposits";
}

/// How long a claimed run may take before another worker's claim assumes the
/// first died: every handler is idempotent, so a wrongly-taken-over run is a
/// duplicated pass, not a correctness hazard.
pub const CLAIM_LEASE_SECS: i64 = 300;
/// Attempts a run gets before it lands `dead`.
pub const MAX_ATTEMPTS: i32 = 5;
/// The retry delays after attempts 1 through 5; past the table the run is dead.
const RETRY_DELAYS_SECS: [i64; 5] = [30, 120, 300, 900, 3600];
/// How far out a dead periodic run's kind is re-enqueued: an hour of quiet
/// rather than either an instant retry loop or a permanent stop.
const DEAD_RESCHEDULE_SECS: i64 = 3600;
/// Rows each retention pass takes per rule — a backlog drains over passes.
const RETENTION_BATCH: i64 = 10_000;

/// One claimed run, as the worker reads it.
#[derive(Debug)]
pub struct Job {
    /// The run's identifier.
    pub job_id: Uuid,
    /// The periodic kind, as [`kinds`] spells it.
    pub kind: String,
    /// When the run is next due.
    pub run_at: OffsetDateTime,
    /// How many attempts the run has made so far.
    pub attempts: i32,
    /// The claim's stamp: finish and fail fence on it, so a worker whose lease
    /// lapsed cannot overwrite the next claimant's state.
    pub claimed_at: OffsetDateTime,
    /// The kind-specific JSON the run carries, if any.
    pub payload: Value,
    /// Whether the kind runs under the advisory-lock leader election (issue
    /// #281): a worker claiming a leader-required run that cannot take the
    /// lock hands the row back instead of duplicating the pass.
    pub requires_leader: bool,
}

/// Whether a kind runs under the advisory-lock leader election — exactly one
/// instance may hold the lock at a time, so a pass that outlives its claim
/// lease (a `statements` pass over many organizations, a witness round-trip)
/// cannot be claimed and re-run by a second instance mid-flight. The sweep,
/// the month-end aggregation and the outbound passes qualify: duplicating any
/// of them is wasted work and a double-fired webhook. `deliver-webhooks`
/// stays unleadered — its rows are claimed `SKIP LOCKED` individually, so a
/// second runner is a throughput gain, not a duplicate.
pub fn requires_leader(kind: &str) -> bool {
    use kinds::*;
    matches!(
        kind,
        SWEEP_HOLDS
            | WITNESS_CHECKPOINTS
            | STATEMENTS
            | RETENTION
            | SYNC_UPSTREAM_PRICES
            | UPSTREAM_BILL_SYNC
            | USAGE_ANCHORS
            | SUBSCRIPTION_ROLLOVERS
            | PLAN_RECOGNITION
            | EXPIRE_LOTS
            | STATEMENT_PAST_DUE
            | RECONCILE
            | PAYMENT_FULFILLMENT
            | RECONCILE_FINANCIAL
            | CLOSE_MONTH
    )
}

/// The advisory-lock key namespace for leader election. `pg_try_advisory_lock`
/// takes a bigint; `hashtext` folds the name into an i32 Postgres widens, and
/// the `oxsum.job-leader:` prefix keeps the keys disjoint from every other
/// advisory lock the codebase takes (per-key hold locks, credit-limit locks,
/// the witness serve lock) — a collision would deadlock a periodic chain.
pub fn leader_lock_key(kind: &str) -> String {
    format!("oxsum.job-leader:{kind}")
}

/// What a failed run became — the worker logs and counts on it.
#[derive(Debug, PartialEq, Eq)]
pub enum JobOutcome {
    /// Back in `pending`, due again at the stamped time.
    Rescheduled(OffsetDateTime),
    /// Past the attempt budget; the row stays as the evidence a `jobs_dead`
    /// drift reads, and the kind is re-enqueued on the dead cooldown.
    Dead,
}

/// The retention windows the cleanup applies — configured durations, see
/// `OXSUM_RETENTION_*` in `.env.example`.
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    /// Days before `usage_details.provider_raw` is nulled — the raw upstream
    /// payload may carry prompt fragments, so this window is the privacy one.
    pub provider_raw_days: i64,
    /// Days a delivered or failed `webhook_deliveries` row is kept.
    pub deliveries_days: i64,
    /// Days a finished `jobs` row is kept.
    pub jobs_days: i64,
    /// Days a `request_log` row is kept (issue #505) — the operational record
    /// is a search surface, not a billing fact, so it ages out like the other
    /// projections rather than living forever.
    pub request_log_days: i64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            provider_raw_days: 7,
            deliveries_days: 30,
            jobs_days: 90,
            request_log_days: 30,
        }
    }
}

/// What one retention pass removed, per rule.
#[derive(Debug, Default, Clone, Copy)]
pub struct RetentionReport {
    /// `idempotency_records` rows past their own `expires_at`.
    pub idempotency_records: u64,
    /// `usage_records` rows whose `provider_raw` was nulled.
    pub provider_raw_nulled: u64,
    /// Terminal `webhook_deliveries` rows deleted.
    pub webhook_deliveries: u64,
    /// Finished `jobs` rows deleted.
    pub jobs: u64,
    /// Expired `request_log` rows deleted.
    pub request_log: u64,
}

/// How far out the next periodic occurrence of `kind` is due — the worker asks
/// for it after every finished run. An unknown kind gets no next run.
pub fn next_run(kind: &str) -> Option<Duration> {
    use kinds::*;
    Some(match kind {
        SWEEP_HOLDS => Duration::seconds(60),
        DELIVER_WEBHOOKS => Duration::seconds(30),
        POLL_TASKS => Duration::seconds(30),
        PAYMENT_FULFILLMENT => Duration::seconds(5),
        RECONCILE | STATEMENTS | RETENTION | SYNC_UPSTREAM_PRICES | UPSTREAM_BILL_SYNC
        | USAGE_ANCHORS => Duration::days(1),
        // Anchoring is hourly: often enough that a head ages at most an hour
        // before a witness vouches for it, rare enough that every pass is one
        // request per organization per witness.
        WITNESS_CHECKPOINTS => Duration::hours(1),
        // Hourly: often enough that an ended period closes even when no
        // request arrives, rare enough that every pass is one scan.
        SUBSCRIPTION_ROLLOVERS | PLAN_RECOGNITION => Duration::hours(1),
        // Every minute: an expiring lot should burn within a minute of its
        // `expires_at`, because a hold admitted just before the deadline may
        // still pin it — the pass is one indexed UPDATE per due lot, cheap.
        EXPIRE_LOTS => Duration::minutes(1),
        // The refund pass rides the row's own `next_retry_at`, so the poll
        // only bounds how late a due retry starts.
        REFUND_RETRIES => Duration::seconds(30),
        // Stuck x402 credits should heal promptly once whatever broke the
        // ledger credit clears; a no-op pass is one indexed SELECT.
        RECOVER_DEPOSIT_CREDITS => Duration::seconds(30),
        // A pending solana intent is payable for a bounded window; a ~15 s
        // cadence lands a just-confirmed transfer within a few seconds without
        // hammering the RPC.
        SOLANA_DEPOSITS => Duration::seconds(15),
        // The receivable marks past-due promptly once its terms lapse: the
        // scan is one indexed UPDATE bounded per pass, so a minute cadence
        // costs nothing and a backlog still drains over ticks.
        STATEMENT_PAST_DUE => Duration::minutes(1),
        // Hourly: often enough that a just-closed day reconciles before the
        // next reporting read, rare enough that each pass is a bounded diff
        // per organization.
        RECONCILE_FINANCIAL => Duration::hours(1),
        _ => return None,
    })
}

/// The cooldown after which a `dead` periodic run's kind is re-enqueued: an
/// hour of quiet rather than either an instant retry loop or a permanent stop.
pub const DEAD_RESCHEDULE: Duration = Duration::seconds(DEAD_RESCHEDULE_SECS);

/// What a statements pass did.
#[derive(Debug, Default, Clone, Copy)]
pub struct StatementReport {
    /// Organizations the pass visited.
    pub organizations: i64,
    /// Statements the run generated — a draft rebuilt still counts once.
    pub statements: i64,
    /// Finalized statements flipped to overdue.
    pub overdue_flipped: i64,
    /// Organizations the dunning sweep suspended (issue #258).
    pub suspended: i64,
    /// Organizations the dunning sweep reinstated (issue #258).
    pub reinstated: i64,
}

/// How many failures a close-month run names; `failed` counts every one.
const MAX_LISTED_FAILURES: usize = 50;
/// How many inserts a close-month enqueue tries while the kind's slot keeps
/// freeing between its insert and its read of the run holding it.
const CLOSE_MONTH_ENQUEUE_ATTEMPTS: usize = 3;
/// The reason a failure carries when its error may only reach the server log.
const UNSHOWN_REASON: &str = "the seal failed; the server log has the detail";

/// A job row's state, as `oxsum.jobs.status` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// Waiting for its `run_at`: not claimed yet, handed back by a leader
    /// yield, or a failed attempt waiting on the retry backoff.
    Pending,
    /// Claimed by a worker that is running it.
    Running,
    /// Finished.
    Done,
    /// Failed [`MAX_ATTEMPTS`] times; the row stays as the evidence.
    Dead,
}

impl FromStr for JobStatus {
    type Err = WalletError;

    fn from_str(stored: &str) -> Result<Self, Self::Err> {
        Ok(match stored {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "done" => Self::Done,
            "dead" => Self::Dead,
            other => return Err(crate::orgs::malformed("jobs.status", other)),
        })
    }
}

/// The manual close [`Db::enqueue_close_month`] queued or found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseMonthQueued {
    /// The run: the row this call inserted, or the pending or running run of
    /// the same month it found.
    pub job_id: Uuid,
    /// `true` when this call queued the run, `false` when a run for the month
    /// was already pending or running.
    pub queued: bool,
}

/// One organization whose month did not seal in a close-month run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseMonthFailure {
    /// The organization.
    pub organization_id: Uuid,
    /// Its display name.
    pub name: String,
    /// Why the seal failed: the ledger calendar's refusal or a validation
    /// message, or a fixed generic reason when the error may only reach the
    /// server log.
    pub reason: String,
}

/// A close-month attempt's walk through the organizations, as the run's row
/// stores it in `result`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseMonthProgress {
    /// How many organizations the attempt walks.
    pub organizations: i64,
    /// How many of them it has visited.
    pub processed: i64,
    /// Organizations whose month is sealed, an already-sealed month included.
    pub sealed: i64,
    /// Organizations whose seal failed.
    pub failed: i64,
    /// The first fifty failures; `failed` counts all of them.
    pub failures: Vec<CloseMonthFailure>,
}

impl CloseMonthProgress {
    /// Counts one organization whose seal failed, and names it while the list
    /// has room.
    fn record_failure(&mut self, organization_id: Uuid, name: &str, reason: String) {
        self.failed += 1;
        if self.failures.len() < MAX_LISTED_FAILURES {
            self.failures.push(CloseMonthFailure {
                organization_id,
                name: name.to_owned(),
                reason,
            });
        }
    }

    /// The finished walk as the attempt's result. An attempt in which every
    /// organization failed is an error, so it retries on the backoff rather
    /// than reading as done; the error carries the counts alone, because the
    /// worker stores it in `last_error` and logs it.
    fn outcome(self) -> Result<Self, WalletError> {
        if self.sealed == 0 && self.failed > 0 {
            return Err(WalletError::Misconfigured(format!(
                "the close-month pass sealed none of {} organizations; {} failed",
                self.organizations, self.failed
            )));
        }
        Ok(self)
    }
}

/// One manual close-month run, as `GET /admin/closings/jobs/{jobId}` answers
/// it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseMonthJob {
    /// The run's id.
    pub job_id: Uuid,
    /// The month the run seals, `YYYY-MM`.
    pub month: String,
    /// Where the run stands.
    pub status: JobStatus,
    /// Failed attempts so far; the [`MAX_ATTEMPTS`]th lands the run dead.
    pub attempts: i32,
    /// The latest attempt's progress: zero counts and no failures before the
    /// first attempt starts.
    #[serde(flatten)]
    pub progress: CloseMonthProgress,
    /// When the run was queued.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the run landed done or dead; `None` while it is pending or running.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

/// The part of a failed seal's error a response may carry, or `None` when
/// only the server log may. The ledger calendar's refusals name period ids
/// and dates. Every validation message on the `Wallet::close_month` path is
/// fixed text, a date-range error or a period-label check. None of them names
/// a tenant, a schema or a stored value. A storage or configuration failure
/// can, so it is never shown.
fn shown_reason(error: &WalletError) -> Option<String> {
    match error {
        WalletError::Storage(PostgresError::Period(refusal)) => Some(refusal.to_string()),
        WalletError::InvalidInput(message) => Some(message.clone()),
        _ => None,
    }
}

/// The claimed row's columns, `j`-qualified: the claim's CTE exposes a `job_id`
/// of its own, so bare names would be ambiguous in the RETURNING.
const JOB_COLS: &str =
    "j.job_id, j.kind, j.run_at, j.status, j.attempts, j.claimed_at, j.payload, j.requires_leader";

fn job_from_row(row: &sqlx::postgres::PgRow) -> Result<Job, WalletError> {
    Ok(Job {
        job_id: row.try_get("job_id")?,
        kind: row.try_get("kind")?,
        run_at: row.try_get("run_at")?,
        attempts: row.try_get("attempts")?,
        claimed_at: row.try_get("claimed_at")?,
        payload: row.try_get("payload")?,
        requires_leader: row.try_get("requires_leader")?,
    })
}

impl Db {
    /// Enqueues one run of `kind` due at `run_at`. The `(kind)` partial unique
    /// index makes the insert idempotent — a pending or running row of the same
    /// kind means one is already scheduled, and this call answers `false`. The
    /// row inherits the kind's [`requires_leader`] flag: leader election is a
    /// property of the kind, not something one run may opt out of.
    pub async fn enqueue_job(
        &self,
        kind: &str,
        run_at: OffsetDateTime,
    ) -> Result<bool, WalletError> {
        Ok(self.insert_job(kind, run_at, Value::Null).await?.is_some())
    }

    /// Inserts one run of `kind` due at `run_at` carrying kind-specific
    /// `payload`; `None` when the `(kind)` partial unique index refuses it,
    /// because a pending or running run of the kind already holds the slot.
    async fn insert_job(
        &self,
        kind: &str,
        run_at: OffsetDateTime,
        payload: Value,
    ) -> Result<Option<Uuid>, WalletError> {
        Ok(sqlx::query_scalar(
            "INSERT INTO oxsum.jobs (kind, run_at, requires_leader, payload) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT DO NOTHING RETURNING job_id",
        )
        .bind(kind)
        .bind(run_at)
        .bind(requires_leader(kind))
        .bind(payload)
        .fetch_optional(self.pool())
        .await?)
    }

    /// Queues the manual close of `period` (`POST /admin/closings`), due now,
    /// and names the run: the row this call inserted, or the pending or
    /// running run of the same month a retry finds. The kind holds one
    /// pending or running run at a time, so a different month in flight is a
    /// conflict rather than a request silently dropped.
    ///
    /// # Errors
    ///
    /// [`WalletError::Conflict`] while a close of another month is pending or
    /// running; storage failures surface as [`WalletError`].
    pub async fn enqueue_close_month(
        &self,
        period: &StatementPeriod,
    ) -> Result<CloseMonthQueued, WalletError> {
        let month = period.label();
        for _ in 0..CLOSE_MONTH_ENQUEUE_ATTEMPTS {
            if let Some(job_id) = self
                .insert_job(
                    kinds::CLOSE_MONTH,
                    OffsetDateTime::now_utc(),
                    serde_json::json!({ "month": month }),
                )
                .await?
            {
                return Ok(CloseMonthQueued {
                    job_id,
                    queued: true,
                });
            }
            let live = sqlx::query(
                "SELECT job_id, payload->>'month' AS month FROM oxsum.jobs \
                 WHERE kind = $1 AND status IN ('pending', 'running')",
            )
            .bind(kinds::CLOSE_MONTH)
            .fetch_optional(self.pool())
            .await?;
            // The run holding the slot finished between the insert and this
            // read: the slot is free again.
            let Some(live) = live else {
                continue;
            };
            let job_id: Uuid = live.try_get("job_id")?;
            let other: Option<String> = live.try_get("month")?;
            if other.as_deref() == Some(month.as_str()) {
                return Ok(CloseMonthQueued {
                    job_id,
                    queued: false,
                });
            }
            return Err(WalletError::Conflict(format!(
                "a close of {} is still pending or running; queue {month} once it finishes",
                other.as_deref().unwrap_or("another month")
            )));
        }
        Err(WalletError::Conflict(format!(
            "the close of {month} could not be queued while other closes kept starting \
             and finishing; try again"
        )))
    }

    /// One manual close-month run, as `GET /admin/closings/jobs/{jobId}`
    /// answers it; `None` when no close-month row has the id — unknown,
    /// another kind's, or removed by the jobs retention window.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]; so does a stored status,
    /// month or progress this build cannot read.
    pub async fn close_month_job(
        &self,
        job_id: Uuid,
    ) -> Result<Option<CloseMonthJob>, WalletError> {
        let row = sqlx::query(
            "SELECT job_id, payload->>'month' AS month, status, attempts, result, \
                    created_at, finished_at \
             FROM oxsum.jobs WHERE job_id = $1 AND kind = $2",
        )
        .bind(job_id)
        .bind(kinds::CLOSE_MONTH)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let month = row.try_get::<Option<String>, _>("month")?.ok_or_else(|| {
            WalletError::Storage(PostgresError::Malformed(
                "a close-month run's payload carries no month".to_owned(),
            ))
        })?;
        let progress: Option<sqlx::types::Json<CloseMonthProgress>> = row.try_get("result")?;
        Ok(Some(CloseMonthJob {
            job_id: row.try_get("job_id")?,
            month,
            status: row.try_get::<String, _>("status")?.parse()?,
            attempts: row.try_get("attempts")?,
            progress: progress.map(|json| json.0).unwrap_or_default(),
            created_at: row.try_get("created_at")?,
            finished_at: row.try_get("finished_at")?,
        }))
    }

    /// Moves up to `limit` due runs `pending` → `running` atomically. A row left
    /// `running` past [`CLAIM_LEASE_SECS`] belonged to a worker that died mid-run
    /// and is claimed again.
    pub async fn claim_due_jobs(&self, limit: i64) -> Result<Vec<Job>, WalletError> {
        let rows = sqlx::query(&format!(
            "WITH due AS ( \
                 SELECT job_id FROM oxsum.jobs \
                 WHERE (status = 'pending' AND run_at <= now()) \
                    OR (status = 'running' AND claimed_at <= now() - interval '{CLAIM_LEASE_SECS} seconds') \
                 ORDER BY run_at LIMIT {limit} \
                 FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE oxsum.jobs j \
             SET status = 'running', claimed_at = now() \
             FROM due WHERE j.job_id = due.job_id \
             RETURNING {JOB_COLS}"
        ))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(job_from_row).collect()
    }

    /// Hands a claimed run back to `pending` without spending an attempt — the
    /// yield a worker performs when it takes a leader-required row but cannot
    /// take the kind's advisory lock (someone else is mid-pass). `postpone`
    /// pushes `run_at` out so the same row is not reclaimed in a hot loop
    /// while the leader is still working. Fenced on the claim stamp like
    /// [`finish_job`](Self::finish_job); `false` when the lease already moved
    /// the row to a new claimant.
    pub async fn unclaim_job(
        &self,
        job_id: Uuid,
        claimed_at: OffsetDateTime,
        postpone: Duration,
    ) -> Result<bool, WalletError> {
        let changed = sqlx::query(
            "UPDATE oxsum.jobs SET status = 'pending', claimed_at = NULL, run_at = $3 \
             WHERE job_id = $1 AND claimed_at = $2",
        )
        .bind(job_id)
        .bind(claimed_at)
        .bind(OffsetDateTime::now_utc() + postpone)
        .execute(self.pool())
        .await?;
        Ok(changed.rows_affected() > 0)
    }

    /// Tries to take the kind's leader-election advisory lock on the given
    /// connection (issue #281). The lock is session-scoped — it survives the
    /// statement that takes it — so the caller must hold the same connection
    /// for the whole pass and release with `pg_advisory_unlock` (or by closing
    /// it) afterwards. `pg_try_advisory_lock` never blocks: a `false` answer
    /// means another instance is mid-pass and this tick should yield.
    pub async fn try_leader_lock(
        conn: &mut sqlx::postgres::PgConnection,
        kind: &str,
    ) -> Result<bool, WalletError> {
        let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtext($1))")
            .bind(leader_lock_key(kind))
            .fetch_one(&mut *conn)
            .await?;
        Ok(taken)
    }

    /// Releases the kind's leader-election lock on the same connection that
    /// took it. A `false` answer means the connection did not hold it — already
    /// released, or a different connection took it — which the caller logs
    /// rather than treats as fatal.
    pub async fn leader_unlock(
        conn: &mut sqlx::postgres::PgConnection,
        kind: &str,
    ) -> Result<bool, WalletError> {
        let released: bool = sqlx::query_scalar("SELECT pg_advisory_unlock(hashtext($1))")
            .bind(leader_lock_key(kind))
            .fetch_one(&mut *conn)
            .await?;
        Ok(released)
    }

    /// Marks a claimed run `done`; `false` when the claim stamp no longer
    /// matches — the lease lapsed and a later claim owns the row now.
    pub async fn finish_job(
        &self,
        job_id: Uuid,
        claimed_at: OffsetDateTime,
    ) -> Result<bool, WalletError> {
        let changed = sqlx::query(
            "UPDATE oxsum.jobs SET status = 'done', finished_at = now() \
             WHERE job_id = $1 AND claimed_at = $2",
        )
        .bind(job_id)
        .bind(claimed_at)
        .execute(self.pool())
        .await?;
        Ok(changed.rows_affected() > 0)
    }

    /// Fails a claimed run: one more attempt, back into `pending` on the
    /// backoff, or `dead` past [`MAX_ATTEMPTS`]. Fenced on the claim stamp like
    /// [`finish_job`](Self::finish_job); an unfenced call reports nothing.
    pub async fn fail_job(
        &self,
        job_id: Uuid,
        claimed_at: OffsetDateTime,
        error: &str,
    ) -> Result<Option<JobOutcome>, WalletError> {
        let rows = sqlx::query(&format!(
            "UPDATE oxsum.jobs SET \
                 attempts = attempts + 1, \
                 last_error = $3, \
                 claimed_at = NULL, \
                 status = CASE WHEN attempts + 1 >= {MAX_ATTEMPTS} THEN 'dead' ELSE 'pending' END, \
                 run_at = CASE WHEN attempts + 1 >= {MAX_ATTEMPTS} THEN run_at \
                          ELSE now() + (ARRAY{RETRY_DELAYS_SECS:?})[LEAST(attempts + 1, {MAX_ATTEMPTS})] * interval '1 second' END, \
                 finished_at = CASE WHEN attempts + 1 >= {MAX_ATTEMPTS} THEN now() ELSE NULL END \
             WHERE job_id = $1 AND claimed_at = $2 \
             RETURNING status, run_at"
        ))
        .bind(job_id)
        .bind(claimed_at)
        .bind(error)
        .fetch_all(self.pool())
        .await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let status: &str = row.try_get("status")?;
        Ok(Some(if status == "dead" {
            JobOutcome::Dead
        } else {
            JobOutcome::Rescheduled(row.try_get("run_at")?)
        }))
    }

    /// The retention spec's batched cleanup (docs/decisions.md): idempotency
    /// records past their own `expires_at`, `usage_details.provider_raw` nulled
    /// past its window, terminal webhook deliveries and finished job rows past
    /// theirs. Each rule takes at most `RETENTION_BATCH` rows a pass — the
    /// daily cadence drains any backlog without one giant transaction.
    pub async fn run_retention(
        &self,
        retention: &Retention,
    ) -> Result<RetentionReport, WalletError> {
        let mut report = RetentionReport::default();
        let expired = sqlx::query(&format!(
            "DELETE FROM oxsum.idempotency_records WHERE (organization_id, idempotency_key) IN ( \
                 SELECT organization_id, idempotency_key FROM oxsum.idempotency_records \
                 WHERE expires_at <= now() LIMIT {RETENTION_BATCH})"
        ))
        .execute(self.pool())
        .await?;
        report.idempotency_records = expired.rows_affected();

        let raw_cutoff = OffsetDateTime::now_utc() - Duration::days(retention.provider_raw_days);
        let nulled = sqlx::query(&format!(
            "UPDATE oxsum.usage_records SET usage_details = usage_details - 'provider_raw' \
             WHERE request_id IN ( \
                 SELECT request_id FROM oxsum.usage_records \
                 WHERE usage_details ? 'provider_raw' AND settled_at <= $1 LIMIT {RETENTION_BATCH})"
        ))
        .bind(raw_cutoff)
        .execute(self.pool())
        .await?;
        report.provider_raw_nulled = nulled.rows_affected();

        let deliveries_cutoff =
            OffsetDateTime::now_utc() - Duration::days(retention.deliveries_days);
        let deliveries = sqlx::query(&format!(
            "DELETE FROM oxsum.webhook_deliveries WHERE delivery_id IN ( \
                 SELECT delivery_id FROM oxsum.webhook_deliveries \
                 WHERE status IN ('delivered', 'failed') \
                   AND COALESCE(delivered_at, next_attempt_at) <= $1 LIMIT {RETENTION_BATCH})"
        ))
        .bind(deliveries_cutoff)
        .execute(self.pool())
        .await?;
        report.webhook_deliveries = deliveries.rows_affected();

        let jobs_cutoff = OffsetDateTime::now_utc() - Duration::days(retention.jobs_days);
        let jobs = sqlx::query(&format!(
            "DELETE FROM oxsum.jobs WHERE job_id IN ( \
                 SELECT job_id FROM oxsum.jobs \
                 WHERE status IN ('done', 'dead') AND finished_at <= $1 LIMIT {RETENTION_BATCH})"
        ))
        .bind(jobs_cutoff)
        .execute(self.pool())
        .await?;
        report.jobs = jobs.rows_affected();

        let request_log_cutoff =
            OffsetDateTime::now_utc() - Duration::days(retention.request_log_days);
        let request_log = sqlx::query(&format!(
            "DELETE FROM oxsum.request_log WHERE (request_id, tenant_id) IN ( \
                 SELECT request_id, tenant_id FROM oxsum.request_log \
                 WHERE created_at <= $1 LIMIT {RETENTION_BATCH})"
        ))
        .bind(request_log_cutoff)
        .execute(self.pool())
        .await?;
        report.request_log = request_log.rows_affected();

        Ok(report)
    }

    /// The period's aggregation job (`statements`): seals the month that just
    /// ended on every billable organization's ledger — both calls are idempotent,
    /// so the daily pass doubles as the late-usage pickup — generates its draft
    /// statements, and flips finalized ones whose due date passed to overdue.
    /// Drafts stay drafts: issuing the document stays the operator's call.
    pub async fn run_statement_due(
        &self,
        tenants: &crate::Tenants,
        today: Date,
    ) -> Result<StatementReport, WalletError> {
        let mut report = StatementReport::default();
        let this_month = today.replace_day(1).map_err(crate::error::invalid)?;
        let last_month = (this_month - Duration::days(1))
            .replace_day(1)
            .map_err(crate::error::invalid)?;
        let period = crate::statement_period(&format!(
            "{:04}-{:02}",
            last_month.year(),
            u8::from(last_month.month())
        ))?;

        for organization in self.billable_organizations().await? {
            let wallet = tenants.get(&organization.tenant_id).await?;
            wallet.close_month(last_month).await?;
            report.organizations += 1;
            if self
                .generate_statement(&organization, wallet.as_ref(), &period)
                .await?
                .is_some()
            {
                report.statements += 1;
            }
        }
        report.overdue_flipped = self.flip_overdue(today).await?;
        // Dunning follows the flip (issue #258): a statement that crossed its
        // grace today suspends in the same pass, and a repaid line's sweep-set
        // suspension lifts. `sweep_dunning` re-reads the standings the flip
        // just wrote, so it must run after it.
        let (suspended, reinstated) = self.sweep_dunning(today).await?;
        report.suspended = suspended;
        report.reinstated = reinstated;
        Ok(report)
    }

    /// Writes a close-month run's progress to its row, fenced on the claim
    /// stamp like [`finish_job`](Self::finish_job).
    ///
    /// # Errors
    ///
    /// [`WalletError::Conflict`] when the claim was lost: the row was handed
    /// back or claimed again, so the walk must stop rather than write over the
    /// next claimant's progress. Storage failures surface as [`WalletError`].
    async fn write_close_month_progress(
        &self,
        job_id: Uuid,
        claimed_at: OffsetDateTime,
        progress: &CloseMonthProgress,
    ) -> Result<(), WalletError> {
        let written =
            sqlx::query("UPDATE oxsum.jobs SET result = $3 WHERE job_id = $1 AND claimed_at = $2")
                .bind(job_id)
                .bind(claimed_at)
                .bind(sqlx::types::Json(progress))
                .execute(self.pool())
                .await?;
        if written.rows_affected() == 0 {
            tracing::warn!(%job_id, "a close-month run lost its claim; its walk stops");
            return Err(WalletError::Conflict(
                "the close-month run lost its claim to another worker".to_owned(),
            ));
        }
        Ok(())
    }

    /// Seals `first`'s month across every organization's ledger: the pass of
    /// the manual `close-month` run claimed as `job_id` at `claimed_at`. Each
    /// organization's `close_month` is independent. A failure is counted, and
    /// the first fifty are named with a reason a response may carry, rather
    /// than aborting the pass, so one bad ledger cannot hold every other
    /// organization's month hostage. A sealed month answers its standing
    /// record, so a re-run of a partially-failed pass closes only what was
    /// left.
    ///
    /// The progress lands on the run's row once the organizations are listed
    /// and after each one, fenced on the claim stamp, so the console's poll
    /// reads the walk as it happens. A reason the response may not carry goes
    /// to the server log with the organization's id.
    ///
    /// # Errors
    ///
    /// Listing the organizations or writing the progress failing is the
    /// pass's own error, and so is a lost claim
    /// ([`WalletError::Conflict`]): the walk stops there. An attempt in which
    /// every organization failed is an error carrying only the counts, so it
    /// retries on the backoff rather than reading as done.
    pub async fn close_month_organizations(
        &self,
        tenants: &crate::Tenants,
        first: Date,
        job_id: Uuid,
        claimed_at: OffsetDateTime,
    ) -> Result<CloseMonthProgress, WalletError> {
        let organizations = self.organizations().await?;
        let mut progress = CloseMonthProgress {
            organizations: i64::try_from(organizations.len()).unwrap_or(i64::MAX),
            ..CloseMonthProgress::default()
        };
        self.write_close_month_progress(job_id, claimed_at, &progress)
            .await?;
        for organization in &organizations {
            let failure = match tenants.get(&organization.tenant_id).await {
                // Opening the ledger validates the tenant id, and that
                // refusal names it, so no part of this error is shown.
                Err(error) => {
                    tracing::error!(
                        organization_id = %organization.id, %error,
                        "the close-month pass could not open an organization's ledger"
                    );
                    Some(UNSHOWN_REASON.to_owned())
                }
                Ok(wallet) => match wallet.close_month(first).await {
                    Ok(_) => None,
                    Err(error) => Some(match shown_reason(&error) {
                        Some(reason) => {
                            tracing::warn!(
                                organization_id = %organization.id, %error,
                                "an organization's month did not seal"
                            );
                            reason
                        }
                        None => {
                            tracing::error!(
                                organization_id = %organization.id, %error,
                                "an organization's month did not seal"
                            );
                            UNSHOWN_REASON.to_owned()
                        }
                    }),
                },
            };
            match failure {
                None => progress.sealed += 1,
                Some(reason) => {
                    progress.record_failure(organization.id, &organization.name, reason)
                }
            }
            progress.processed += 1;
            self.write_close_month_progress(job_id, claimed_at, &progress)
                .await?;
        }
        progress.outcome()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use doubleentry::{PeriodError, PeriodId};

    use super::*;

    fn failing(organizations: i64, sealed: i64, failed: usize) -> CloseMonthProgress {
        let mut progress = CloseMonthProgress {
            organizations,
            sealed,
            ..CloseMonthProgress::default()
        };
        for n in 0..failed {
            progress.record_failure(
                Uuid::new_v4(),
                &format!("Organization {n}"),
                format!("a reason naming ledger_{n}"),
            );
        }
        progress.processed = sealed + i64::try_from(failed).unwrap();
        progress
    }

    #[test]
    fn an_attempt_in_which_every_organization_failed_errs_with_counts_only() {
        let Err(WalletError::Misconfigured(message)) = failing(2, 0, 2).outcome() else {
            panic!("an all-failed attempt is an error");
        };
        assert_eq!(
            message,
            "the close-month pass sealed none of 2 organizations; 2 failed"
        );
    }

    #[test]
    fn an_attempt_that_sealed_one_organization_finishes_with_its_failures() {
        let progress = failing(3, 1, 2)
            .outcome()
            .expect("one seal finishes the run");
        assert_eq!((progress.sealed, progress.failed), (1, 2));
        assert_eq!(progress.failures.len(), 2);
    }

    #[test]
    fn an_attempt_with_no_organizations_finishes() {
        assert_eq!(
            failing(0, 0, 0).outcome().expect("nothing to seal is done"),
            CloseMonthProgress::default()
        );
    }

    #[test]
    fn the_failure_list_names_the_first_fifty_and_counts_the_rest() {
        let progress = failing(60, 0, 60);
        assert_eq!(progress.failed, 60);
        assert_eq!(progress.failures.len(), MAX_LISTED_FAILURES);
        assert_eq!(progress.failures[0].name, "Organization 0");
        assert_eq!(progress.failures[49].name, "Organization 49");
    }

    #[test]
    fn only_calendar_refusals_and_validation_messages_are_shown() {
        let refusal = PeriodError::Unknown {
            id: PeriodId::new("2026-09").unwrap(),
        };
        assert_eq!(
            shown_reason(&WalletError::Storage(PostgresError::Period(
                refusal.clone()
            ))),
            Some(refusal.to_string())
        );
        assert_eq!(
            shown_reason(&WalletError::InvalidInput(
                "only a month that has fully ended can be closed".to_owned()
            )),
            Some("only a month that has fully ended can be closed".to_owned())
        );
        for hidden in [
            WalletError::Storage(PostgresError::Malformed(
                "ledger_0123 holds an unknown value".to_owned(),
            )),
            WalletError::Misconfigured("ledger_0123".to_owned()),
            WalletError::Conflict("ledger_0123".to_owned()),
            WalletError::PoolBusy,
        ] {
            assert_eq!(shown_reason(&hidden), None, "{hidden:?} is not shown");
        }
    }

    #[test]
    fn a_stored_status_parses_and_an_unknown_one_is_malformed() {
        for (stored, status) in [
            ("pending", JobStatus::Pending),
            ("running", JobStatus::Running),
            ("done", JobStatus::Done),
            ("dead", JobStatus::Dead),
        ] {
            assert_eq!(stored.parse::<JobStatus>().unwrap(), status);
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::json!(stored)
            );
        }
        assert!(matches!(
            "paused".parse::<JobStatus>(),
            Err(WalletError::Storage(PostgresError::Malformed(_)))
        ));
    }
}
