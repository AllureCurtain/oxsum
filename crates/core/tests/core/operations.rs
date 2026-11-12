//! The durable request operation (issue #601, roadmap T10): replay under the
//! scoped fingerprint, the lease CAS, heartbeat, terminal settle, the sweeper's
//! second pass, and the backward-compat sweep of a pre-operation watch row.
//! Needs `DATABASE_URL`, skips without it.

use oxsum_core::{
    AttemptStatus, Db, NewOperation, NewUser, OpenHold, OperationSettle, OperationState,
    SWEEPER_OWNER, Tenants, sweep_stale_holds,
};
use sqlx::PgPool;
use time::macros::date;
use time::{Date, Duration, OffsetDateTime};
use uuid::Uuid;

/// The posting date of the test writes, like the server's `today()`.
const D: Date = date!(2026 - 11 - 06);

/// The sweeper's CAS and the heartbeat are global across the scratch
/// database: parallel tests' `sweep_stale_holds` passes would claim each
/// other's lapsed operations, so the suite runs serially on one lock.
static OPS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// A migrated database, a registered organization, and a funded wallet —
/// the hold paths the sweep tests drive.
async fn world(url: &str, top_up: i64) -> (Db, Uuid, String, Tenants) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let tenants = Tenants::new(pool);
    let email = format!(
        "ops_{}@example.com",
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
    let tenant_id = registration.organization.tenant_id.clone();
    if top_up > 0 {
        let wallet = tenants.get(&tenant_id).await.expect("opens the ledger");
        wallet.top_up("top-1", top_up, D).await.expect("funds");
    }
    (db, org, tenant_id, tenants)
}

/// A unique fingerprint per test — the unique index on
/// `(organization_id, fingerprint)` makes reruns collide without it.
fn fingerprint() -> String {
    format!("fp-{}", Uuid::new_v4())
}

fn new_operation(org: Uuid, fingerprint: &str, bound: i64) -> NewOperation {
    NewOperation {
        operation_id: Uuid::new_v4(),
        organization_id: org,
        key_id: None,
        surface: "chat".to_owned(),
        fingerprint: fingerprint.to_owned(),
        bound_minor: bound,
        model: "test-model".to_owned(),
        channel: Some("test-channel".to_owned()),
        customer_id: None,
        terms_version: Some(1),
    }
}

#[tokio::test]
async fn an_operation_replays_under_the_same_fingerprint() {
    let url = db_or_skip!();
    let (db, org, _, _) = world(&url, 0).await;
    let fp = fingerprint();

    let (first, fresh) = db
        .insert_operation(&new_operation(org, &fp, 1_000))
        .await
        .expect("inserts");
    assert!(fresh, "the first insert is fresh");

    // The same fingerprint under the same organization names the same row —
    // a retry's insert loses the conflict and reads the winner's.
    let (second, fresh) = db
        .insert_operation(&new_operation(org, &fp, 1_000))
        .await
        .expect("the replay reads the row back");
    assert!(!fresh, "the replay is not fresh");
    assert_eq!(second.operation_id, first.operation_id);
}

#[tokio::test]
async fn a_changed_body_fingerprint_is_a_new_operation() {
    let url = db_or_skip!();
    let (db, org, _, _) = world(&url, 0).await;

    let (first, _) = db
        .insert_operation(&new_operation(org, &fingerprint(), 1_000))
        .await
        .expect("inserts");
    // A different body digests to a different fingerprint: the insert writes a
    // new row rather than conflicting. The 409 mapping lives at the route
    // layer; the Db's answer is "a new operation".
    let (second, fresh) = db
        .insert_operation(&new_operation(org, &fingerprint(), 1_000))
        .await
        .expect("a different fingerprint is a new row");
    assert!(fresh);
    assert_ne!(second.operation_id, first.operation_id);
}

#[tokio::test]
async fn the_lease_cas_lets_one_worker_win() {
    let url = db_or_skip!();
    let (db, org, _, _) = world(&url, 0).await;

    let (operation, _) = db
        .insert_operation(&new_operation(org, &fingerprint(), 1_000))
        .await
        .expect("inserts");
    db.bind_hold(operation.operation_id, "req-x:hold", "worker-1")
        .await
        .expect("binds");

    // Past the lease the row is claimable: two contenders, exactly one wins.
    sqlx::query(
        "UPDATE oxsum.request_operations SET leased_until = now() - interval '1 second' \
         WHERE operation_id = $1",
    )
    .bind(operation.operation_id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        db.claim_operation(operation.operation_id, SWEEPER_OWNER, 60)
            .await
            .expect("the first claim wins")
    );
    assert!(
        !db.claim_operation(operation.operation_id, "sweeper-2", 60)
            .await
            .expect("the second claim loses the CAS")
    );
}

