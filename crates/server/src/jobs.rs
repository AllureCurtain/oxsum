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
    DEAD_RESCHEDULE, Db, JobOutcome, ModelPair, ProbeOutcome, Retention, SecretKey, Tenants,
    UpstreamPrices, kinds, next_run, sweep_stale_holds,
};
use serde::Deserialize;
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
    /// kind, the same deployment `POST /api/v1/webhooks` refuses on. The
    /// upstream price-sync pass reuses the pair: the client fetches the live
    /// catalog, the key unseals each channel's `api_key_sealed` for the probe's
    /// `Authorization` header (issue #222).
    pub webhook: Option<(reqwest::Client, SecretKey)>,
    /// The retention windows, `OXSUM_RETENTION_*`.
    pub retention: Retention,
    /// Whether `OXSUM_UPSTREAM_PRICE_SYNC` lets the `SYNC_UPSTREAM_PRICES`
    /// periodic kind run. It still needs `webhook` for the fetcher and the
    /// credential unsealer, so both must hold (issue #222).
    pub upstream_price_sync: bool,
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
        if ctx.webhook.is_some() && ctx.upstream_price_sync {
            periodic.push(kinds::SYNC_UPSTREAM_PRICES);
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
        kinds::SYNC_UPSTREAM_PRICES => {
            let Some((http, secret)) = &ctx.webhook else {
                return Err(oxsum_core::WalletError::Misconfigured(
                    "sync-upstream-prices has no sealing key configured".into(),
                ));
            };
            let report = sync_upstream_prices(db, http, secret).await?;
            info!(
                channels = report.channels,
                probed = report.probed,
                drifted = report.drifted,
                "the upstream price-sync pass probed the channels"
            );
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

/// What a `SYNC_UPSTREAM_PRICES` pass did, for the worker's log line.
struct SyncReport {
    /// The channels the pass tried to probe.
    channels: usize,
    /// The channels whose catalog answered and diffed.
    probed: usize,
    /// The open drift rows the pass left behind.
    drifted: usize,
}

/// The live catalog as a probe reads it: `{data:{models:[...]}}`, each model
/// carrying the model name and its `upstream` block. `CatalogModel` is
/// serialize-only, so the wire side needs its own shape (issue #222).
#[derive(Deserialize)]
struct RemoteCatalog {
    data: RemoteData,
}

#[derive(Deserialize)]
struct RemoteData {
    models: Vec<RemotePricedModel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePricedModel {
    model: String,
    upstream: Option<UpstreamPrices>,
}

/// One `SYNC_UPSTREAM_PRICES` pass: probe every channel's live catalog, diff
/// its `upstream` rates against the stored ones, and let the persistence layer
/// open or resolve drift. A channel that will not answer records its outcome
/// and the pass moves on — drift is detected, never inferred from silence, so
/// one unreachable channel never fails the pass.
async fn sync_upstream_prices(
    db: &Db,
    http: &reqwest::Client,
    secret: &SecretKey,
) -> Result<SyncReport, oxsum_core::WalletError> {
    let channels = db.probe_channels().await?;
    let stored = db.catalog().await?;
    let mut report = SyncReport {
        channels: channels.len(),
        probed: 0,
        drifted: 0,
    };
    for channel in channels {
        match probe_channel(db, http, secret, &channel, &stored).await {
            Ok(drifted) => {
                report.probed += 1;
                report.drifted += drifted;
            }
            Err(error) => {
                error!(channel = %channel.name, %error, "the upstream price probe failed");
            }
        }
    }
    Ok(report)
}

/// Probes one channel and returns how many fresh drift rows the diff opened.
/// Every failure is recorded as its own probe outcome rather than propagated:
/// the channel's catalog is the probe's business, not the pass's.
async fn probe_channel(
    db: &Db,
    http: &reqwest::Client,
    secret: &SecretKey,
    channel: &oxsum_core::ProbeChannel,
    stored: &[oxsum_core::CatalogModel],
) -> Result<usize, oxsum_core::WalletError> {
    let url = format!("{}/api/v1/pricing", channel.base_url.trim_end_matches('/'));
    let api_key = match secret.open(&channel.api_key_sealed) {
        Ok(api_key) => api_key,
        Err(error) => {
            db.record_probe(&channel.name, ProbeOutcome::Auth, &error.to_string())
                .await?;
            return Ok(0);
        }
    };
    let response = match http
        .get(&url)
        .bearer_auth(api_key)
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            db.record_probe(&channel.name, ProbeOutcome::Unreachable, &error.to_string())
                .await?;
            return Ok(0);
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        db.record_probe(&channel.name, ProbeOutcome::Auth, status.as_str())
            .await?;
        return Ok(0);
    }
    if !status.is_success() {
        db.record_probe(&channel.name, ProbeOutcome::Invalid, status.as_str())
            .await?;
        return Ok(0);
    }
    let catalog = match response.json::<RemoteCatalog>().await {
        Ok(catalog) => catalog,
        Err(error) => {
            db.record_probe(&channel.name, ProbeOutcome::Invalid, &error.to_string())
                .await?;
            return Ok(0);
        }
    };

    // Pair each stored model for this channel with the live entry of the same
    // name. A live model the stored side never saw has nothing to contradict,
    // so it is not added; a stored model the live catalog dropped pairs
    // `live: None` and resolves whatever drift was open on it.
    let mut pairs: Vec<ModelPair<'_>> = Vec::new();
    let mut drifted = 0;
    for entry in stored.iter().filter(|entry| entry.channel == channel.name) {
        let live = catalog
            .data
            .models
            .iter()
            .find(|remote| remote.model == entry.model);
        pairs.push(ModelPair {
            model: &entry.model,
            stored: entry.price.upstream.as_ref(),
            live: live.and_then(|remote| remote.upstream.as_ref()),
        });
        if let (Some(stored_up), Some(live_up)) = (
            entry.price.upstream.as_ref(),
            live.and_then(|r| r.upstream.as_ref()),
        ) {
            drifted += oxsum_core::diff_upstream(stored_up, live_up).len();
        }
    }
    db.sync_upstream_prices(&channel.name, &pairs).await?;
    db.record_probe(
        &channel.name,
        ProbeOutcome::Ok,
        &format!("{} models", catalog.data.models.len()),
    )
    .await?;
    Ok(drifted)
}
