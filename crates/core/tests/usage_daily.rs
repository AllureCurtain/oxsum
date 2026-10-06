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

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
const PASSWORD: &str = "correct horse battery staple";

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

fn fresh(name: &str) -> String {
    format!("{name}_{}", &Uuid::new_v4().simple().to_string()[..8])
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
        model: model.to_owned(),
        channel: channel.to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        usage: UsageRecord {
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: 20,
            reasoning_tokens: 10,
            ..Default::default()
        },
        charged_minor: charged,
        freeze_minor: freeze,
        upstream_cost_minor: None,
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
        f.db.usage_daily(&f.org.tenant_id, MONDAY, FRIDAY)
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
        f.db.usage_daily(&f.org.tenant_id, MONDAY, MONDAY)
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
        f.db.usage_daily(&f.org.tenant_id, MONDAY, MONDAY)
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
        a.db.usage_daily(&a.org.tenant_id, MONDAY, MONDAY)
            .await
            .expect("a's rollup reads");
    assert_eq!(a_rows.len(), 1);
    assert_eq!(a_rows[0].model, "m-a");
    assert_eq!(a_rows[0].charged_minor, 2 * ONE);

    let b_rows =
        b.db.usage_daily(&b.org.tenant_id, MONDAY, MONDAY)
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
        f.db.usage_daily(&f.org.tenant_id, today, today)
            .await
            .expect("the rollup reads");
    assert!(rows.is_empty());
}