#[tokio::test]
async fn a_heartbeat_extends_the_lease() {
    let url = db_or_skip!();
    let (db, org, _, _) = world(&url, 0).await;

    let (operation, _) = db
        .insert_operation(&new_operation(org, &fingerprint(), 1_000))
        .await
        .expect("inserts");
    db.bind_hold(operation.operation_id, "req-x:hold", "worker-1")
        .await
        .expect("binds");
    let before = db
        .operation(org, operation.operation_id)
        .await
        .expect("reads")
        .expect("the row")
        .leased_until
        .expect("the lease is stamped");

    assert!(
        db.heartbeat(operation.operation_id, "worker-1", 300)
            .await
            .expect("the owner's heartbeat lands")
    );
    let after = db
        .operation(org, operation.operation_id)
        .await
        .expect("reads")
        .expect("the row")
        .leased_until
        .expect("the lease moved");
    assert!(after > before, "the heartbeat extended the lease");

    // A foreign owner heartbeats nothing — the CAS names the holder.
    assert!(
        !db.heartbeat(operation.operation_id, "worker-2", 300)
            .await
            .expect("a foreign heartbeat is refused")
    );
}

#[tokio::test]
async fn settle_operation_is_terminal_once() {
    let url = db_or_skip!();
    let (db, org, _, _) = world(&url, 0).await;

    let (operation, _) = db
        .insert_operation(&new_operation(org, &fingerprint(), 1_000))
        .await
        .expect("inserts");
    db.bind_hold(operation.operation_id, "req-x:hold", "worker-1")
        .await
        .expect("binds");

    let first = db
        .settle_operation(
            operation.operation_id,
            "worker-1",
            OperationState::Succeeded,
            700,
            "usage",
        )
        .await
        .expect("settles");
    assert_eq!(first, OperationSettle::Settled);

    // The same writer's second settle is the raced double-settle: already
    // terminal, never an error.
    let second = db
        .settle_operation(
            operation.operation_id,
            "worker-1",
            OperationState::Failed,
            0,
            "swept",
        )
        .await
        .expect("the second settle reads the terminal state");
    assert_eq!(second, OperationSettle::AlreadySettled);

    let row = db
        .operation(org, operation.operation_id)
        .await
        .expect("reads")
        .expect("the row");
    assert_eq!(row.state, OperationState::Succeeded);
    assert_eq!(row.charged_minor, Some(700));
    assert_eq!(row.outcome_kind.as_deref(), Some("usage"));
    assert!(row.completed_at.is_some());
}

#[tokio::test]
async fn an_operation_past_its_lease_is_claimable() {
    let _guard = OPS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, org, _, _) = world(&url, 0).await;

    let (operation, _) = db
        .insert_operation(&new_operation(org, &fingerprint(), 1_000))
        .await
        .expect("inserts");
    db.bind_hold(operation.operation_id, "req-x:hold", "worker-1")
        .await
        .expect("binds");
    sqlx::query(
        "UPDATE oxsum.request_operations SET leased_until = now() - interval '1 second' \
         WHERE operation_id = $1",
    )
    .bind(operation.operation_id)
    .execute(db.pool())
    .await
    .unwrap();

    let past = db.operations_past_lease(64).await.expect("lists");
    assert!(
        past.iter()
            .any(|o| o.operation_id == operation.operation_id),
        "the lapsed row is the sweeper's working set"
    );
    assert!(
        db.claim_operation(operation.operation_id, SWEEPER_OWNER, 60)
            .await
            .expect("claims")
    );
}

#[tokio::test]
async fn the_winning_attempt_carries_the_usage() {
    let url = db_or_skip!();
    let (db, org, _, _) = world(&url, 0).await;

    let (operation, _) = db
        .insert_operation(&new_operation(org, &fingerprint(), 1_000))
        .await
        .expect("inserts");
    // Two attempts: the first refused, the second answered with the usage the
    // settle priced from — the failover audit is one row per call.
    let loser = db
        .record_attempt(operation.operation_id, "channel-a", None, None)
        .await
        .expect("records the first attempt");
    db.complete_attempt(
        loser,
        AttemptStatus::Refused,
        None,
        Some("upstream answered 429"),
    )
    .await
    .expect("completes the refusal");
    let winner = db
        .record_attempt(
            operation.operation_id,
            "channel-b",
            None,
            Some("upstream-id-9"),
        )
        .await
        .expect("records the second attempt");
    db.complete_attempt(
        winner,
        AttemptStatus::Answered,
        Some(&serde_json::json!({"prompt_tokens": 10, "completion_tokens": 5})),
        None,
    )
    .await
    .expect("completes the winner");

    let attempts = db
        .operation_attempts(operation.operation_id)
        .await
        .expect("lists");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].status, AttemptStatus::Refused);
    assert_eq!(attempts[1].status, AttemptStatus::Answered);
    assert_eq!(
        attempts[1].usage_json.as_ref().unwrap()["prompt_tokens"],
        10
    );
    assert_eq!(attempts[1].external_id.as_deref(), Some("upstream-id-9"));
}

