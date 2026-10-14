//! The jobs worker (issue #166): `run_due` claims due rows off `oxsum.jobs`,
//! dispatches each kind's pass, and chains the next occurrence — the loop
//! `spawn_worker` wraps. Needs DATABASE_URL and skips without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use oxsum_core::{
    Db, OpenHold, Retention, SecretKey, Tenants, UpstreamPrices, Wallet, entry_id_for,
    hold_description, kinds, settlement_key_for,
};
use oxsum_server::Metrics;
use oxsum_server::jobs::{JobContext, run_due};
use serde_json::Value;
use sqlx::PgPool;
use time::OffsetDateTime;
use time::macros::date;
use uuid::Uuid;

/// The posting date of the test writes, like the server's `today()`.
const D: time::Date = date!(2026 - 10 - 03);

/// Claims take whatever is due regardless of kind, so two tests running at
/// once would claim each other's rows: the suite runs serially.
static JOBS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("jobs")
}

fn fresh(name: &str) -> String {
    format!("{name}_{}", &Uuid::new_v4().simple().to_string()[..8])
}

struct World {
    db: Db,
    tenant: String,
    wallet: Arc<Wallet>,
}

async fn world(url: &str) -> World {
    let pool = PgPool::connect(url).await.expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    // Nothing else claims the table between runs of this suite, but a crashed
    // earlier run may have left a pending row a claim would pick up.
    sqlx::query("DELETE FROM oxsum.jobs")
        .execute(db.pool())
        .await
        .expect("clears leftover runs");
    let tenants = Tenants::new(pool);
    let tenant = fresh("jobs");
    let wallet = tenants.get(&tenant).await.expect("opens the ledger");
    wallet
        .top_up("top-1", 10_000, D)
        .await
        .expect("funds the wallet");
    World { db, tenant, wallet }
}

fn ctx() -> JobContext {
    JobContext {
        hold_timeout: Duration::from_secs(60),
        metrics: Metrics::new(),
        webhook: None,
        retention: Retention::default(),
        upstream_price_sync: false,
        witness: None,
    }
}

/// A due row of `kind`, enqueued the way the startup pass does.
async fn enqueue(db: &Db, kind: &str) {
    assert!(
        db.enqueue_job(kind, OffsetDateTime::now_utc())
            .await
            .unwrap()
    );
}

/// The row counts of `kind` per status — `(pending, running, done, dead)`.
async fn counts(db: &Db, kind: &str) -> (i64, i64, i64, i64) {
    let row = sqlx::query(
        "SELECT \
             count(*) FILTER (WHERE status = 'pending'), \
             count(*) FILTER (WHERE status = 'running'), \
             count(*) FILTER (WHERE status = 'done'), \
             count(*) FILTER (WHERE status = 'dead') \
         FROM oxsum.jobs WHERE kind = $1",
    )
    .bind(kind)
    .fetch_one(db.pool())
    .await
    .expect("counts the kind's rows");
    use sqlx::Row;
    (
        row.get::<i64, _>(0),
        row.get::<i64, _>(1),
        row.get::<i64, _>(2),
        row.get::<i64, _>(3),
    )
}

/// A watched hold aged past the context's timeout — the sweeper's prey.
async fn stale_hold(world: &World) -> String {
    let request_id = fresh("jobs-sweep");
    let hold_key = format!("req-{request_id}:hold");
    world
        .db
        .note_open_hold(&OpenHold {
            hold_key: hold_key.clone(),
            tenant_id: world.tenant.clone(),
            request_id: request_id.clone(),
            model: "test-model".to_owned(),
            channel: "test-channel".to_owned(),
            price_version: 1,
            input_price: 1_000_000,
            output_price: 1_000_000,
            freeze_minor: 100,
            key_id: None,
            end_user: None,
            service_tier: None,
            tags: Default::default(),
            sweep_attempts: 0,
            last_error: None,
            dead_at: None,
        })
        .await
        .expect("watches the hold");
    let description = hold_description(&request_id, "test-model", 100)
        .expect("the test's own hold record builds");
    world
        .wallet
        .hold(&hold_key, &description, 100, D)
        .await
        .expect("takes the hold");
    sqlx::query("UPDATE oxsum.open_holds SET opened_at = $1 WHERE hold_key = $2")
        .bind(OffsetDateTime::now_utc() - Duration::from_secs(120))
        .bind(&hold_key)
        .execute(world.db.pool())
        .await
        .expect("ages the watch row");
    hold_key
}

