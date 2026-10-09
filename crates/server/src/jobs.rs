//! The jobs worker (issue #166, roadmap P8-1).
//!
//! One background task claims due runs off `oxsum.jobs` — the periodic kinds
//! [`kinds`] names — and runs each kind's pass. The claim is `SKIP LOCKED` plus
//! a lease, so the queue is already multi-instance safe; a worker that dies
//! mid-run simply has its row claimed again once the lease lapses. Every
//! handler is idempotent, which is what makes that takeover safe.
//!
//! On a finished run the next occurrence is enqueued at the kind's own
//! interval; on a failed one [`Db::fail_job`] backs off until the row lands
//! `dead`, where it stands as the `jobs_dead` drift class's evidence while the
//! kind is re-enqueued on the dead cooldown.

use std::time::Duration;

use oxsum_core::{
    DEAD_RESCHEDULE, Db, JobOutcome, Retention, SecretKey, Tenants, kinds, next_run,
    sweep_stale_holds,
};
use time::OffsetDateTime;
use tracing::{error, info, warn};

use crate::metrics::Metrics;

/// How often the worker asks the table for due runs. The kinds' own `run_at`
/// spacing is what bounds how often a pass runs — the poll only bounds how
/// late one starts.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// What one run needs beyond the database.
#[derive(Clone)]
pub struct JobContext {
    /// The sweeper's stale-hold age, `OXSUM_HOLD_TIMEOUT`.
    pub hold_timeout: Duration,
    /// The shared registry `/metrics` renders.
    pub metrics: Metrics,
    /// The webhook delivery pass's pieces — `None` runs no `deliver-webhooks`
    /// kind, the same deployment `POST /api/v1/webhooks` refuses on.
    pub webhook: Option<(reqwest::Client, SecretKey)>,
    /// The retention windows, `OXSUM_RETENTION_*`.
    pub retention: Retention,
}

/// The background task: startup enqueues whatever periodic kinds have nothing
/// due — which both schedules a first pass the way the old in-process loops
/// ran one, and heals a chain whose last link died with the process.
pub fn spawn_worker(db: Db, ctx: JobContext) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut periodic = vec![
            kinds::SWEEP_HOLDS,
            kinds::RECONCILE,
            kinds::STATEMENTS,
            kinds::RETENTION,
        ];
        if ctx.webhook.is_some() {
            periodic.push(kinds::DELIVER_WEBHOOKS);
        }
        for kind in periodic {
            match db.enqueue_job(kind, OffsetDateTime::now_utc()).await {
                Ok(true) => info!(kind, "scheduled a periodic job"),
                Ok(false) => {}
                Err(error) => error!(kind, %error, "could not schedule a periodic job"),
            }
        }
        loop {
            run_due(&db, &ctx).await;
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
}

/// Claims due runs and dispatches each to its kind's pass — one iteration of
/// the worker loop, exposed so tests can drive it without the timer.
pub async fn run_due(db: &Db, ctx: &JobContext) {
    let jobs = match db.claim_due_jobs(16).await {
        Ok(jobs) => jobs,
        Err(error) => {
            error!(%error, "the jobs worker could not claim due runs");
            return;
        }
    };
    for job in jobs {
        match run_one(db, ctx, &job).await {
            Ok(()) => {
                ctx.metrics.job_run(&job.kind, "done");
                if db
                    .finish_job(job.job_id, job.claimed_at)
                    .await
                    .unwrap_or(false)
                    && let Some(interval) = next_run(&job.kind)
                {
                    // The claim still stands, so the next occurrence is this
                    // worker's to schedule; a lapsed lease lets the next
                    // claimant schedule it instead.
                    let _ = db
                        .enqueue_job(&job.kind, OffsetDateTime::now_utc() + interval)
                        .await;
                }
            }
            Err(error) => {
                match db
                    .fail_job(job.job_id, job.claimed_at, &error.to_string())
                    .await
                {
                    Ok(Some(JobOutcome::Rescheduled(run_at))) => {
                        ctx.metrics.job_run(&job.kind, "failed");
                        warn!(kind = %job.kind, %error, %run_at, "a periodic job failed, retrying on the backoff");
                    }
                    Ok(Some(JobOutcome::Dead)) => {
                        ctx.metrics.job_run(&job.kind, "dead");
                        error!(kind = %job.kind, %error, "a periodic job exhausted its attempts");
                        // The dead row stays as the jobs_dead drift's evidence;
                        // a periodic kind's chain resumes on the cooldown rather
                        // than holding the pass hostage to one bad patch. An
                        // unknown kind carries no cadence — it stays dead.
                        if next_run(&job.kind).is_some() {
                            let _ = db
                                .enqueue_job(&job.kind, OffsetDateTime::now_utc() + DEAD_RESCHEDULE)
                                .await;
                        }
                    }
                    // The lease lapsed: whoever claimed the row next owns the
                    // outcome, so there is nothing here to record.
                    Ok(None) => {}
                    Err(error) => {
                        error!(kind = %job.kind, %error, "could not record a job's failure");
                    }
                }
            }
        }
    }
}

/// One run of the kind's pass.
async fn run_one(
    db: &Db,
    ctx: &JobContext,
    job: &oxsum_core::Job,
) -> Result<(), oxsum_core::WalletError> {
    let tenants = Tenants::new(db.pool().clone());
    match job.kind.as_str() {
        kinds::SWEEP_HOLDS => {
            let older_than = OffsetDateTime::now_utc() - ctx.hold_timeout;
            let on = OffsetDateTime::now_utc().date();
            let report = sweep_stale_holds(db, &tenants, older_than, on).await?;
            if report.resolved > 0 {
                ctx.metrics.swept(report.resolved as u64);
                info!(
                    resolved = report.resolved,
                    "the hold sweeper resolved stale holds"
                );
            }
            if report.dead_lettered > 0 {
                ctx.metrics.dead_lettered(report.dead_lettered as u64);
                warn!(
                    dead_lettered = report.dead_lettered,
                    "the hold sweeper dead-lettered holds"
                );
            }
        }
        kinds::DELIVER_WEBHOOKS => {
            let Some((http, secret)) = &ctx.webhook else {
                return Err(oxsum_core::WalletError::Misconfigured(
                    "deliver-webhooks has no sealing key configured".into(),
                ));
            };
            crate::webhooks::deliver_due(db, http, secret, &ctx.metrics, None).await;
        }
        kinds::RECONCILE => {
            let report = db.reconcile().await?;
            let drifted = report
                .classes
                .iter()
                .filter(|class| class.count > 0)
                .map(|class| format!("{:?}:{}", class.class, class.count))
                .collect::<Vec<_>>();
            if drifted.is_empty() {
                info!("the scheduled reconciliation found no drift");
            } else {
                warn!(drift = ?drifted, "the scheduled reconciliation found drift");
            }
        }
        kinds::STATEMENTS => {
            let report = db
                .run_statement_due(&tenants, OffsetDateTime::now_utc().date())
                .await?;
            info!(
                organizations = report.organizations,
                statements = report.statements,
                overdue_flipped = report.overdue_flipped,
                "the statements pass closed the ended month"
            );
        }
        kinds::RETENTION => {
            let report = db.run_retention(&ctx.retention).await?;
            info!(
                idempotency_records = report.idempotency_records,
                provider_raw_nulled = report.provider_raw_nulled,
                webhook_deliveries = report.webhook_deliveries,
                jobs = report.jobs,
                "the retention pass cleaned up"
            );
        }
        other => {
            return Err(oxsum_core::WalletError::InvalidInput(format!(
                "unknown job kind {other:?}"
            )));
        }
    }
    Ok(())
}
