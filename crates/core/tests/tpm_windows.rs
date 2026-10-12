//! `oxsum.tpm_windows` integration tests, running against real PostgreSQL
//! (roadmap P3-4, issue #203).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! The table is the durable trailing-minute token window a `tokensPerMinute`
//! key checks at admission: `record_usage` rolls each settled turn's
//! `input + output` tokens into one-second buckets inside the same transaction
//! that writes the usage row, so a replay accrues nothing and a rollback keeps
//! nothing. These tests pin the accrual, the replay rule, the unkeyed and
//! zero-token skips, the trailing-minute read, and the prune-on-write — not the
//! admission check, which the server suites cover.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{AdminOrganization, Db, NewUser, SettlementKind, UsageRecord, UsageRow, Wallet};
use sqlx::PgPool;
use time::OffsetDateTime;
use time::macros::date;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery staple";
/// A settle day safely in the past — `record_usage` uses the entry's booking
/// date for `usage_daily`, but the token window keys on the settle's instant.
const ON: time::Date = date!(2026 - 10 - 05);

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

/// A registered organization with its wallet and a key to accrue under.
struct Fixture {
    db: Db,
    org: AdminOrganization,
    wallet: Wallet,
    pool: PgPool,
    key_id: Uuid,
}

async fn fixture(url: &str, name: &str) -> Fixture {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
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
    let wallet = Wallet::open(pool.clone(), &org.tenant_id)
        .await
        .expect("the wallet opens");
    // The holds the turns place need balance to reserve against.
    wallet
        .top_up("topup", 10_000_000, ON)
        .await
        .expect("the top-up funds the wallet");
    Fixture {
        db,
        org,
        wallet,
        pool,
        key_id: registration.api_key.key.id,
    }
}

/// One settled turn and its usage row, with the token counts the window sums.
async fn turn(
    f: &Fixture,
    request: &str,
    key_id: Option<Uuid>,
    input: i64,
    output: i64,
) -> UsageRow {
    let hold_key = format!("hold-{request}");
    f.wallet
        .hold(&hold_key, "", 1_000_000, ON)
        .await
        .expect("the hold reserves");
    let receipt = f
        .wallet
        .settle(&hold_key, "", 100_000, ON)
        .await
        .expect("the turn settles");
    let row = UsageRow {
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id,
        model: "mock".to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        usage: UsageRecord {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        },
        charged_minor: 100_000,
        freeze_minor: 1_000_000,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    };
    f.db.record_usage(&row)
        .await
        .expect("the usage row is written");
    row
}

/// The raw rows a key's window holds, straight from the table.
async fn window_rows(f: &Fixture, key_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT sum(tokens)::bigint FROM oxsum.tpm_windows WHERE key_id = $1",
    )
    .bind(key_id)
    .fetch_one(&f.pool)
    .await
    .expect("the window reads")
    .unwrap_or(0)
}

/// Every settled turn feeds its key's window in the settle's own transaction:
/// `input + output` lands under the settle's instant, and a second turn adds.
#[tokio::test]
async fn settled_tokens_roll_into_the_key_window() {
    let url = db_or_skip!();
    let f = fixture(&url, "tpmaccrue").await;

    turn(&f, "first", Some(f.key_id), 100, 50).await;
    let (settled, _retry) = f.db.tpm_window(f.key_id).await.expect("the window reads");
    assert_eq!(settled, 150, "input + output of the settle");
    assert_eq!(window_rows(&f, f.key_id).await, 150);

    turn(&f, "second", Some(f.key_id), 200, 100).await;
    let (settled, _) = f.db.tpm_window(f.key_id).await.unwrap();
    assert_eq!(settled, 450, "the window accumulates across settles");
}

