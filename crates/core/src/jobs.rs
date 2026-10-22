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

use serde_json::Value;
use sqlx::Row;
use time::{Date, Duration, OffsetDateTime};
use uuid::Uuid;

use crate::error::WalletError;

use crate::Db;

/// The periodic kinds, as `oxsum.jobs.kind` spells them.
pub mod kinds {
    /// The stale-hold pass `sweep_stale_holds` ran on an in-process interval.
    pub const SWEEP_HOLDS: &str = "sweep-holds";
    /// One `deliver_due` pass over the webhook queue.
    pub const DELIVER_WEBHOOKS: &str = "deliver-webhooks";
    /// The scheduled `Db::reconcile` scan — drift the on-demand endpoint also reads.
    pub const RECONCILE: &str = "reconcile";
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
    pub job_id: Uuid,
    pub kind: String,
    pub run_at: OffsetDateTime,
    pub attempts: i32,
    /// The claim's stamp: finish and fail fence on it, so a worker whose lease
    /// lapsed cannot overwrite the next claimant's state.
    pub claimed_at: OffsetDateTime,
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
            | RECONCILE
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
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            provider_raw_days: 7,
            deliveries_days: 30,
            jobs_days: 90,
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
}

/// How far out the next periodic occurrence of `kind` is due — the worker asks
/// for it after every finished run. An unknown kind gets no next run.
pub fn next_run(kind: &str) -> Option<Duration> {
    use kinds::*;
    Some(match kind {
        SWEEP_HOLDS => Duration::seconds(60),
        DELIVER_WEBHOOKS => Duration::seconds(30),
        RECONCILE | STATEMENTS | RETENTION | SYNC_UPSTREAM_PRICES | UPSTREAM_BILL_SYNC
        | USAGE_ANCHORS => Duration::days(1),
        // Anchoring is hourly: often enough that a head ages at most an hour
        // before a witness vouches for it, rare enough that every pass is one
        // request per organization per witness.
        WITNESS_CHECKPOINTS => Duration::hours(1),
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
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO oxsum.jobs (kind, run_at, requires_leader) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING RETURNING job_id",
        )
        .bind(kind)
        .bind(run_at)
        .bind(requires_leader(kind))
        .fetch_optional(self.pool())
        .await?;
        Ok(inserted.is_some())
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
    /// theirs. Each rule takes at most [`RETENTION_BATCH`] rows a pass — the
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
}