/// A watch row written before this table existed keeps sweeping on the old
/// criterion: the sweeper's second pass does not need an operation to release
/// a stale hold.
#[tokio::test]
async fn an_old_open_hold_sweeps_without_an_operation() {
    let _guard = OPS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, _org, tenant_id, tenants) = world(&url, 1_000_000).await;
    let wallet = tenants.get(&tenant_id).await.expect("opens");

    let request_id = format!("ops-sweep-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let hold_key = format!("req-{request_id}:hold");
    let freeze = 400_000;
    db.note_open_hold(&OpenHold {
        hold_key: hold_key.clone(),
        tenant_id: tenant_id.clone(),
        request_id: request_id.clone(),
        model: "test-model".to_owned(),
        channel: "test-channel".to_owned(),
        price_version: 1,
        input_price: 1_000_000,
        output_price: 1_000_000,
        freeze_minor: freeze,
        key_id: None,
        mandate_id: None,
        end_user: None,
        service_tier: None,
        tags: Default::default(),
        sweep_attempts: 0,
        last_error: None,
        dead_at: None,
    })
    .await
    .expect("watches");
    wallet
        .hold(&hold_key, "the test's hold", freeze, D)
        .await
        .expect("takes the hold");
    sqlx::query(
        "UPDATE oxsum.open_holds SET opened_at = now() - interval '1 hour' WHERE hold_key = $1",
    )
    .bind(&hold_key)
    .execute(db.pool())
    .await
    .unwrap();

    let cutoff = OffsetDateTime::now_utc() - Duration::minutes(30);
    let report = sweep_stale_holds(&db, &tenants, cutoff, D)
        .await
        .expect("sweeps");
    assert!(report.resolved >= 1, "the sweep resolved nothing");
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM oxsum.open_holds WHERE hold_key = $1)"
        )
        .bind(&hold_key)
        .fetch_one(db.pool())
        .await
        .unwrap(),
        "the watch row is gone"
    );
}

/// The new half of the sweeper: an operation whose lease lapsed is claimed,
/// its bound hold releases through the same `swept` path, and the row goes
/// `timed_out` — the same anomaly surface a swept watch row joins.
#[tokio::test]
async fn a_lapsed_operation_sweeps_its_bound_hold() {
    let _guard = OPS_LOCK.lock().await;
    let url = db_or_skip!();
    let (db, org, tenant_id, tenants) = world(&url, 1_000_000).await;
    let wallet = tenants.get(&tenant_id).await.expect("opens");

    let (operation, _) = db
        .insert_operation(&new_operation(org, &fingerprint(), 400_000))
        .await
        .expect("inserts");
    let hold_key = format!("req-{}:hold", operation.operation_id);
    db.note_open_hold(&OpenHold {
        hold_key: hold_key.clone(),
        tenant_id: tenant_id.clone(),
        request_id: operation.operation_id.to_string(),
        model: "test-model".to_owned(),
        channel: "test-channel".to_owned(),
        price_version: 1,
        input_price: 1_000_000,
        output_price: 1_000_000,
        freeze_minor: 400_000,
        key_id: None,
        mandate_id: None,
        end_user: None,
        service_tier: None,
        tags: Default::default(),
        sweep_attempts: 0,
        last_error: None,
        dead_at: None,
    })
    .await
    .expect("watches");
    db.bind_hold(operation.operation_id, &hold_key, "worker-1")
        .await
        .expect("binds");
    wallet
        .hold(&hold_key, "the test's hold", 400_000, D)
        .await
        .expect("takes");
    sqlx::query(
        "UPDATE oxsum.request_operations SET leased_until = now() - interval '1 second' \
         WHERE operation_id = $1",
    )
    .bind(operation.operation_id)
    .execute(db.pool())
    .await
    .unwrap();

    let cutoff = OffsetDateTime::now_utc() - Duration::minutes(30);
    sweep_stale_holds(&db, &tenants, cutoff, D)
        .await
        .expect("sweeps");

    let row = db
        .operation(org, operation.operation_id)
        .await
        .expect("reads")
        .expect("the row");
    assert_eq!(row.state, OperationState::TimedOut);
    assert_eq!(row.charged_minor, Some(0));
    assert_eq!(row.outcome_kind.as_deref(), Some("swept"));
    assert_eq!(wallet.reserved().await.unwrap(), 0);
}
