//! `usage_daily` rollup integration tests, running against real PostgreSQL
//! (roadmap P3-3, issue #126).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! The rollup is what the usage dashboard reads: `record_usage` rolls each
//! settled turn into `oxsum.usage_daily` in the same transaction, keyed by the
//! settlement entry's `booking_date`. These tests pin the aggregate's
//! correctness — the day key, the sums, the replay rule, the tenant scope — not
//! the page that draws it.

use oxsum_core::{AdminOrganization, Db, NewUser, SettlementKind, UsageRecord, UsageRow, Wallet};
use sqlx::PgPool;
use time::macros::date;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

const ONE: i64 = 1_000_000;
/// Two days a week apart, both safely in the past — `booking_date` is the test's
/// own dial, so the rollup's day key is checked rather than assumed.
const MONDAY: Date = date!(2026 - 10 - 05);
const FRIDAY: Date = date!(2026 - 10 - 09);
/// A day no other test settles on: the anchor tests seal their day process-wide
/// (the anchor pass visits every organization), so sharing a day with the
/// settling tests would race the seal against them.
const ANCHOR_DAY: Date = date!(2026 - 09 - 17);
const PASSWORD: &str = "correct horse battery staple";

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

fn fresh(name: &str) -> String {
    format!("{name}_{}", &Uuid::new_v4().simple().to_string()[..8])
}

/// The scope these tests read under: every key plus the unattributed rows —
/// the owner/admin view the rollup's aggregation checks exercise.
fn everything() -> oxsum_core::CustomerRecordScope {
    oxsum_core::CustomerRecordScope {
        key_ids: None,
        include_unattributed: true,
    }
}

/// A registered organization with its wallet: the smallest world a rollup test
/// needs.
struct Fixture {
    db: Db,
    org: AdminOrganization,
    wallet: Wallet,
}

async fn fixture(url: &str, name: &str) -> Fixture {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", fresh(name)),
            password: PASSWORD.to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    let org = db
        .organization_by_id(registration.organization.id)
        .await
        .expect("the organization reads back");
    let wallet = Wallet::open(db.pool().clone(), &org.tenant_id)
        .await
        .expect("the wallet opens");
    Fixture { db, org, wallet }
}

/// One settled turn and its usage row — the write path `record_usage` rolls
/// from. `key_id` rides on the row so the members' scope can be checked.
async fn turn(
    f: &Fixture,
    request: &str,
    (channel, model): (&str, &str),
    key_id: Option<Uuid>,
    freeze: i64,
    charged: i64,
    on: Date,
) -> UsageRow {
    let hold_key = format!("hold-{request}");
    f.wallet
        .hold(&hold_key, "", freeze, on)
        .await
        .expect("the hold reserves");
    let receipt = f
        .wallet
        .settle(&hold_key, "", charged, on)
        .await
        .expect("the turn settles");
    let row = UsageRow {
        // The usage table deduplicates on request_id globally, so a fixed id a
        // previous run wrote would silently skip this insert.
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id,
        mandate_id: None,
        model: model.to_owned(),
        channel: channel.to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        status: "settled".to_owned(),
        failure_reason: None,
        usage: UsageRecord {
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: 20,
            reasoning_tokens: 10,
            ..Default::default()
        },
        counts: oxsum_core::CountsProvenance::Provider {
            input: 100,
            output: 50,
        },
        charged_minor: charged,
        freeze_minor: freeze,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 1,
    };
    f.db.record_usage(&row)
        .await
        .expect("the usage row is written");
    row
}