/// The turn's settlement record once one exists.
async fn settlement_record(wallet: &Arc<Wallet>, hold_key: &str) -> Option<Value> {
    let entry_id = entry_id_for(&settlement_key_for(hold_key));
    let bundle = wallet
        .receipt_proof(entry_id, None)
        .await
        .expect("the proof")?;
    Some(
        serde_json::from_str(bundle.entry.description().as_str()).expect("the description is JSON"),
    )
}

#[tokio::test]
async fn a_sweep_run_resolves_stale_holds_and_chains_the_next() {
    let _guard = JOBS_LOCK.lock().await;
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url).await;
    let hold_key = stale_hold(&world).await;
    let ctx = ctx();
    enqueue(&world.db, kinds::SWEEP_HOLDS).await;

    run_due(&world.db, &ctx).await;

    // The watch row is gone and the ledger carries a `swept` settlement at 0.
    let watched: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oxsum.open_holds WHERE hold_key = $1)")
            .bind(&hold_key)
            .fetch_one(world.db.pool())
            .await
            .unwrap();
    assert!(!watched);
    let record = settlement_record(&world.wallet, &hold_key)
        .await
        .expect("the swept settlement was written");
    assert_eq!(record["kind"], "swept");
    assert_eq!(record["charged"], 0);

    // The run finished and the kind's next occurrence is already scheduled.
    let (pending, running, done, dead) = counts(&world.db, kinds::SWEEP_HOLDS).await;
    assert_eq!((pending, running, done, dead), (1, 0, 1, 0));
    let next_run_at: OffsetDateTime =
        sqlx::query_scalar("SELECT run_at FROM oxsum.jobs WHERE kind = $1 AND status = 'pending'")
            .bind(kinds::SWEEP_HOLDS)
            .fetch_one(world.db.pool())
            .await
            .unwrap();
    assert!(next_run_at > OffsetDateTime::now_utc());
}

#[tokio::test]
async fn a_failing_run_retries_then_dies_and_the_kind_recovers() {
    let _guard = JOBS_LOCK.lock().await;
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url).await;
    let ctx = ctx();
    // `deliver-webhooks` with no sealing key configured fails every pass — the
    // deployment's own fault shape, exercised through the whole chain.
    enqueue(&world.db, kinds::DELIVER_WEBHOOKS).await;

    for attempt in 1..=5 {
        // The backoff's next due is in the future; the test fast-forwards it.
        sqlx::query(
            "UPDATE oxsum.jobs SET run_at = now() \
             WHERE kind = $1 AND status = 'pending'",
        )
        .bind(kinds::DELIVER_WEBHOOKS)
        .execute(world.db.pool())
        .await
        .unwrap();
        run_due(&world.db, &ctx).await;
        let (pending, _, done, dead) = counts(&world.db, kinds::DELIVER_WEBHOOKS).await;
        if attempt < 5 {
            assert_eq!((pending, done, dead), (1, 0, 0), "attempt {attempt}");
        } else {
            // The dead row stands as drift evidence; the cooldown's row keeps
            // the chain alive rather than holding the kind hostage.
            assert_eq!((pending, done, dead), (1, 0, 1), "attempt {attempt}");
        }
    }
}

