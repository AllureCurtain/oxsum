//! The durable periodic-work table (issue #166): claiming, fencing, retries,
//! the dead-letter state, the retention pass, the statements aggregation pass,
//! and the `jobs_dead` drift class. Needs DATABASE_URL, skips without it.

use oxsum_core::{
    CloseMonthProgress, Db, JobOutcome, JobStatus, MAX_ATTEMPTS, NewUser, Retention, SecretKey,
    SettlementKind, Tenants, UsageRecord, UsageRow, WalletError, kinds, next_run,
};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Claims take whatever is due regardless of kind, so two tests running at
/// once would claim each other's rows: the whole suite runs serially.
static JOBS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

macro_rules! db_or_skip {
    () => {
        match oxsum_testkit::suite_url!() {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// A migrated database and a registered organization (id + tenant id).
async fn world(url: &str) -> (Db, Uuid, String) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let email = format!(
        "jobs_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let registration = db
        .register(NewUser {
            email,
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    let org = registration.organization.id;
    (db, org, org.simple().to_string())
}

/// A job kind no other test or running worker touches: the singleton index
/// makes kinds globally unique while pending, so tests pick fresh names.
fn kind() -> String {
    format!("test-kind-{}", &Uuid::new_v4().simple().to_string()[..8])
}

async fn status_of(db: &Db, job_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM oxsum.jobs WHERE job_id = $1")
        .bind(job_id)
        .fetch_one(db.pool())
        .await
        .expect("the job row reads back")
}

/// One pending run of `kind`, due now; the row's id.
async fn enqueued(db: &Db, kind: &str) -> Uuid {
    assert!(
        db.enqueue_job(kind, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    sqlx::query_scalar("SELECT job_id FROM oxsum.jobs WHERE kind = $1 AND status = 'pending'")
        .bind(kind)
        .fetch_one(db.pool())
        .await
        .expect("the enqueued row reads back")
}

#[tokio::test]
async fn enqueue_is_idempotent_while_a_run_is_outstanding() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    let kind = kind();
    let now = OffsetDateTime::now_utc();

    assert!(db.enqueue_job(&kind, now).await.unwrap());
    // A pending row of the kind means one is already scheduled.
    assert!(!db.enqueue_job(&kind, now).await.unwrap());

    let job = db.claim_due_jobs(4).await.unwrap();
    let job = job.iter().find(|j| j.kind == kind).expect("claimed");
    // A running row still blocks the next occurrence.
    assert!(!db.enqueue_job(&kind, now).await.unwrap());

    assert!(db.finish_job(job.job_id, job.claimed_at).await.unwrap());
    // `done` is history — the kind may schedule again.
    assert!(db.enqueue_job(&kind, now).await.unwrap());
    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = $1")
        .bind(&kind)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_due_run_is_claimed_once_until_its_lease_lapses() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    let kind = kind();
    enqueued(&db, &kind).await;

    let first = db.claim_due_jobs(4).await.unwrap();
    let first = first.iter().find(|j| j.kind == kind).expect("claimed");

    // Held under a live lease, the row is invisible to the next claim.
    let second = db.claim_due_jobs(64).await.unwrap();
    assert!(second.iter().all(|j| j.kind != kind));

    // Past the lease the row belonged to a worker that died: it is claimed again.
    sqlx::query(
        "UPDATE oxsum.jobs SET claimed_at = now() - interval '400 seconds' WHERE job_id = $1",
    )
    .bind(first.job_id)
    .execute(db.pool())
    .await
    .unwrap();
    let third = db.claim_due_jobs(64).await.unwrap();
    let reclaimed = third
        .iter()
        .find(|j| j.job_id == first.job_id)
        .expect("reclaimed after the lease lapsed");
    assert!(reclaimed.claimed_at > first.claimed_at);
    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = $1")
        .bind(&kind)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn finish_and_fail_fence_on_the_claim_stamp() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    let kind = kind();
    let job_id = enqueued(&db, &kind).await;

    // A stamp that was never issued fences nothing's write.
    assert!(
        !db.finish_job(job_id, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    assert!(
        db.fail_job(job_id, OffsetDateTime::now_utc(), "stale claim")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(status_of(&db, job_id).await, "pending");

    let job = db
        .claim_due_jobs(4)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("claimed");
    assert!(db.finish_job(job.job_id, job.claimed_at).await.unwrap());
    assert_eq!(status_of(&db, job_id).await, "done");
    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = $1")
        .bind(&kind)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn failures_back_off_until_the_run_lands_dead() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    let kind = kind();
    let job_id = enqueued(&db, &kind).await;

    for attempt in 1..MAX_ATTEMPTS {
        // A rescheduled run is due on its backoff — the test fast-forwards it
        // rather than waiting real seconds.
        sqlx::query("UPDATE oxsum.jobs SET run_at = now() WHERE job_id = $1")
            .bind(job_id)
            .execute(db.pool())
            .await
            .unwrap();
        let job = db
            .claim_due_jobs(64)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.job_id == job_id)
            .expect("claimed");
        match db
            .fail_job(job.job_id, job.claimed_at, &format!("attempt {attempt}"))
            .await
            .unwrap()
        {
            Some(JobOutcome::Rescheduled(run_at)) => {
                assert!(run_at > OffsetDateTime::now_utc());
            }
            other => panic!("attempt {attempt} should reschedule, got {other:?}"),
        }
        assert_eq!(status_of(&db, job_id).await, "pending");
    }

    // The last failure exhausts the budget.
    sqlx::query("UPDATE oxsum.jobs SET run_at = now() WHERE job_id = $1")
        .bind(job_id)
        .execute(db.pool())
        .await
        .unwrap();
    let job = db
        .claim_due_jobs(64)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("claimed");
    assert_eq!(
        db.fail_job(job.job_id, job.claimed_at, "the last straw")
            .await
            .unwrap(),
        Some(JobOutcome::Dead)
    );
    assert_eq!(status_of(&db, job_id).await, "dead");
    let last_error: String =
        sqlx::query_scalar("SELECT last_error FROM oxsum.jobs WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(last_error, "the last straw");
    sqlx::query("DELETE FROM oxsum.jobs WHERE job_id = $1")
        .bind(job_id)
        .execute(db.pool())
        .await
        .unwrap();
}

#[test]
fn the_periodic_kinds_carry_their_cadence() {
    assert_eq!(next_run(kinds::SWEEP_HOLDS), Some(Duration::seconds(60)));
    assert_eq!(
        next_run(kinds::DELIVER_WEBHOOKS),
        Some(Duration::seconds(30))
    );
    assert_eq!(next_run(kinds::RECONCILE), Some(Duration::days(1)));
    assert_eq!(next_run(kinds::STATEMENTS), Some(Duration::days(1)));
    assert_eq!(next_run(kinds::RETENTION), Some(Duration::days(1)));
    assert_eq!(next_run(kinds::PLAN_RECOGNITION), Some(Duration::hours(1)));
    assert_eq!(next_run("not-a-kind"), None);
}

#[tokio::test]
async fn dead_runs_surface_as_jobs_dead_drift() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    let kind = kind();
    let job_id = enqueued(&db, &kind).await;
    sqlx::query(
        "UPDATE oxsum.jobs SET status = 'dead', finished_at = now(), \
         last_error = 'died in a test' WHERE job_id = $1",
    )
    .bind(job_id)
    .execute(db.pool())
    .await
    .unwrap();

    let report = db.reconcile().await.unwrap();
    let jobs_dead = report
        .classes
        .iter()
        .find(|class| format!("{:?}", class.class) == "JobsDead")
        .expect("the class is in the fixed order");
    assert!(jobs_dead.count >= 1);
    let sample = jobs_dead
        .samples
        .iter()
        .find(|s| s.detail == format!("{kind}:{job_id}"))
        .expect("the dead run is sampled");
    assert_eq!(sample.organization, "platform");
    sqlx::query("DELETE FROM oxsum.jobs WHERE job_id = $1")
        .bind(job_id)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn the_retention_pass_ages_out_each_rule_once() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, org, tenant_id) = world(&url).await;

    // An expired idempotency record and a live one.
    for (key, expires) in [
        ("old-claim", OffsetDateTime::now_utc() - Duration::days(1)),
        ("live-claim", OffsetDateTime::now_utc() + Duration::days(1)),
    ] {
        sqlx::query(
            "INSERT INTO oxsum.idempotency_records \
             (organization_id, idempotency_key, request_fingerprint, request_id, status, expires_at) \
             VALUES ($1, $2, 'fp', $3, 'completed', $4)",
        )
        .bind(org)
        .bind(format!("{key}-{}", Uuid::new_v4()))
        .bind(Uuid::new_v4().to_string())
        .bind(expires)
        .execute(db.pool())
        .await
        .unwrap();
    }

    // A usage row whose provider_raw is past the privacy window.
    let mut usage = UsageRecord::tokens(10, 2).unwrap();
    usage.usage_details = Some(json!({"provider_raw": {"raw": true}}));
    let request_id = format!("retention-{}", Uuid::new_v4());
    db.record_usage(&UsageRow {
        request_id: request_id.clone(),
        tenant_id: tenant_id.clone(),
        key_id: None,
        mandate_id: None,
        model: "m".to_owned(),
        channel: "c".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: Uuid::new_v4(),
        status: "settled".to_owned(),
        failure_reason: None,
        usage: usage.clone(),
        counts: oxsum_core::CountsProvenance::Provider {
            input: usage.input_tokens,
            output: usage.output_tokens,
        },
        charged_minor: 12,
        freeze_minor: 100,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 1,
    })
    .await
    .unwrap();
    sqlx::query("UPDATE oxsum.usage_records SET settled_at = now() - interval '30 days' WHERE request_id = $1")
        .bind(&request_id)
        .execute(db.pool())
        .await
        .unwrap();

    // A delivered webhook row past its window — and a still-pending one.
    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["request.settled".to_owned()],
            &SecretKey::from_bytes([9; 32]),
        )
        .await
        .unwrap()
        .endpoint
        .id;
    for (status, marker) in [("delivered", "old"), ("pending", "live")] {
        sqlx::query(
            "INSERT INTO oxsum.webhook_deliveries \
             (delivery_id, endpoint_id, organization_id, event_type, payload, status, \
              next_attempt_at, delivered_at) \
             VALUES ($1, $2, $3, 'request.settled', '{}', $4, $5, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(endpoint)
        .bind(org)
        .bind(status)
        .bind(OffsetDateTime::now_utc() - Duration::days(if marker == "old" { 60 } else { 0 }))
        .execute(db.pool())
        .await
        .unwrap();
    }

    // A finished job row past its window, beside a recent one.
    let kind = kind();
    let old_job = enqueued(&db, &kind).await;
    sqlx::query(
        "UPDATE oxsum.jobs SET status = 'done', finished_at = now() - interval '120 days' \
         WHERE job_id = $1",
    )
    .bind(old_job)
    .execute(db.pool())
    .await
    .unwrap();

    let report = db.run_retention(&Retention::default()).await.unwrap();
    assert!(report.idempotency_records >= 1);
    assert!(report.provider_raw_nulled >= 1);
    assert!(report.webhook_deliveries >= 1);
    assert!(report.jobs >= 1);

    // Each rule removed exactly what its window says, once.
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.idempotency_records WHERE organization_id = $1",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(remaining, 1);
    let raw: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT usage_details -> 'provider_raw' FROM oxsum.usage_records WHERE request_id = $1",
    )
    .bind(&request_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(raw.is_none());
    let deliveries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.webhook_deliveries WHERE organization_id = $1 AND status = 'delivered'",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(deliveries, 0);
    let old_job_left: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.jobs WHERE job_id = $1")
        .bind(old_job)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(old_job_left, 0);
}

// --- Leader election (issue #281) ---

#[test]
fn the_leader_required_kinds_are_the_singleton_passes() {
    use oxsum_core::requires_leader;
    // The kinds a duplicated pass would hurt: sweep, month-end, and every
    // outbound pass. `deliver-webhooks` stays unleadered — a second runner is
    // throughput, not duplication, because its rows are claimed one by one.
    assert!(requires_leader(kinds::SWEEP_HOLDS));
    assert!(requires_leader(kinds::WITNESS_CHECKPOINTS));
    assert!(requires_leader(kinds::STATEMENTS));
    assert!(requires_leader(kinds::RETENTION));
    assert!(requires_leader(kinds::SYNC_UPSTREAM_PRICES));
    assert!(requires_leader(kinds::RECONCILE));
    assert!(!requires_leader(kinds::DELIVER_WEBHOOKS));
    assert!(!requires_leader("not-a-kind"));
}

#[tokio::test]
async fn an_enqueued_row_inherits_the_kinds_leader_flag() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;

    // Clear any pending reconcile the suite's own worker enqueued — the
    // singleton index would make this enqueue a no-op.
    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = 'reconcile'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.enqueue_job(kinds::RECONCILE, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    let flagged: bool = sqlx::query_scalar(
        "SELECT requires_leader FROM oxsum.jobs WHERE kind = 'reconcile' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(flagged);
    // The claim reads the flag back onto the row the worker sees.
    let job = db
        .claim_due_jobs(64)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.kind == kinds::RECONCILE)
        .expect("claimed");
    assert!(job.requires_leader);
    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = 'reconcile'")
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn the_leader_lock_excludes_a_second_holder_until_released() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    let kind = kind();

    // The lock is session-scoped: two connections stand in for two instances.
    let mut first = db.pool().acquire().await.unwrap();
    assert!(Db::try_leader_lock(&mut first, &kind).await.unwrap());

    let mut second = db.pool().acquire().await.unwrap();
    assert!(!Db::try_leader_lock(&mut second, &kind).await.unwrap());
    // The contender releases nothing.
    assert!(!Db::leader_unlock(&mut second, &kind).await.unwrap());

    // Releasing on the holder's connection frees the kind for the next claim.
    assert!(Db::leader_unlock(&mut first, &kind).await.unwrap());
    assert!(Db::try_leader_lock(&mut second, &kind).await.unwrap());
    assert!(Db::leader_unlock(&mut second, &kind).await.unwrap());
}

#[tokio::test]
async fn a_yielded_row_returns_to_pending_without_spending_an_attempt() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    let kind = kind();
    let job_id = enqueued(&db, &kind).await;

    let job = db
        .claim_due_jobs(4)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("claimed");
    assert_eq!(job.attempts, 0);

    // The yield postpones the row: not due now, pending again, attempt unspent.
    assert!(
        db.unclaim_job(job.job_id, job.claimed_at, Duration::seconds(60))
            .await
            .unwrap()
    );
    assert_eq!(status_of(&db, job_id).await, "pending");
    let (attempts, run_at): (i32, OffsetDateTime) =
        sqlx::query_as("SELECT attempts, run_at FROM oxsum.jobs WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(attempts, 0);
    assert!(run_at > OffsetDateTime::now_utc() + Duration::seconds(30));

    // Fast-forward past the postponement: the row claims and finishes normally.
    sqlx::query("UPDATE oxsum.jobs SET run_at = now() WHERE job_id = $1")
        .bind(job_id)
        .execute(db.pool())
        .await
        .unwrap();
    let job = db
        .claim_due_jobs(4)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.job_id == job_id)
        .expect("reclaimed after the yield");
    assert!(db.finish_job(job.job_id, job.claimed_at).await.unwrap());
    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = $1")
        .bind(&kind)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_reclaimed_leader_row_yields_while_the_lock_is_held() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;

    // Simulate the mid-flight scenario the lock exists for: a leader-required
    // row reclaimed after its lease lapsed while the kind's lock is still held
    // on another connection (the first worker is slow, not dead).
    let mut leader = db.pool().acquire().await.unwrap();
    assert!(
        Db::try_leader_lock(&mut leader, kinds::RETENTION)
            .await
            .unwrap()
    );

    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = 'retention'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.enqueue_job(kinds::RETENTION, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
    let job = db
        .claim_due_jobs(64)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.kind == kinds::RETENTION)
        .expect("claimed");
    assert!(job.requires_leader);

    // The reclaiming worker cannot take the lock — it yields the row back.
    let mut contender = db.pool().acquire().await.unwrap();
    assert!(
        !Db::try_leader_lock(&mut contender, job.kind.as_str())
            .await
            .unwrap()
    );
    assert!(
        db.unclaim_job(job.job_id, job.claimed_at, Duration::seconds(60))
            .await
            .unwrap()
    );

    // Once the leader releases, the row claims and runs under the lock.
    assert!(
        Db::leader_unlock(&mut leader, kinds::RETENTION)
            .await
            .unwrap()
    );
    sqlx::query("UPDATE oxsum.jobs SET run_at = now() WHERE kind = 'retention'")
        .execute(db.pool())
        .await
        .unwrap();
    let job = db
        .claim_due_jobs(64)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.kind == kinds::RETENTION)
        .expect("reclaimed after the leader finished");
    assert!(
        Db::try_leader_lock(&mut contender, job.kind.as_str())
            .await
            .unwrap()
    );
    assert!(
        Db::leader_unlock(&mut contender, job.kind.as_str())
            .await
            .unwrap()
    );
    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = 'retention'")
        .execute(db.pool())
        .await
        .unwrap();
}

// --- The manual close-month run (issue #1090) ---

/// Removes every close-month row: the kind holds one pending or running run at
/// a time, so a leftover would make the next enqueue a same-slot conflict.
async fn clear_close_month(db: &Db) {
    sqlx::query("DELETE FROM oxsum.jobs WHERE kind = 'close-month'")
        .execute(db.pool())
        .await
        .unwrap();
}

/// The months immediately before the running one — `prev` is the newest
/// closeable, `earlier` the month behind it, as `YYYY-MM` labels.
fn ended_months() -> (String, String) {
    let today = OffsetDateTime::now_utc().date();
    let label = |date: time::Date| format!("{:04}-{:02}", date.year(), u8::from(date.month()));
    let previous_first = today.replace_day(1).unwrap() - Duration::days(1);
    let earlier_first = previous_first.replace_day(1).unwrap() - Duration::days(1);
    (label(previous_first), label(earlier_first))
}

/// The first of a `YYYY-MM` label as a date — the worker's `month_first`.
fn first_of(month: &str) -> time::Date {
    let (year, month) = month.split_once('-').unwrap();
    time::Date::from_calendar_date(
        year.parse().unwrap(),
        time::Month::try_from(month.parse::<u8>().unwrap()).unwrap(),
        1,
    )
    .unwrap()
}

#[tokio::test]
async fn a_close_month_run_answers_its_progress_and_gates_a_second_month() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    clear_close_month(&db).await;
    let (month, earlier) = ended_months();
    let period = |m: &str| oxsum_core::statement_period(m).unwrap();

    // The first queue claims the slot; a retry of the same month names the
    // same run rather than a duplicate.
    let queued = db.enqueue_close_month(&period(&month)).await.unwrap();
    assert!(queued.queued);
    let retry = db.enqueue_close_month(&period(&month)).await.unwrap();
    assert!(!retry.queued);
    assert_eq!(retry.job_id, queued.job_id);

    // A different month cannot queue while one is in flight — the conflict
    // names the month holding the slot and the refused one.
    match db.enqueue_close_month(&period(&earlier)).await {
        Err(WalletError::Conflict(message)) => {
            assert!(message.contains(&month), "{message}");
            assert!(message.contains(&earlier), "{message}");
        }
        other => panic!("a different month in flight conflicts, got {other:?}"),
    }

    // The queued row reads back: pending, no attempt yet, empty progress.
    let job = db
        .close_month_job(queued.job_id)
        .await
        .unwrap()
        .expect("the queued run answers");
    assert_eq!(job.month, month);
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.attempts, 0);
    assert_eq!(job.progress, CloseMonthProgress::default());
    assert!(job.finished_at.is_none());

    // An id that names no close-month run is invisible: unknown, and another
    // kind's row alike.
    assert!(db.close_month_job(Uuid::new_v4()).await.unwrap().is_none());
    let other = enqueued(&db, "deliver-webhooks").await;
    assert!(db.close_month_job(other).await.unwrap().is_none());
    sqlx::query("DELETE FROM oxsum.jobs WHERE job_id = $1")
        .bind(other)
        .execute(db.pool())
        .await
        .unwrap();

    // Finished, the run keeps answering — done, stamped — and the slot frees.
    sqlx::query("UPDATE oxsum.jobs SET status = 'done', finished_at = now() WHERE job_id = $1")
        .bind(queued.job_id)
        .execute(db.pool())
        .await
        .unwrap();
    let job = db
        .close_month_job(queued.job_id)
        .await
        .unwrap()
        .expect("a finished run still answers");
    assert_eq!(job.status, JobStatus::Done);
    assert!(job.finished_at.is_some());
    assert!(
        db.enqueue_close_month(&period(&earlier))
            .await
            .unwrap()
            .queued,
        "the earlier month queues once the slot frees"
    );
    clear_close_month(&db).await;
}

#[tokio::test]
async fn a_close_month_pass_records_each_organizations_outcome() {
    let _guard = JOBS_LOCK.lock().await;
    let url = db_or_skip!();
    // Two registrations on the same suite database share one `Db` — the second
    // adds the healthy organization next to the victim.
    let (db, victim, victim_tenant) = world(&url).await;
    let (_, _, _) = world(&url).await;
    clear_close_month(&db).await;
    let (month, earlier) = ended_months();
    let tenants = Tenants::new(db.pool().clone());
    let today = OffsetDateTime::now_utc().date();

    // Two organizations, each with a dated entry so the earlier month's seal
    // has a trial balance to commit.
    for organization in db.organizations().await.unwrap() {
        tenants
            .get(&organization.tenant_id)
            .await
            .unwrap()
            .top_up("seed", 1_000_000, today)
            .await
            .unwrap();
    }
    let count = db.organizations().await.unwrap().len();
    assert!(count >= 2, "the pass needs at least two organizations");

    // The victim seals the newest closeable month first: the calendar then
    // refuses the earlier one — sealing runs in date order.
    tenants
        .get(&victim_tenant)
        .await
        .unwrap()
        .close_month(first_of(&month))
        .await
        .unwrap();

    let queued = db
        .enqueue_close_month(&oxsum_core::statement_period(&earlier).unwrap())
        .await
        .unwrap();
    let claimed_at: OffsetDateTime = sqlx::query_scalar(
        "UPDATE oxsum.jobs SET status = 'running', claimed_at = now() \
         WHERE job_id = $1 RETURNING claimed_at",
    )
    .bind(queued.job_id)
    .fetch_one(db.pool())
    .await
    .unwrap();

    // A stale claim stamp writes nothing: the pass fails fenced at its first
    // progress write, and no organization's month sealed.
    assert!(matches!(
        db.close_month_organizations(
            &tenants,
            first_of(&earlier),
            queued.job_id,
            claimed_at - Duration::seconds(1)
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    let sealed_after_stale: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM ledger_{victim_tenant}.seals WHERE period_id = $1"
    ))
    .bind(&earlier)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(sealed_after_stale, 0);

    // The real claim walks every organization: each is sealed or named a
    // failure, and the progress row carries the same counts the return does.
    let progress = db
        .close_month_organizations(&tenants, first_of(&earlier), queued.job_id, claimed_at)
        .await
        .unwrap();
    assert_eq!(progress.processed, progress.organizations);
    assert_eq!(
        progress.organizations,
        i64::try_from(count).unwrap(),
        "every organization the database holds is walked"
    );
    assert_eq!(progress.sealed + progress.failed, progress.organizations);
    assert!(progress.sealed >= 1);
    assert_eq!(progress.failed, 1, "only the victim fails");

    // The failure names the organization and the calendar's refusal — no
    // tenant id or schema spelling ever reaches the stored reason.
    let failure = &progress.failures[0];
    assert_eq!(failure.organization_id, victim);
    assert!(failure.reason.contains(&earlier), "{}", failure.reason);
    assert!(
        failure.reason.contains("sealed watermark"),
        "{}",
        failure.reason
    );
    assert!(!failure.reason.contains(&victim_tenant));
    assert!(!failure.reason.contains("ledger_"));

    // The healthy ledger really did seal the earlier month.
    let healthy = db
        .organizations()
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.id != victim)
        .expect("a second organization");
    let seals = tenants
        .get(&healthy.tenant_id)
        .await
        .unwrap()
        .seals()
        .await
        .unwrap();
    assert!(
        seals.iter().any(|s| s.period.to_string() == earlier),
        "the earlier month sealed in the healthy ledger"
    );

    // The job endpoint answers the attempt's stored progress verbatim.
    let job = db
        .close_month_job(queued.job_id)
        .await
        .unwrap()
        .expect("the run answers");
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(job.progress, progress);
    clear_close_month(&db).await;
}