#[tokio::test]
async fn turns_roll_into_their_booking_day() {
    let url = db_or_skip!();
    let f = fixture(&url, "daily").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, MONDAY)
        .await
        .expect("the wallet funds");

    // Two turns on Monday (same model, different keys roll apart), one on
    // Friday: the window covers three keyed rows plus one unattributed.
    let key_a = Uuid::new_v4();
    turn(
        &f,
        "m1",
        ("chan", "m-a"),
        Some(key_a),
        10 * ONE,
        2 * ONE,
        MONDAY,
    )
    .await;
    turn(
        &f,
        "m2",
        ("chan", "m-a"),
        Some(key_a),
        10 * ONE,
        3 * ONE,
        MONDAY,
    )
    .await;
    turn(&f, "m3", ("chan", "m-b"), None, 10 * ONE, ONE, MONDAY).await;
    turn(
        &f,
        "f1",
        ("chan", "m-a"),
        Some(key_a),
        10 * ONE,
        4 * ONE,
        FRIDAY,
    )
    .await;

    let rows =
        f.db.usage_daily(&f.org.tenant_id, MONDAY, FRIDAY, &everything())
            .await
            .expect("the rollup reads");

    let monday_a = rows
        .iter()
        .find(|row| row.day == MONDAY && row.key_id == Some(key_a) && row.model == "m-a")
        .expect("the Monday keyed row exists");
    assert_eq!(monday_a.turns, 2);
    assert_eq!(monday_a.input_tokens, 200);
    assert_eq!(monday_a.output_tokens, 100);
    assert_eq!(monday_a.cached_tokens, 40);
    assert_eq!(monday_a.reasoning_tokens, 20);
    assert_eq!(monday_a.charged_minor, 5 * ONE);

    let monday_shared = rows
        .iter()
        .find(|row| row.day == MONDAY && row.key_id.is_none() && row.model == "m-b")
        .expect("the unattributed Monday row exists");
    assert_eq!(monday_shared.turns, 1);
    assert_eq!(monday_shared.charged_minor, ONE);

    let friday = rows
        .iter()
        .find(|row| row.day == FRIDAY)
        .expect("the Friday row exists");
    assert_eq!(friday.charged_minor, 4 * ONE);

    // The window bound holds: a narrower window drops Friday.
    let rows =
        f.db.usage_daily(&f.org.tenant_id, MONDAY, MONDAY, &everything())
            .await
            .expect("the bounded rollup reads");
    assert!(rows.iter().all(|row| row.day == MONDAY));
}

#[tokio::test]
async fn a_replayed_write_counts_once() {
    let url = db_or_skip!();
    let f = fixture(&url, "replay").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, MONDAY)
        .await
        .expect("the wallet funds");

    let row = turn(&f, "r1", ("chan", "m-a"), None, 10 * ONE, 2 * ONE, MONDAY).await;
    // The same request writes again — a retry of the settle path. The usage
    // insert no-ops, so the rollup must not grow.
    f.db.record_usage(&row)
        .await
        .expect("the replay is ignored");

    let rows =
        f.db.usage_daily(&f.org.tenant_id, MONDAY, MONDAY, &everything())
            .await
            .expect("the rollup reads");
    let row = rows
        .iter()
        .find(|row| row.day == MONDAY && row.model == "m-a")
        .expect("the row exists");
    assert_eq!(row.turns, 1);
    assert_eq!(row.charged_minor, 2 * ONE);
}

#[tokio::test]
async fn the_rollup_is_scoped_per_tenant() {
    let url = db_or_skip!();
    let a = fixture(&url, "tenant-a").await;
    let b = fixture(&url, "tenant-b").await;
    for f in [&a, &b] {
        f.wallet
            .top_up(&fresh("seed"), 20 * ONE, MONDAY)
            .await
            .expect("the wallet funds");
    }

    turn(&a, "a1", ("chan", "m-a"), None, 10 * ONE, 2 * ONE, MONDAY).await;
    turn(&b, "b1", ("chan", "m-b"), None, 10 * ONE, 7 * ONE, MONDAY).await;

    let a_rows =
        a.db.usage_daily(&a.org.tenant_id, MONDAY, MONDAY, &everything())
            .await
            .expect("a's rollup reads");
    assert_eq!(a_rows.len(), 1);
    assert_eq!(a_rows[0].model, "m-a");
    assert_eq!(a_rows[0].charged_minor, 2 * ONE);

    let b_rows =
        b.db.usage_daily(&b.org.tenant_id, MONDAY, MONDAY, &everything())
            .await
            .expect("b's rollup reads");
    assert_eq!(b_rows.len(), 1);
    assert_eq!(b_rows[0].model, "m-b");
    assert_eq!(b_rows[0].charged_minor, 7 * ONE);
}

