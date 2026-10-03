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
    Db, OpenHold, Settlement, SettlementKind, Tenants, Wallet, WalletError, entry_id_for,
    hold_description, settlement_key_for, sweep_stale_holds,
};
use serde_json::Value;
use sqlx::PgPool;
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

/// The settlement entry's record for a hold, once it exists.
async fn settlement_record(wallet: &Arc<Wallet>, hold_key: &str) -> Option<Value> {
    let entry_id = entry_id_for(&settlement_key_for(hold_key));
    let bundle = wallet
        .receipt_proof(entry_id)
        .await
        .expect("the proof is built")?;
    Some(
        serde_json::from_str(bundle.entry.description().as_str()).expect("the description is JSON"),
    )
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
    for _ in 0..600 {
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
    panic!("could not take the sweep lock within a minute");
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
    Settlement {
        request: request_id,
        channel: "test-channel",
        model: "test-model",
        price_version: 1,
        kind: SettlementKind::Usage,
        input_tokens: 10,
        output_tokens: 5,
        input_price: 1_000_000,
        output_price: 1_000_000,
        charged,
        freeze,
    }
    .description()
    .expect("the test's own record builds")
}

#[tokio::test]
async fn a_stale_hold_is_swept_at_zero_and_recorded_as_an_anomaly() {
    let world = world!(1_000_000);
    let sweeper = lock_sweeper(world.db.pool()).await;
    let freeze = 400_000;
    let (hold_key, request_id, watch) = watch(&world.tenant, 1, freeze);
    take_hold(&world, &hold_key, &request_id, &watch, freeze).await;
    assert_eq!(world.wallet.available().await.unwrap(), 600_000);
    age_row(&world.db, &hold_key, Duration::from_secs(3600)).await;

    let resolved = sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D)
        .await
        .expect("sweeps");
    // This world's row is among those resolved. The count may also cover stale rows other worlds
    // left behind, which is correct, so it is not asserted exactly.
    assert!(resolved >= 1, "the sweep resolved nothing at all");

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
    assert_eq!(record["inputTokens"], 0);
    assert_eq!(record["outputTokens"], 0);
    assert_eq!(record["freeze"], freeze);
    assert_eq!(record["request"], request_id);
    // A second pass finds nothing: the row is gone, and nothing can have aged a new one — only a
    // test ages rows, and this one still holds the sweep lock.
    let resolved = sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D)
        .await
        .expect("sweeps again");
    assert_eq!(resolved, 0);
    assert!(!is_watched(&world.db, &hold_key).await);
    unlock_sweeper(sweeper).await;
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

    let resolved = sweep_stale_holds(&world.db, &world.tenants, stale_cutoff(), D)
        .await
        .expect("sweeps");
    // This world's row is among those resolved, whatever else the shared table held.
    assert!(resolved >= 1, "the sweep resolved nothing at all");
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
            swept.expect("the sweep runs") >= 1,
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