#[tokio::test]
async fn an_unknown_kind_fails_the_run_not_the_worker() {
    let _guard = JOBS_LOCK.lock().await;
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url).await;
    let ctx = ctx();
    let kind = fresh("test-unknown");
    enqueue(&world.db, &kind).await;

    run_due(&world.db, &ctx).await;

    // Failed once, back on the queue for the backoff's retry — and no next
    // occurrence: an unknown kind carries no cadence.
    let (pending, running, done, dead) = counts(&world.db, &kind).await;
    assert_eq!((pending, running, done, dead), (1, 0, 0, 0));
    let error: String = sqlx::query_scalar("SELECT last_error FROM oxsum.jobs WHERE kind = $1")
        .bind(&kind)
        .fetch_one(world.db.pool())
        .await
        .unwrap();
    assert!(error.contains("unknown job kind"), "got {error:?}");
}

#[tokio::test]
async fn a_retention_run_finishes_and_chains_a_day_out() {
    let _guard = JOBS_LOCK.lock().await;
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url).await;
    let ctx = ctx();
    enqueue(&world.db, kinds::RETENTION).await;

    run_due(&world.db, &ctx).await;

    let (pending, running, done, dead) = counts(&world.db, kinds::RETENTION).await;
    assert_eq!((pending, running, done, dead), (1, 0, 1, 0));
}

/// An upstream catalog that answers `GET /api/v1/pricing` with one model whose
/// `upstream` rates disagree with what the channel stores — the live side of
/// the `SYNC_UPSTREAM_PRICES` diff (issue #222).
async fn stub_upstream_catalog() -> String {
    let app = axum::Router::new().route(
        "/api/v1/pricing",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "data": { "models": [{
                    "model": "test-model",
                    "upstream": { "inputPricePerMillion": 9, "outputPricePerMillion": 2 }
                }]}
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A due `SYNC_UPSTREAM_PRICES` run probes the channel's live catalog, opens a
/// drift row for the rate that disagrees, and records an `ok` probe outcome —
/// the run never fails on a reachable upstream.
#[tokio::test]
async fn a_price_sync_run_opens_drift_and_chains_a_day_out() {
    let _guard = JOBS_LOCK.lock().await;
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url).await;
    // The shared test database keeps this channel afterwards; every suite seals
    // channels with [7; 32] so `check_sealed_keys` under that same key still
    // passes for other tests.
    let secret = SecretKey::from_bytes([7; 32]);

    // The channel, pointed at a stub upstream whose live rate disagrees.
    let base_url = stub_upstream_catalog().await;
    let channel = fresh("chan");
    world
        .db
        .set_channel(&channel, &base_url, "sk-stub-secret", "openai", &secret)
        .await
        .unwrap();
    world
        .db
        .append_price(
            &channel,
            "test-model",
            oxsum_core::Price {
                input_price_per_million: 1,
                output_price_per_million: 2,
                max_output_tokens: 1024,
                cache_read_price_per_million: None,
                cache_write_5m_price_per_million: None,
                cache_write_1h_price_per_million: None,
                reasoning_price_per_million: None,
                cost_per_request: None,
                upstream: Some(UpstreamPrices {
                    input_price_per_million: Some(1),
                    output_price_per_million: Some(2),
                    ..UpstreamPrices::default()
                }),
                mode: oxsum_core::BillingMode::Chat,
                rules: Vec::new(),
            },
            1,
        )
        .await
        .unwrap();

    let ctx = JobContext {
        webhook: Some((reqwest::Client::new(), secret)),
        upstream_price_sync: true,
        ..ctx()
    };
    enqueue(&world.db, kinds::SYNC_UPSTREAM_PRICES).await;

    run_due(&world.db, &ctx).await;

    // The differing rate opened a drift row; the probe recorded `ok`.
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.upstream_price_drift \
         WHERE channel = $1 AND resolved_at IS NULL",
    )
    .bind(&channel)
    .fetch_one(world.db.pool())
    .await
    .unwrap();
    assert_eq!(open, 1, "inputPricePerMillion drifts 1 -> 9");
    let outcome: String = sqlx::query_scalar(
        "SELECT outcome FROM oxsum.upstream_price_probes \
         WHERE channel = $1 ORDER BY probed_at DESC LIMIT 1",
    )
    .bind(&channel)
    .fetch_one(world.db.pool())
    .await
    .unwrap();
    assert_eq!(outcome, "ok");

    // The run finished and the daily occurrence is already scheduled.
    let (pending, running, done, dead) = counts(&world.db, kinds::SYNC_UPSTREAM_PRICES).await;
    assert_eq!((pending, running, done, dead), (1, 0, 1, 0));
}