#[tokio::test]
async fn an_empty_window_reads_empty() {
    let url = db_or_skip!();
    let f = fixture(&url, "empty").await;
    let today = OffsetDateTime::now_utc().date();
    let rows =
        f.db.usage_daily(&f.org.tenant_id, today, today, &everything())
            .await
            .expect("the rollup reads");
    assert!(rows.is_empty());
}

/// One settled turn attributed to `end_user` — the row a usage report reads and
/// the day log folds.
async fn customer_turn(
    f: &Fixture,
    request: &str,
    end_user: &str,
    charged: i64,
    on: Date,
) -> UsageRow {
    let hold_key = format!("hold-{request}");
    f.wallet
        .hold(&hold_key, "", 10 * ONE, on)
        .await
        .expect("the hold reserves");
    let receipt = f
        .wallet
        .settle(&hold_key, "", charged, on)
        .await
        .expect("the turn settles");
    let usage = UsageRecord {
        input_tokens: 100,
        output_tokens: 50,
        end_user: Some(end_user.to_owned()),
        ..Default::default()
    };
    let row = UsageRow {
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id: None,
        mandate_id: None,
        model: "m-a".to_owned(),
        channel: "chan".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        status: "settled".to_owned(),
        failure_reason: None,
        usage,
        counts: oxsum_core::CountsProvenance::Provider {
            input: 100,
            output: 50,
        },
        charged_minor: charged,
        freeze_minor: 10 * ONE,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 1,
    };
    f.db.record_usage(&row)
        .await
        .expect("the usage row is written");
    row
}

/// The stored day row as `(count, root, leaf)`.
async fn day_row(f: &Fixture, end_user: &str, day: Date) -> Option<(i64, Vec<u8>, Vec<u8>)> {
    use sqlx::Row;
    sqlx::query(
        "SELECT entry_count, rows_root, leaf_hash FROM oxsum.usage_day_log \
         WHERE tenant_id = $1 AND end_user = $2 AND day = $3",
    )
    .bind(&f.org.tenant_id)
    .bind(end_user)
    .bind(day)
    .fetch_optional(f.db.pool())
    .await
    .expect("reads the day row")
    .map(|row| {
        (
            row.get("entry_count"),
            row.get("rows_root"),
            row.get("leaf_hash"),
        )
    })
}

/// The root recomputed the way the writer does: the entries' content hashes in
/// log order folded by the ledger's own accumulator.
async fn expected_root(f: &Fixture, entry_ids: &[Uuid]) -> Vec<u8> {
    let mut accumulator = oxsum_core::MerkleAccumulator::new();
    for entry_id in entry_ids {
        let hash: Vec<u8> = sqlx::query_scalar(&format!(
            "SELECT content_hash FROM ledger_{}.entries WHERE entry_id = $1",
            f.org.tenant_id
        ))
        .bind(entry_id)
        .fetch_one(f.db.pool())
        .await
        .expect("reads the entry's hash");
        let array: [u8; 32] = hash.as_slice().try_into().expect("32 bytes");
        accumulator.push(oxsum_core::Hash::from_bytes(array));
    }
    accumulator.root().as_bytes().to_vec()
}

/// `H(domain, day || count || root)` — the leaf the verifier will rebuild.
fn expected_leaf(day: Date, count: i64, root: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(day.to_string().as_bytes());
    payload.extend_from_slice(&count.to_le_bytes());
    payload.extend_from_slice(root);
    oxsum_core::Hash::digest(oxsum_core::USAGE_DAY_DOMAIN, &payload)
        .as_bytes()
        .to_vec()
}