/// The window sums what the record normalizes, not what the price billed: the
/// cached and reasoning subsets ride inside `input_tokens`/`output_tokens`, so a
/// turn reporting them accrues the two totals, not the lines.
#[tokio::test]
async fn the_window_counts_the_normalized_totals() {
    let url = db_or_skip!();
    let f = fixture(&url, "tpmnorm").await;

    let hold_key = "hold-norm";
    f.wallet.hold(hold_key, "", 1_000_000, ON).await.unwrap();
    let receipt = f.wallet.settle(hold_key, "", 100_000, ON).await.unwrap();
    f.db.record_usage(&UsageRow {
        request_id: format!("norm-{}", Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id: Some(f.key_id),
        model: "mock".to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        usage: UsageRecord {
            input_tokens: 100,
            output_tokens: 40,
            cached_tokens: 30,
            reasoning_tokens: 10,
            ..Default::default()
        },
        charged_minor: 100_000,
        freeze_minor: 1_000_000,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    })
    .await
    .unwrap();
    let (settled, _) = f.db.tpm_window(f.key_id).await.unwrap();
    // 100 + 40, not the billed lines: TPM bounds consumption, not price.
    assert_eq!(settled, 140);
}

/// A replayed usage row rolls nothing: `record_usage`'s `ON CONFLICT DO NOTHING`
/// early-return keeps the window from double-counting one turn.
#[tokio::test]
async fn a_replayed_usage_row_rolls_nothing() {
    let url = db_or_skip!();
    let f = fixture(&url, "tpmreplay").await;

    let row = turn(&f, "replay", Some(f.key_id), 100, 50).await;
    // The identical request_id writes again — the dedup swallows it.
    f.db.record_usage(&row)
        .await
        .expect("the replay is swallowed");
    assert_eq!(
        window_rows(&f, f.key_id).await,
        150,
        "the second write accrued nothing"
    );
}

/// Only a key's own settles move its window: unkeyed rows (metering, session
/// settles) and zero-token turns write nothing for a window to read.
#[tokio::test]
async fn unkeyed_and_zero_token_rows_skip_the_window() {
    let url = db_or_skip!();
    let f = fixture(&url, "tpmskip").await;

    // Unkeyed: a service credential's metering event has no key window.
    turn(&f, "metered", None, 100, 50).await;
    // Zero-token: a zero-charge or count-less settle has nothing to accrue.
    turn(&f, "zero", Some(f.key_id), 0, 0).await;

    assert_eq!(window_rows(&f, f.key_id).await, 0);
    let (settled, retry) = f.db.tpm_window(f.key_id).await.unwrap();
    assert_eq!((settled, retry), (0, 0), "an empty window reads zero");
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.tpm_windows WHERE key_id = $1")
        .bind(f.key_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(total, 0, "no window rows were written at all");
}

/// The read is a trailing minute: rows aged past it drop out of the sum and out
/// of the retry-after anchor, while the rows still inside bound both.
#[tokio::test]
async fn the_window_sums_only_the_last_minute() {
    let url = db_or_skip!();
    let f = fixture(&url, "tpmminute").await;

    // A row already past the window: it must not count.
    sqlx::query(
        "INSERT INTO oxsum.tpm_windows (key_id, window_start, tokens) \
         VALUES ($1, now() - interval '90 seconds', 500)",
    )
    .bind(f.key_id)
    .execute(&f.pool)
    .await
    .unwrap();
    // A fresh row inside it.
    sqlx::query(
        "INSERT INTO oxsum.tpm_windows (key_id, window_start, tokens) \
         VALUES ($1, date_trunc('second', now()), 40)",
    )
    .bind(f.key_id)
    .execute(&f.pool)
    .await
    .unwrap();

    let (settled, retry_after) = f.db.tpm_window(f.key_id).await.unwrap();
    assert_eq!(settled, 40, "only the in-window row counts");
    assert!(
        retry_after > 0 && retry_after <= 60,
        "the fresh row frees inside the minute, got {retry_after}"
    );
}

/// The window prunes as it writes: a settle leaves the aged rows it just pushed
/// out of the minute behind, so the table stays bounded by what admission reads.
#[tokio::test]
async fn rows_older_than_the_window_are_pruned_on_write() {
    let url = db_or_skip!();
    let f = fixture(&url, "tpmprune").await;

    sqlx::query(
        "INSERT INTO oxsum.tpm_windows (key_id, window_start, tokens) \
         VALUES ($1, now() - interval '90 seconds', 500)",
    )
    .bind(f.key_id)
    .execute(&f.pool)
    .await
    .unwrap();
    let aged: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.tpm_windows WHERE key_id = $1")
        .bind(f.key_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(aged, 1);

    // A fresh settle writes a row and prunes the aged one in the same sweep.
    turn(&f, "fresh", Some(f.key_id), 10, 5).await;

    let remaining: Vec<OffsetDateTime> =
        sqlx::query_scalar("SELECT window_start FROM oxsum.tpm_windows WHERE key_id = $1")
            .bind(f.key_id)
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(remaining.len(), 1, "only the fresh row remains");
    assert!(
        OffsetDateTime::now_utc() - remaining[0] < time::Duration::seconds(61),
        "the remaining row is inside the window"
    );
}

// ── tag-scoped buckets (issue #209) ──────────────────────────────────────────

/// A settle carrying `metadata` tags, against the fixture's own key.
async fn tagged_turn(f: &Fixture, request: &str, tag: &str, value: &str, tokens: i64) {
    let hold_key = format!("hold-{request}");
    f.wallet
        .hold(&hold_key, "", 1_000_000, ON)
        .await
        .expect("the hold reserves");
    let receipt = f
        .wallet
        .settle(&hold_key, "", 100_000, ON)
        .await
        .expect("the turn settles");
    let mut usage = UsageRecord {
        input_tokens: tokens,
        output_tokens: 0,
        ..Default::default()
    };
    usage.tags.insert(tag.to_owned(), value.to_owned());
    f.db.record_usage(&UsageRow {
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id: Some(f.key_id),
        model: "mock".to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        usage,
        charged_minor: 100_000,
        freeze_minor: 1_000_000,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    })
    .await
    .expect("the usage row is written");
}

/// The scope column on a key's window rows: the tags a settle carries land it in
/// its per-value bucket beside the plain one — and `tpm_window_scoped` reads
/// exactly that bucket back.
#[tokio::test]
async fn a_tagged_settle_accrues_its_scoped_bucket() {
    let url = db_or_skip!();
    let f = fixture(&url, "tpmscope").await;

    // `team` fans out per value (`each`), `tier` pins one value (`concrete`),
    // `region` aggregates (`total`) — all on the one key.
    let mut scopes = oxsum_core::TpmScopes::new();
    scopes.insert("team".to_owned(), oxsum_core::TpmScope::Each(1000));
    scopes.insert(
        "tier".to_owned(),
        oxsum_core::TpmScope::Concrete {
            concrete: "free".to_owned(),
            tokens_per_minute: 1000,
        },
    );
    scopes.insert("region".to_owned(), oxsum_core::TpmScope::Total(1000));
    sqlx::query("UPDATE oxsum.api_keys SET tokens_per_minute_scopes = $1 WHERE key_id = $2")
        .bind(serde_json::to_value(&scopes).unwrap())
        .bind(f.key_id)
        .execute(&f.pool)
        .await
        .unwrap();

    // `team=red`, `tier=free`, `region=us`: every scope selects, so the settle
    // fans into three buckets beside the plain window.
    let mut usage = UsageRecord {
        input_tokens: 10,
        output_tokens: 2,
        ..Default::default()
    };
    usage.tags.insert("team".to_owned(), "red".to_owned());
    usage.tags.insert("tier".to_owned(), "free".to_owned());
    usage.tags.insert("region".to_owned(), "us".to_owned());
    let receipt = {
        let hold_key = "hold-scoped";
        f.wallet.hold(hold_key, "", 1_000_000, ON).await.unwrap();
        f.wallet.settle(hold_key, "", 100_000, ON).await.unwrap()
    };
    f.db.record_usage(&UsageRow {
        request_id: format!("scoped-{}", Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id: Some(f.key_id),
        model: "mock".to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        usage,
        charged_minor: 100_000,
        freeze_minor: 1_000_000,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    })
    .await
    .unwrap();

    let (plain, _) = f.db.tpm_window(f.key_id).await.unwrap();
    assert_eq!(plain, 12, "the plain window still counts every settle");
    let (red, _) =
        f.db.tpm_window_scoped(f.key_id, "tag:team=red")
            .await
            .unwrap();
    assert_eq!(red, 12, "the each bucket fanned out per value");
    let (free, _) =
        f.db.tpm_window_scoped(f.key_id, "tag:tier=free")
            .await
            .unwrap();
    assert_eq!(free, 12, "the concrete bucket pinned its value");
    let (region, _) =
        f.db.tpm_window_scoped(f.key_id, "tag:region")
            .await
            .unwrap();
    assert_eq!(region, 12, "the total bucket aggregated");
    // A value never carried never accrues.
    let (blue, _) =
        f.db.tpm_window_scoped(f.key_id, "tag:team=blue")
            .await
            .unwrap();
    assert_eq!(blue, 0, "no settle carried blue");
}

/// A settle that selects no scope — an untagged turn, or a tag the key does not
/// scope — writes only the plain window row.
#[tokio::test]
async fn a_turn_beyond_its_scopes_writes_only_the_plain_window() {
    let url = db_or_skip!();
    let f = fixture(&url, "tpmpartial").await;

    let mut scopes = oxsum_core::TpmScopes::new();
    scopes.insert(
        "tier".to_owned(),
        oxsum_core::TpmScope::Concrete {
            concrete: "free".to_owned(),
            tokens_per_minute: 1000,
        },
    );
    sqlx::query("UPDATE oxsum.api_keys SET tokens_per_minute_scopes = $1 WHERE key_id = $2")
        .bind(serde_json::to_value(&scopes).unwrap())
        .bind(f.key_id)
        .execute(&f.pool)
        .await
        .unwrap();

    // `tier=pro` is not the pinned value: it accrues nowhere but the plain
    // window, even though the tag itself is scoped.
    tagged_turn(&f, "pro", "tier", "pro", 10).await;
    let (free, _) =
        f.db.tpm_window_scoped(f.key_id, "tag:tier=free")
            .await
            .unwrap();
    assert_eq!(free, 0, "the pinned value never saw the turn");
    let (plain, _) = f.db.tpm_window(f.key_id).await.unwrap();
    assert_eq!(plain, 10);
}