/// `witness-checkpoints` publishes each organization's signed head to the
/// configured witnesses and stores the checkpoint plus the cosigned notes the
/// witnesses answer (issue #231). The stub echoes the note back with its own
/// signature line, like a real `tlog-witness` `/add-checkpoint` endpoint.
#[tokio::test]
async fn witness_checkpoints_anchor_and_collect_cosignatures() {
    let _guard = JOBS_LOCK.lock().await;
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url).await;

    // The pass walks `oxsum.organizations`; the world's tenant gets a row.
    let organization = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO oxsum.organizations (organization_id, name, tenant_id, kind) \
         VALUES ($1, $2, $3, 'team')",
    )
    .bind(organization)
    .bind("witness-test")
    .bind(&world.tenant)
    .execute(world.db.pool())
    .await
    .unwrap();

    // A stub tlog-witness: accepts every checkpoint and answers with the note
    // plus its own signature line.
    let stub = axum::Router::new().route(
        "/add-checkpoint",
        axum::routing::post(|body: String| async move {
            let note = body.split("\n\n").next().unwrap_or_default().to_owned();
            format!("{note}\n\n— fake-witness c2ln")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let witness_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, stub).await;
    });

    let ctx = JobContext {
        witness: Some(oxsum_server::jobs::WitnessConfig {
            http: reqwest::Client::new(),
            head_signing_seed: [9; 32],
            urls: vec![witness_url.clone()],
        }),
        ..ctx()
    };
    enqueue(&world.db, kinds::WITNESS_CHECKPOINTS).await;
    run_due(&world.db, &ctx).await;

    let (head_size, note): (i64, String) = sqlx::query_as(
        "SELECT head_size, note FROM oxsum.witness_checkpoints WHERE organization_id = $1",
    )
    .bind(organization)
    .fetch_one(world.db.pool())
    .await
    .expect("the checkpoint was anchored");
    assert!(head_size > 0, "the top-up already wrote ledger entries");
    let parsed = doubleentry::witness::SignedTreeHead::parse(&note).unwrap();
    assert_eq!(parsed.head.size as i64, head_size);

    let cosig: String = sqlx::query_scalar(
        "SELECT note FROM oxsum.witness_cosignatures \
         WHERE organization_id = $1 AND witness_url = $2 AND head_size = $3",
    )
    .bind(organization)
    .bind(&witness_url)
    .bind(head_size)
    .fetch_one(world.db.pool())
    .await
    .expect("the witness's cosignature was stored");
    assert!(cosig.contains("fake-witness"), "{cosig}");

    // A second pass over an unchanged head anchors and cosigns nothing new.
    enqueue(&world.db, kinds::WITNESS_CHECKPOINTS).await;
    run_due(&world.db, &ctx).await;
    let cosigs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.witness_cosignatures WHERE organization_id = $1",
    )
    .bind(organization)
    .fetch_one(world.db.pool())
    .await
    .unwrap();
    assert_eq!(cosigs, 1, "no duplicate cosignature for an unchanged head");

    let (pending, running, done, dead) = counts(&world.db, kinds::WITNESS_CHECKPOINTS).await;
    assert_eq!((pending, running, done, dead), (1, 0, 2, 0));
}