/// The day log (issue #337): a settled turn with an `endUser` writes its day's
/// leaf, a second turn rewrites the root and count, a replay changes nothing,
/// and another day is its own row.
#[tokio::test]
async fn a_settled_turn_writes_its_customers_day_leaf() {
    let url = db_or_skip!();
    let f = fixture(&url, "daylog").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, MONDAY)
        .await
        .expect("the wallet funds");
    let end_user = "customer-day-1";

    let first = customer_turn(&f, "d1", end_user, 2 * ONE, MONDAY).await;
    let (count, root, leaf) = day_row(&f, end_user, MONDAY)
        .await
        .expect("a day row is written");
    assert_eq!(count, 1);
    assert_eq!(root, expected_root(&f, &[first.entry_id]).await);
    assert_eq!(leaf, expected_leaf(MONDAY, count, &root));

    // A second turn the same day rewrites the day's root and count.
    customer_turn(&f, "d2", end_user, 3 * ONE, MONDAY).await;
    let (count, root_after, _) = day_row(&f, end_user, MONDAY)
        .await
        .expect("the day row is rewritten");
    assert_eq!(count, 2);
    assert_ne!(root_after, root, "a new row changes the day's root");

    // A replayed write changes nothing: the usage insert dedupes on request_id
    // and the day log is only reached for a fresh row.
    f.db.record_usage(&first)
        .await
        .expect("the replay is a no-op");
    let (count, root_replayed, _) = day_row(&f, end_user, MONDAY)
        .await
        .expect("the day row stands");
    assert_eq!((count, root_replayed), (2, root_after));

    // Another day is its own leaf.
    customer_turn(&f, "d3", end_user, ONE, FRIDAY).await;
    assert_eq!(
        day_row(&f, end_user, FRIDAY).await.map(|row| row.0),
        Some(1)
    );
}

/// A customer's head over a single day leaf.
fn single_leaf_root(leaf: &[u8]) -> oxsum_core::Hash {
    let mut accumulator = oxsum_core::MerkleAccumulator::new();
    accumulator.push(oxsum_core::Hash::from_bytes(
        leaf.try_into().expect("32 bytes"),
    ));
    accumulator.root()
}

/// The anchor pass (issue #338): one ledger entry per organization and day
/// commits the sorted-map root over every customer's day-log head, the entry
/// sequences in the organization's log, and the second run is a no-op.
#[tokio::test]
async fn the_anchor_pass_commits_the_map_root_once() {
    use sqlx::Row;
    let url = db_or_skip!();
    let f = fixture(&url, "anchor").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, ANCHOR_DAY)
        .await
        .expect("the wallet funds");

    customer_turn(&f, "a1", "customer-a", 2 * ONE, ANCHOR_DAY).await;
    customer_turn(&f, "a2", "customer-a", 3 * ONE, ANCHOR_DAY).await;
    customer_turn(&f, "b1", "customer-b", 2 * ONE, ANCHOR_DAY).await;

    let report =
        f.db.anchor_usage_days(ANCHOR_DAY)
            .await
            .expect("the pass anchors");
    assert_eq!(report.anchors, 1, "one organization anchored");
    assert_eq!(report.skipped, 0);

    let row = sqlx::query(
        "SELECT entry_id, map_root, enduser_count FROM oxsum.usage_anchors \
         WHERE organization_id = $1 AND day = $2",
    )
    .bind(f.org.id)
    .bind(ANCHOR_DAY)
    .fetch_one(f.db.pool())
    .await
    .expect("the anchor row");
    let entry_id: Uuid = row.get("entry_id");
    let map_root: Vec<u8> = row.get("map_root");
    assert_eq!(row.get::<i64, _>("enduser_count"), 2, "two customers");

    // The root is the map over each customer's single-day head, in endUser order.
    let leaf_a = day_row(&f, "customer-a", ANCHOR_DAY)
        .await
        .expect("a's leaf")
        .2;
    let leaf_b = day_row(&f, "customer-b", ANCHOR_DAY)
        .await
        .expect("b's leaf")
        .2;
    let expected = oxsum_verify::usage_anchor_map_root(&[
        ("customer-a", single_leaf_root(&leaf_a), 1),
        ("customer-b", single_leaf_root(&leaf_b), 1),
    ]);
    assert_eq!(map_root, expected.as_bytes().to_vec());

    // The anchor entry sequenced in the organization's log.
    let log_index: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT log_index FROM ledger_{}.entries WHERE entry_id = $1",
        f.org.tenant_id
    ))
    .bind(entry_id)
    .fetch_one(f.db.pool())
    .await
    .expect("the anchor entry");
    assert!(log_index.is_some(), "the anchor sequenced");

    // A rerun is a no-op and answers the standing entry.
    let again =
        f.db.anchor_usage_days(ANCHOR_DAY)
            .await
            .expect("the pass reruns");
    assert_eq!(again.anchors, 0);
    assert_eq!(again.skipped, 1);
    let standing: Uuid = sqlx::query_scalar(
        "SELECT entry_id FROM oxsum.usage_anchors \
         WHERE organization_id = $1 AND day = $2",
    )
    .bind(f.org.id)
    .bind(ANCHOR_DAY)
    .fetch_one(f.db.pool())
    .await
    .expect("the standing anchor");
    assert_eq!(standing, entry_id);
}

