//! The hold sweeper: a watched hold older than the timeout is settled at 0 with kind
//! `swept`, and a late settlement racing the sweep cannot take effect alongside it.
//!
//! The tests need DATABASE_URL and skip without it, like the other suites. They drive
//! [`oxsum_core::sweep_stale_holds`] directly — the background loop in `main.rs` only wraps it
//! with a cutoff of `now - timeout` — and the gateway wiring (a turn noting and clearing its
//! watch row) is covered in `gateway.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use oxsum_core::{
    BillLine, Db, OpenHold, Settlement, SettlementKind, Tenants, UsageRecord, Wallet, WalletError,
    entry_id_for, hold_description, settlement_key_for, sweep_stale_holds,
};
use serde_json::Value;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use time::macros::date;

/// The posting date of the test writes, like the server's `today()`.
const D: time::Date = date!(2026 - 10 - 03);

/// Each test uses a tenant name with a random suffix, so tests never interfere and reruns never
/// collide.
fn fresh(name: &str) -> String {
    format!("{name}_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

/// A funded wallet behind one pool, or an early return when there is no database.
macro_rules! world {
    ($top_up:expr) => {
        match url() {
            Some(url) => world_for(&url, $top_up).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

struct World {
    db: Db,
    tenants: Tenants,
    tenant: String,
    wallet: Arc<Wallet>,
}

async fn world_for(url: &str, top_up: i64) -> World {
    let pool = PgPool::connect(url).await.expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let tenants = Tenants::new(pool);
    let tenant = fresh("sweep");
    let wallet = tenants.get(&tenant).await.expect("opens the ledger");
    wallet
        .top_up("top-1", top_up, D)
        .await
        .expect("funds the wallet");
    World {
        db,
        tenants,
        tenant,
        wallet,
    }
}

/// A watch row and its hold key for one request, the way the gateway notes them. The request
/// id carries a random suffix: the watch table is global, so reruns must not collide with rows
/// a previous run left behind.
fn watch(tenant: &str, n: u64, freeze: i64) -> (String, String, OpenHold) {
    let request_id = format!(
        "sweep-test-{n}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let hold_key = format!("req-{request_id}:hold");
    let hold = OpenHold {
        hold_key: hold_key.clone(),
        tenant_id: tenant.to_owned(),
        request_id: request_id.clone(),
        model: "test-model".to_owned(),
        channel: "test-channel".to_owned(),
        price_version: 1,
        input_price: 1_000_000,
        output_price: 1_000_000,
        freeze_minor: freeze,
        key_id: None,
        end_user: None,
        service_tier: None,
        tags: Default::default(),
        sweep_attempts: 0,
        last_error: None,
        dead_at: None,
    };
    (hold_key, request_id, hold)
}

/// Takes a hold the way the gateway does: the watch row first, then the ledger hold.
async fn take_hold(world: &World, hold_key: &str, request_id: &str, watch: &OpenHold, freeze: i64) {
    world
        .db
        .note_open_hold(watch)
        .await
        .expect("watches the hold");
    let description = hold_description(request_id, "test-model", freeze)
        .expect("the test's own hold record builds");
    world
        .wallet
        .hold(hold_key, &description, freeze, D)
        .await
        .expect("takes the hold");
}

/// The settlement entry's description for a hold, verbatim — the bytes
/// `verify_charge` recomputes.
async fn settlement_description(wallet: &Arc<Wallet>, hold_key: &str) -> Option<String> {
    let entry_id = entry_id_for(&settlement_key_for(hold_key));
    let bundle = wallet
        .receipt_proof(entry_id)
        .await
        .expect("the proof is built")?;
    Some(bundle.entry.description().as_str().to_owned())
}

/// The settlement entry's record for a hold, once it exists.
async fn settlement_record(wallet: &Arc<Wallet>, hold_key: &str) -> Option<Value> {
    settlement_description(wallet, hold_key)
        .await
        .map(|text| serde_json::from_str(&text).expect("the description is JSON"))
}

/// Serialises the test sweepers across test binaries: production runs one sweeper, so the
/// tests take an advisory lock around aging and sweeping, and no two test sweepers run at once.
///
/// The lock orders sweepers; it says nothing about the rows they find. `oxsum.open_holds` is
/// shared with every other world and every other run against this database, and
/// [`sweep_stale_holds`] counts every world's rows — a row another world left stale is resolved
/// here too, which is the sweeper working rather than a failure. A test therefore asserts on the
/// row it owns, never on the count (issue #34).
const SWEEP_LOCK: i64 = i64::from_be_bytes(*b"oxsumswp");

/// Takes the sweep lock, returning the connection that holds it. A previous test that failed
/// while holding the lock fails this one loudly instead of hanging the suite.
async fn lock_sweeper(pool: &PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    let mut conn = pool
        .acquire()
        .await
        .expect("a connection for the sweep lock");
    // Three minutes, not one: the dead-letter test holds the lock through ten
    // failing sweeps, which alone runs past a minute.
    for _ in 0..1_800 {
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(SWEEP_LOCK)
            .fetch_one(&mut *conn)
            .await
            .expect("tries the sweep lock");
        if locked {
            return conn;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("could not take the sweep lock within three minutes");
}

/// Releases the sweep lock.
async fn unlock_sweeper(mut conn: sqlx::pool::PoolConnection<sqlx::Postgres>) {
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(SWEEP_LOCK)
        .execute(&mut *conn)
        .await
        .expect("releases the sweep lock");
}

/// A cutoff that only matches rows the test aged on purpose. Young rows — a hold whose
/// request is still running — are never stale under it.
fn stale_cutoff() -> OffsetDateTime {
    OffsetDateTime::now_utc() - Duration::from_secs(30 * 60)
}

/// Makes a watch row look `ago` old. The sweeper only sees aged rows, which keeps the parallel
/// tests from sweeping each other's holds: staleness is global, so the tests scope it with age.
async fn age_row(db: &Db, hold_key: &str, ago: Duration) {
    let opened_at = OffsetDateTime::now_utc() - ago;
    sqlx::query("UPDATE oxsum.open_holds SET opened_at = $1 WHERE hold_key = $2")
        .bind(opened_at)
        .bind(hold_key)
        .execute(db.pool())
        .await
        .expect("ages the watch row");
}

/// Whether the watch table still holds a row for this key.
async fn is_watched(db: &Db, hold_key: &str) -> bool {
    let watched: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oxsum.open_holds WHERE hold_key = $1)")
            .bind(hold_key)
            .fetch_one(db.pool())
            .await
            .expect("checks the watch row");
    watched
}

/// The turn's own settlement record, for the race: usage priced at the watched version.
fn usage_settlement(request_id: &str, charged: i64, freeze: i64) -> String {
    let usage = UsageRecord::tokens(10, 5).expect("the test counts are valid");
    let lines = [
        BillLine {
            item: "input".to_owned(),
            units: usage.input_tokens,
            price_per_m: 1_000_000,
        },
        BillLine {
            item: "output".to_owned(),
            units: usage.output_tokens,
            price_per_m: 1_000_000,
        },
    ];
    Settlement {
        request: request_id,
        channel: "test-channel",
        model: "test-model",
        price_version: 1,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged,
        freeze,
        upstream_attempts: 1,
    }
    .description()
    .expect("the test's own record builds")
}

#[tokio::test]
async fn a_stale_hold_is_swept_at_zero_and_recorded_as_an_anomaly() {
    let world = world!(1_000_000);
    let sweeper = lock_sweeper(world.db.pool()).await;
    let freeze = 400_000;
    let (hold_key, request_id, mut watch) = watch(&world.tenant, 1, freeze);
    // The caller's attribution rides the watch row, so a swept turn's usage row still
    // says whose turn it was.
    watch.end_user = Some("u_7".to_owned());
    watch.service_tier = Some("flex".to_owned());
    watch.tags.insert("team".to_owned(), "search".to_owned());
    take_hold(&world, &hold_key, &request_id, &watch, freeze).await;
    assert_eq!(world.wallet.available().await.unwrap(), 600_000);
    age_row(&world.db, &hold_key, Duration::from_secs(3600)).await;

    let report = sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D)
        .await
        .expect("sweeps");
    // This world's row is among those resolved. The count may also cover stale rows other worlds
    // left behind, which is correct, so it is not asserted exactly.
    assert!(report.resolved >= 1, "the sweep resolved nothing at all");

    // The whole freeze is released...
    assert_eq!(world.wallet.reserved().await.unwrap(), 0);
    assert_eq!(world.wallet.available().await.unwrap(), 1_000_000);
    // ...and the settlement says what happened, in the record's own words: the `swept` kind is
    // what the admin page will later list as an anomaly.
    let record = settlement_record(&world.wallet, &hold_key)
        .await
        .expect("the sweep wrote a settlement");
    assert_eq!(record["kind"], "swept");
    assert_eq!(record["charged"], 0);
    // Zero usage writes no dimensions: the sparse usage object is empty, and the
    // priced lines count zero units.
    assert_eq!(record["usage"], serde_json::json!({}));
    assert_eq!(record["lines"][0][1], 0);
    assert_eq!(record["lines"][1][1], 0);
    assert_eq!(record["freeze"], freeze);
    assert_eq!(record["request"], request_id);
    // The swept record recomputes too: zero lines, zero charge, capped by the freeze.
    let description = settlement_description(&world.wallet, &hold_key)
        .await
        .expect("the sweep wrote a settlement");
    assert_eq!(
        oxsum_core::verify_charge(&description),
        oxsum_core::ChargeCheck::Recomputed
    );
    // A second pass finds nothing: the row is gone, and nothing can have aged a new one — only a
    // test ages rows, and this one still holds the sweep lock.
    let report = sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D)
        .await
        .expect("sweeps again");
    assert_eq!(report.resolved, 0);
    assert!(!is_watched(&world.db, &hold_key).await);

    // The swept turn still left its usage row: zero counts and zero charge, but the
    // attribution the watch row carried (issue #102). Its upstream cost is NULL —
    // the watch row carries no price, so the turn is untracked, not free (issue #112).
    let row = sqlx::query(
        "SELECT kind, charged_minor, freeze_minor, input_tokens, output_tokens, \
         end_user, service_tier, tags, entry_id, upstream_cost_minor \
         FROM oxsum.usage_records WHERE request_id = $1",
    )
    .bind(&request_id)
    .fetch_one(world.db.pool())
    .await
    .expect("the swept turn left its usage row");
    assert_eq!(row.get::<String, _>("kind"), "swept");
    assert_eq!(row.get::<i64, _>("charged_minor"), 0);
    assert_eq!(row.get::<i64, _>("freeze_minor"), freeze);
    assert_eq!(row.get::<i64, _>("input_tokens"), 0);
    assert_eq!(row.get::<i64, _>("output_tokens"), 0);
    assert_eq!(row.get::<String, _>("end_user"), "u_7");
    assert_eq!(row.get::<String, _>("service_tier"), "flex");
    assert_eq!(
        row.get::<Value, _>("tags"),
        serde_json::json!({"team": "search"})
    );
    assert_eq!(
        row.get::<uuid::Uuid, _>("entry_id"),
        *entry_id_for(&settlement_key_for(&hold_key)).as_uuid()
    );
    assert_eq!(row.get::<Option<i64>, _>("upstream_cost_minor"), None);
    unlock_sweeper(sweeper).await;
}

/// A usage row is written `ON CONFLICT DO NOTHING` on its `request_id`: a replay of the
/// same write is ignored, so the row is idempotent like the settlement it describes.
#[tokio::test]
async fn a_usage_row_replay_is_ignored() {
    let world = world!(1);
    let row = oxsum_core::UsageRow {
        request_id: format!("replay-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
        tenant_id: world.tenant.clone(),
        key_id: None,
        model: "test-model".to_owned(),
        channel: "test-channel".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: uuid::Uuid::new_v4(),
        usage: oxsum_core::UsageRecord::tokens(10, 2).unwrap(),
        charged_minor: 12,
        freeze_minor: 100,
        upstream_cost_minor: Some(4),
        upstream_attempts: 1,
    };
    world.db.record_usage(&row).await.expect("writes the row");
    world
        .db
        .record_usage(&row)
        .await
        .expect("a replay of the same write is ignored");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.usage_records WHERE request_id = $1")
            .bind(&row.request_id)
            .fetch_one(world.db.pool())
            .await
            .expect("counts the rows");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn a_hold_younger_than_the_timeout_is_not_swept() {
    let world = world!(1_000_000);
    let sweeper = lock_sweeper(world.db.pool()).await;
    let freeze = 400_000;
    let (hold_key, request_id, watch) = watch(&world.tenant, 2, freeze);
    take_hold(&world, &hold_key, &request_id, &watch, freeze).await;

    // The row is seconds old: younger than the timeout, so the sweep leaves it alone. The count is
    // not what says so — it spans every world, and an older world's row is this pass's to resolve —
    // the state of this world's row is.
    sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D)
        .await
        .expect("sweeps");
    // Untouched: still reserved, no settlement, still watched for a later pass.
    assert_eq!(world.wallet.reserved().await.unwrap(), freeze);
    assert!(settlement_record(&world.wallet, &hold_key).await.is_none());
    assert!(is_watched(&world.db, &hold_key).await);
    // Tidy up: this is the one test whose row the sweeper must not resolve.
    world
        .db
        .clear_open_hold(&hold_key)
        .await
        .expect("clears the watch row");
    unlock_sweeper(sweeper).await;
}

#[tokio::test]
async fn a_watch_row_without_a_hold_is_cleaned_up_without_a_write() {
    let world = world!(1_000_000);
    let sweeper = lock_sweeper(world.db.pool()).await;
    let (hold_key, _request_id, watch) = watch(&world.tenant, 3, 100_000);
    // The process died between noting the row and taking the hold: there is no hold entry.
    world
        .db
        .note_open_hold(&watch)
        .await
        .expect("watches the hold");
    age_row(&world.db, &hold_key, Duration::from_secs(3600)).await;
    let log_size = world.wallet.log_size().await.expect("the log size");

    let report = sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D)
        .await
        .expect("sweeps");
    // This world's row is among those resolved, whatever else the shared table held.
    assert!(report.resolved >= 1, "the sweep resolved nothing at all");
    // No settlement was written for a hold that never existed...
    assert!(settlement_record(&world.wallet, &hold_key).await.is_none());
    assert_eq!(
        world.wallet.log_size().await.expect("the log size"),
        log_size
    );
    // ...and the row is gone.
    assert!(!is_watched(&world.db, &hold_key).await);
    unlock_sweeper(sweeper).await;
}

#[tokio::test]
async fn the_sweeper_and_a_late_settlement_cannot_both_take_effect() {
    let world = world!(1_000_000);
    let sweeper = lock_sweeper(world.db.pool()).await;
    // The wallet is funded once and the rounds share it, so the expected balance carries over.
    let mut expected = 1_000_000;
    // Several rounds: whichever side the scheduler favors, the invariant is per round.
    for round in 0..10u64 {
        let freeze = 400_000;
        let charge = 15;
        let (hold_key, request_id, watch) = watch(&world.tenant, 100 + round, freeze);
        take_hold(&world, &hold_key, &request_id, &watch, freeze).await;
        age_row(&world.db, &hold_key, Duration::from_secs(3600)).await;
        let late = usage_settlement(&request_id, charge, freeze);

        // The sweep and the turn's own settlement race on the same derived idempotency key:
        // whichever appends first wins, and the other sees the key's conflict.
        let wallet = world.wallet.clone();
        let key = hold_key.clone();
        let (swept, settled) = tokio::join!(
            sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D),
            wallet.settle(&key, &late, charge, D),
        );
        // The sweep resolves this round's row, among whatever else the shared table held.
        assert!(
            swept.expect("the sweep runs").resolved >= 1,
            "the sweep resolved nothing at all"
        );

        // Exactly one settlement exists, and it is either the sweep's or the turn's.
        let record = settlement_record(&world.wallet, &hold_key)
            .await
            .expect("one settlement was written");
        let kind = record["kind"].as_str().expect("the record carries a kind");
        assert!(
            kind == "swept" || kind == "usage",
            "the race wrote an unexpected kind: {kind}"
        );
        if kind == "swept" {
            // The sweep won: the whole freeze is released, and the late settlement saw the
            // derived key's conflict rather than taking effect.
            assert!(
                matches!(settled, Err(WalletError::Conflict(_))),
                "the late settlement lost the race: {settled:?}"
            );
            assert_eq!(record["charged"], 0);
        } else {
            // The turn's settlement won: it is charged, and the sweep cleaned up behind it.
            settled.expect("the late settlement won the race");
            assert_eq!(record["charged"], charge);
            expected -= charge;
        }
        assert_eq!(world.wallet.available().await.unwrap(), expected);
        // No freeze is left stranded, and the watch row is gone whichever side won.
        assert_eq!(world.wallet.reserved().await.unwrap(), 0);
        assert!(!is_watched(&world.db, &hold_key).await);
    }
    unlock_sweeper(sweeper).await;
}

/// A hold whose sweep keeps failing is counted, keeps its last error, and at the tenth
/// failure is dead-lettered: still watched, but retried hourly instead of every pass,
/// and surfaced to the operator through the reconciliation report.
#[tokio::test]
async fn a_hold_that_keeps_failing_is_dead_lettered() {
    let world = world!(1_000_000);
    let sweeper = lock_sweeper(world.db.pool()).await;
    // `bad tenant` fails `validate_tenant_id` inside `Wallet::open`, so every sweep of
    // this row fails before it ever reaches `settle` — a persistent failure by
    // construction rather than by a mock.
    let (hold_key, _request_id, mut watch) = watch(&world.tenant, 9, 100_000);
    watch.tenant_id = "bad tenant".to_owned();
    world
        .db
        .note_open_hold(&watch)
        .await
        .expect("watches the hold");
    age_row(&world.db, &hold_key, Duration::from_secs(3600)).await;

    let mut dead_lettered = 0;
    for _ in 0..oxsum_core::DEAD_AFTER_SWEEP_ATTEMPTS {
        let report = sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D)
            .await
            .expect("sweeps");
        dead_lettered += report.dead_lettered;
    }
    // This world's row transitioned into dead exactly once; another world's row doing
    // the same in the shared table would only add to the count.
    assert!(dead_lettered >= 1, "nothing dead-lettered");

    // The row is still watched — the freeze is still stranded — marked dead and
    // holding the error that keeps killing it.
    let row = sqlx::query(
        "SELECT sweep_attempts, last_error, dead_at FROM oxsum.open_holds WHERE hold_key = $1",
    )
    .bind(&hold_key)
    .fetch_one(world.db.pool())
    .await
    .expect("the dead row is still watched");
    assert_eq!(
        row.get::<i32, _>("sweep_attempts"),
        oxsum_core::DEAD_AFTER_SWEEP_ATTEMPTS
    );
    assert!(
        row.get::<Option<String>, _>("last_error")
            .expect("the failure was recorded")
            .contains("bad tenant"),
        "the error says what is stuck"
    );
    let dead_at: Option<OffsetDateTime> = row.get("dead_at");
    assert!(dead_at.is_some(), "the row is dead-lettered");

    // The next minute pass leaves it alone: a dead row waits an hour between attempts.
    let stale = world
        .db
        .stale_open_holds(stale_cutoff())
        .await
        .expect("stale list");
    assert!(
        !stale.iter().any(|h| h.hold_key == hold_key),
        "a fresh dead row still swept every pass"
    );

    // An hour after the last failure it is eligible again — dead-lettering slows
    // the retry, it does not abandon the hold.
    sqlx::query(
        "UPDATE oxsum.open_holds SET last_attempt_at = now() - interval '2 hours' \
         WHERE hold_key = $1",
    )
    .bind(&hold_key)
    .execute(world.db.pool())
    .await
    .expect("ages the last attempt");
    let stale = world
        .db
        .stale_open_holds(stale_cutoff())
        .await
        .expect("stale list");
    assert!(stale.iter().any(|h| h.hold_key == hold_key));

    // The operator sees it two ways: the reconciliation report's ninth class and the
    // admin holds list's dead marker.
    let report = world.db.reconcile().await.expect("reconciles");
    let class = report
        .classes
        .iter()
        .find(|class| class.class == oxsum_core::DriftKind::HoldsDeadLettered)
        .expect("the report carries the dead-letter class");
    assert!(class.count >= 1, "the dead row is not drift");
    assert!(
        class.samples.iter().any(|s| s.detail.contains(&hold_key)),
        "the sample names the dead hold: {:?}",
        class.samples
    );

    // The row can never resolve — clean it up so later runs do not inherit it.
    world
        .db
        .clear_open_hold(&hold_key)
        .await
        .expect("clears the dead row");
    unlock_sweeper(sweeper).await;
}

/// The bookkeeping below the sweep: `note_sweep_failure` counts attempts, keeps the
/// error, sets `dead_at` at the threshold and never resets it, and treats a key nobody
/// watches as a no-op.
#[tokio::test]
async fn sweep_failure_bookkeeping_counts_to_dead() {
    let world = world!(1);
    let (hold_key, _request_id, watch) = watch(&world.tenant, 10, 100_000);
    world
        .db
        .note_open_hold(&watch)
        .await
        .expect("watches the hold");

    let attempts = world
        .db
        .note_sweep_failure(&hold_key, "the ledger is unreachable")
        .await
        .expect("records the failure");
    assert_eq!(attempts, 1);
    // A key nobody watches updates nothing and reads as zero attempts.
    let attempts = world
        .db
        .note_sweep_failure("req-nobody:hold", "gone")
        .await
        .expect("a missing row is not an error");
    assert_eq!(attempts, 0);

    for _ in 1..oxsum_core::DEAD_AFTER_SWEEP_ATTEMPTS {
        world
            .db
            .note_sweep_failure(&hold_key, "still down")
            .await
            .expect("records the failure");
    }
    let row = sqlx::query(
        "SELECT sweep_attempts, last_error, dead_at FROM oxsum.open_holds WHERE hold_key = $1",
    )
    .bind(&hold_key)
    .fetch_one(world.db.pool())
    .await
    .expect("the row");
    assert_eq!(
        row.get::<i32, _>("sweep_attempts"),
        oxsum_core::DEAD_AFTER_SWEEP_ATTEMPTS
    );
    assert_eq!(
        row.get::<Option<String>, _>("last_error").as_deref(),
        Some("still down")
    );
    let dead_at: Option<OffsetDateTime> = row.get("dead_at");
    let dead_at = dead_at.expect("the tenth failure dead-lettered the row");

    // The marker is the first death, not the latest retry: one more failure bumps
    // the count but keeps `dead_at`.
    world
        .db
        .note_sweep_failure(&hold_key, "and again")
        .await
        .expect("records the failure");
    let row =
        sqlx::query("SELECT sweep_attempts, dead_at FROM oxsum.open_holds WHERE hold_key = $1")
            .bind(&hold_key)
            .fetch_one(world.db.pool())
            .await
            .expect("the row");
    assert_eq!(
        row.get::<i32, _>("sweep_attempts"),
        oxsum_core::DEAD_AFTER_SWEEP_ATTEMPTS + 1
    );
    assert_eq!(
        row.get::<Option<OffsetDateTime>, _>("dead_at"),
        Some(dead_at)
    );

    world
        .db
        .clear_open_hold(&hold_key)
        .await
        .expect("clears the watch row");
}
