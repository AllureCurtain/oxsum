//! The durable periodic-work table (issue #166): claiming, fencing, retries,
//! the dead-letter state, the retention pass, the statements aggregation pass,
//! and the `jobs_dead` drift class. Needs DATABASE_URL, skips without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    Db, JobOutcome, MAX_ATTEMPTS, NewUser, Retention, SecretKey, SettlementKind, UsageRecord,
    UsageRow, kinds, next_run,
};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Claims take whatever is due regardless of kind, so two tests running at
/// once would claim each other's rows: the whole suite runs serially.
static JOBS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! db_or_skip {
    () => {
        match url() {
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
        model: "m".to_owned(),
        channel: "c".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: Uuid::new_v4(),
        usage,
        charged_minor: 12,
        freeze_minor: 100,
        upstream_cost_minor: None,
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