/// The daily anchor seals its day (issue #340): a settlement booking into an
/// anchored day is refused as `DaySealed`, while a later day still settles.
#[tokio::test]
async fn a_settlement_cannot_book_into_an_anchored_day() {
    let url = db_or_skip!();
    let f = fixture(&url, "sealed").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, ANCHOR_DAY)
        .await
        .expect("the wallet funds");
    customer_turn(&f, "s1", "sealed-customer", 2 * ONE, ANCHOR_DAY).await;

    let report =
        f.db.anchor_usage_days(ANCHOR_DAY)
            .await
            .expect("the pass anchors");
    assert!(report.anchors >= 1);

    // The hold still succeeds; the settle is refused.
    let hold_key = "hold-sealed-1";
    f.wallet
        .hold(hold_key, "", 2 * ONE, ANCHOR_DAY)
        .await
        .expect("the hold reserves");
    let err = f
        .wallet
        .settle(hold_key, "", ONE, ANCHOR_DAY)
        .await
        .expect_err("the settle is refused");
    assert!(matches!(err, oxsum_core::WalletError::DaySealed), "{err:?}");

    // Another day is not sealed.
    let hold_key = "hold-sealed-2";
    f.wallet
        .hold(hold_key, "", 2 * ONE, FRIDAY)
        .await
        .expect("the hold reserves");
    f.wallet
        .settle(hold_key, "", ONE, FRIDAY)
        .await
        .expect("the later day settles");
}

/// A day leaf altered after its anchor is reconciliation drift (issue #340).
#[tokio::test]
async fn an_altered_day_leaf_is_reconciliation_drift() {
    let url = db_or_skip!();
    let f = fixture(&url, "anchordrift").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, ANCHOR_DAY)
        .await
        .expect("the wallet funds");
    customer_turn(&f, "a1", "drift-customer", 2 * ONE, ANCHOR_DAY).await;
    f.db.anchor_usage_days(ANCHOR_DAY)
        .await
        .expect("the pass anchors");

    // A hand edit — the shape a bypassed seal leaves behind — rewrites the leaf.
    sqlx::query(
        "UPDATE oxsum.usage_day_log SET leaf_hash = decode(repeat('00', 32), 'hex') \
         WHERE tenant_id = $1 AND end_user = $2 AND day = $3",
    )
    .bind(&f.org.tenant_id)
    .bind("drift-customer")
    .bind(ANCHOR_DAY)
    .execute(f.db.pool())
    .await
    .expect("the leaf is altered");

    let report = f.db.reconcile().await.expect("the scan runs");
    let class = report
        .classes
        .iter()
        .find(|class| class.class == oxsum_core::DriftKind::AnchoredDayAltered)
        .expect("the class is reported");
    assert!(class.count >= 1, "the altered leaf is drift");
    assert!(
        class
            .samples
            .iter()
            .any(|sample| sample.detail == ANCHOR_DAY.to_string()),
        "{:?}",
        class.samples
    );
}

/// A usage row built like `turn`'s but left unwritten — the seal tests need a
/// settled entry id without the leaf write `record_usage` performs.
fn unwritten_row(
    f: &Fixture,
    request: &str,
    entry_id: Uuid,
    charged: i64,
    end_user: &str,
) -> UsageRow {
    let mut row = UsageRow {
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id: None,
        mandate_id: None,
        model: "m-anchor".to_owned(),
        channel: "chan".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id,
        status: "settled".to_owned(),
        failure_reason: None,
        usage: UsageRecord {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        counts: oxsum_core::CountsProvenance::Provider {
            input: 100,
            output: 50,
        },
        charged_minor: charged,
        freeze_minor: charged,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 1,
    };
    row.usage.end_user = Some(end_user.to_owned());
    row
}

/// The writer-side seal (issue #1088): a leaf rewrite that lands after the
/// anchor committed is refused with `DaySealed` instead of rewriting the leaf
/// the day's `map_root` fixed — the half the entry-point `DaySealed` checks
/// cannot reach, a settle that booked before the anchor existed.
#[tokio::test]
async fn a_leaf_rewrite_cannot_follow_the_anchor() {
    let url = db_or_skip!();
    let f = fixture(&url, "leafseal").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, ANCHOR_DAY)
        .await
        .expect("the wallet funds");
    customer_turn(&f, "l1", "leaf-customer", 2 * ONE, ANCHOR_DAY).await;
    f.db.anchor_usage_days(ANCHOR_DAY)
        .await
        .expect("the pass anchors");

    // A leaf write against the committed settle's entry: the usage row's own
    // request id is fresh so the write is no replay — only the day it rolls
    // into is sealed, which is what the re-check under the locks refuses.
    let entry_id: Uuid = sqlx::query_scalar(
        "SELECT entry_id FROM oxsum.usage_records WHERE tenant_id = $1 AND end_user = $2 LIMIT 1",
    )
    .bind(&f.org.tenant_id)
    .bind("leaf-customer")
    .fetch_one(f.db.pool())
    .await
    .expect("the settle's entry");
    let row = unwritten_row(&f, "sealed-leaf", entry_id, ONE, "leaf-customer");
    let err =
        f.db.record_usage(&row)
            .await
            .expect_err("the sealed day's leaf rewrite is refused");
    assert!(matches!(err, oxsum_core::WalletError::DaySealed), "{err:?}");
}

/// The anchor waits out an in-flight leaf write and still commits a root that
/// covers it (issue #1088): the per-(tenant, day) lock the writer holds makes
/// the pass's read the day's final state, so a settle the pass raced lands in
/// the map rather than past it.
#[tokio::test]
async fn the_anchor_waits_for_an_in_flight_leaf_write() {
    let url = db_or_skip!();
    let f = fixture(&url, "anchorrace").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, ANCHOR_DAY)
        .await
        .expect("the wallet funds");
    customer_turn(&f, "r1", "race-customer", 2 * ONE, ANCHOR_DAY).await;

    // A second leaf write holds the day's locks without committing them: its
    // leaf is invisible to the pass's read, so the committed map must still
    // name the customer once the write finishes — by waiting the writer out.
    // The lock here stands in for that write's critical section: the point is
    // the ordering the shared locks enforce, not the write's internals.
    let mut holder = f.db.pool().begin().await.expect("the lock holder");
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('oxsum/usage-anchor/v1' || $1 || ':' || $2, 0))",
    )
    .bind(&f.org.tenant_id)
    .bind(ANCHOR_DAY.to_string())
    .execute(&mut *holder)
    .await
    .expect("the day lock lands");

    let db = f.db.clone();
    let anchor = tokio::spawn(async move { db.anchor_usage_days(ANCHOR_DAY).await });
    // The pass cannot commit while the holder keeps the locks; give it a beat
    // to reach the lock wait, then release — the commit that follows must
    // still have read the leaves the race could have hidden.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!anchor.is_finished(), "the pass waits on the held locks");
    holder.rollback().await.expect("the locks release");

    let report = anchor
        .await
        .expect("the task joins")
        .expect("the pass anchors");
    assert_eq!(report.anchors, 1, "the raced pass still commits");
    let count: i64 = sqlx::query_scalar(
        "SELECT enduser_count FROM oxsum.usage_anchors \
         WHERE organization_id = $1 AND day = $2",
    )
    .bind(f.org.id)
    .bind(ANCHOR_DAY)
    .fetch_one(f.db.pool())
    .await
    .expect("the anchor row");
    assert_eq!(count, 1, "the map covers the day's customer");
}

/// A failed organization's anchor no longer aborts the pass (issue #1088):
/// one bad row is logged and counted, the rest still commit, and the next run
/// retries the organization that failed.
#[tokio::test]
async fn one_failed_organization_does_not_abort_the_pass() {
    let url = db_or_skip!();
    let f = fixture(&url, "anchorfail").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, ANCHOR_DAY)
        .await
        .expect("the wallet funds");
    customer_turn(&f, "f1", "fail-customer", 2 * ONE, ANCHOR_DAY).await;
    // A second organization is the failure: an uppercase tenant id fails the
    // wallet's `validate_tenant_id`, so `tenants.get` refuses it and the
    // anchor attempt errors — which the pass must capture rather than abort
    // on. The leaf row is inserted directly (no usage row behind it), the
    // drift a bypassed writer leaves for the reconciler.
    let broken =
        f.db.register(NewUser {
            email: format!("{}@example.com", fresh("broken")),
            password: PASSWORD.to_owned(),
            organization_name: None,
        })
        .await
        .expect("the second organization registers");
    sqlx::query("UPDATE oxsum.organizations SET tenant_id = 'BAD' WHERE organization_id = $1")
        .bind(broken.organization.id)
        .execute(f.db.pool())
        .await
        .expect("the tenant id is invalidated");
    sqlx::query("INSERT INTO oxsum.usage_day_log \
         (tenant_id, end_user, day, entry_count, rows_root, leaf_hash) \
         VALUES ('BAD', 'ghost', $1, 1, decode(repeat('11', 32), 'hex'), decode(repeat('22', 32), 'hex'))")
        .bind(ANCHOR_DAY)
        .execute(f.db.pool())
        .await
        .expect("a day leaf stands for it");

    let report =
        f.db.anchor_usage_days(ANCHOR_DAY)
            .await
            .expect("the pass completes");
    assert_eq!(report.failed, 1, "the broken organization is counted");
    assert_eq!(report.anchors, 1, "the healthy organization still anchors");
}

/// The pass's deferred count still names an organization with an open
/// operation (issue #603): a day-close that cannot be final waits rather than
/// seal a map the settle path may still change.
#[tokio::test]
async fn an_open_operation_defers_the_anchor() {
    let url = db_or_skip!();
    let f = fixture(&url, "anchordefer").await;
    f.wallet
        .top_up(&fresh("seed"), 20 * ONE, ANCHOR_DAY)
        .await
        .expect("the wallet funds");
    customer_turn(&f, "d1", "defer-customer", 2 * ONE, ANCHOR_DAY).await;
    sqlx::query(
        "INSERT INTO oxsum.request_operations \
             (operation_id, organization_id, surface, fingerprint, state, \
              bound_minor, model, created_at) \
         VALUES ($1, $2, 'gateway', $3, 'running', 0, 'm-a', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(f.org.id)
    .bind(fresh("op"))
    .bind(ANCHOR_DAY.midnight().assume_utc())
    .execute(f.db.pool())
    .await
    .expect("the open operation stands");

    let report =
        f.db.anchor_usage_days(ANCHOR_DAY)
            .await
            .expect("the pass completes");
    assert_eq!(report.deferred, 1, "the open operation defers the org");
    assert_eq!(report.anchors, 0);
}
