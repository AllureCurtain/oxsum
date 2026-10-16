//! The dunning sweep (issue #258), against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! An opted-in organization whose finalized statement stands unpaid past
//! `due_date + dunning_grace_days` is suspended by the statements pass's sweep —
//! the flag set is `dunning_suspended_at`, the document flips to `suspended`, and
//! the `org.suspended` webhook fans out inside the same transaction. Once nothing
//! unpaid stands past its grace — the line repaid, or the automation switched
//! off — the sweep lifts the suspension it placed, never one the operator set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    Db, NewUser, ORG_SUSPENDED, PaymentStatus, SecretKey, SettlementKind, UsageRecord, UsageRow,
    Wallet, statement_period,
};
use sqlx::PgPool;
use time::macros::date;
use time::{Date, Duration, OffsetDateTime};
use uuid::Uuid;

const ONE: i64 = 1_000_000;
/// The billed month — September 2026 has fully ended whatever the real date is.
const SEPT: Date = date!(2026 - 09 - 15);
const PASSWORD: &str = "correct horse battery staple";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("dunning")
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

fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

/// A registered organization with its wallet: the smallest world a dunning
/// test needs — a billed statement it can owe.
struct Fixture {
    db: Db,
    org: oxsum_core::AdminOrganization,
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

/// One billed turn: a hold, a settlement, and the usage row the settlement's
/// writer would leave — enough that the statement owes on the credit line.
async fn turn(f: &Fixture, request: &str, freeze: i64, charged: i64, on: Date) {
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
    f.db.record_usage(&UsageRow {
        request_id: format!("{request}-{}", uuid::Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id: None,
        mandate_id: None,
        model: "gpt-4o".to_owned(),
        channel: "openai".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
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
        freeze_minor: freeze,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    })
    .await
    .expect("the usage row is written");
}

/// A finalized statement owing `charged` on the credit line, issued `terms`
/// days before `today` so it stands past due — the row a grace-lapsed sweep
/// suspends on.
async fn owing_statement(f: &Fixture, charged: i64, terms: i32) -> oxsum_core::Statement {
    f.wallet
        .set_credit_limit(&fresh("cl"), 50 * ONE, SEPT)
        .await
        .expect("the limit grants");
    turn(f, &fresh("r"), 5 * ONE, charged, SEPT).await;
    f.db.set_payment_terms(f.org.id, terms).await.unwrap();
    let draft =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-09").unwrap())
            .await
            .unwrap()
            .unwrap();
    // Issued `terms` days ago, so its due date lands today: the grace is what
    // the sweep counts past, not the issue date.
    f.db.finalize_statement(
        draft.id,
        &f.wallet,
        today() - Duration::days(i64::from(terms)),
    )
    .await
    .expect("the draft finalizes")
}

/// A lapsed statement suspends the opted-in organization: the marker names the
/// sweep as the suspender, the document stands `suspended`, and the webhook
/// announces the transition.
#[tokio::test]
async fn a_statement_past_grace_suspends_the_org() {
    let url = db_or_skip!();
    let f = fixture(&url, "lapse").await;
    f.db.set_dunning_grace(f.org.id, Some(5)).await.unwrap();
    let statement = owing_statement(&f, 3 * ONE, 30).await;
    // Issued 30 days ago on net-30 terms: due today, so today + 1 is past the
    // 5-day grace too.
    let sweep_day = today() + Duration::days(6);

    let (suspended, reinstated) = f.db.sweep_dunning(sweep_day).await.unwrap();
    assert_eq!((suspended, reinstated), (1, 0));

    let org = f.db.organization_by_id(f.org.id).await.unwrap();
    assert!(org.suspended_at.is_some());
    assert!(org.dunning_suspended_at.is_some());
    let standing = f.db.statement_by_id(statement.id).await.unwrap().unwrap();
    assert_eq!(standing.payment_status, PaymentStatus::Suspended);
    assert!(standing.suspended_at.is_some());

    // New holds refuse while the suspension stands — the admission gate is the
    // same one an operator's suspension closes.
    let err = f
        .wallet
        .hold("after", "", ONE, sweep_day)
        .await
        .unwrap_err();
    assert!(
        matches!(err, oxsum_core::WalletError::Forbidden(_)),
        "{err:?}"
    );
}

/// A suspension the sweep placed lifts once the line is repaid: reconcile
/// turns the statement `paid`, the next pass reinstates the organization, and
/// a suspension the operator set by hand is never the sweep's to undo.
#[tokio::test]
async fn a_repaid_line_lifts_the_suspension() {
    let url = db_or_skip!();
    let f = fixture(&url, "repaid").await;
    f.db.set_dunning_grace(f.org.id, Some(5)).await.unwrap();
    owing_statement(&f, 3 * ONE, 30).await;
    let sweep_day = today() + Duration::days(6);
    assert_eq!(f.db.sweep_dunning(sweep_day).await.unwrap(), (1, 0));

    // Repay the draw: the statement reconciles to paid, and the sweep lifts
    // the suspension it placed.
    f.wallet.top_up("pay", 3 * ONE, today()).await.unwrap();
    f.db.reconcile_statements(f.org.id, &f.wallet, sweep_day)
        .await
        .unwrap();
    let (suspended, reinstated) = f.db.sweep_dunning(sweep_day).await.unwrap();
    assert_eq!((suspended, reinstated), (0, 1));
    let org = f.db.organization_by_id(f.org.id).await.unwrap();
    assert!(org.suspended_at.is_none());
    assert!(org.dunning_suspended_at.is_none());
    // Admission is restored.
    f.wallet.hold("again", "", ONE, sweep_day).await.unwrap();
}

/// An organization without a configured grace never suspends — NULL is off —
/// and a negative grace is a validation error.
#[tokio::test]
async fn a_null_grace_never_suspends() {
    let url = db_or_skip!();
    let f = fixture(&url, "nullgrace").await;
    let err =
        f.db.set_dunning_grace(f.org.id, Some(-1))
            .await
            .unwrap_err();
    assert!(
        matches!(err, oxsum_core::WalletError::InvalidInput(_)),
        "{err:?}"
    );

    // No grace set: the statement stands past due, the org does not suspend.
    owing_statement(&f, 3 * ONE, 30).await;
    let (suspended, reinstated) =
        f.db.sweep_dunning(today() + Duration::days(90))
            .await
            .unwrap();
    assert_eq!((suspended, reinstated), (0, 0));
    let org = f.db.organization_by_id(f.org.id).await.unwrap();
    assert!(org.suspended_at.is_none());
}

/// A suspension the operator set by hand is never the sweep's to undo: the
/// marker is NULL on it, so a repaid line leaves the flag standing.
#[tokio::test]
async fn a_manual_suspension_is_not_the_sweeps_to_lift() {
    let url = db_or_skip!();
    let f = fixture(&url, "manual").await;
    f.db.set_suspended(f.org.id, true).await.unwrap();
    // Nothing is even owed — the manual flag simply stands.
    let (suspended, reinstated) = f.db.sweep_dunning(today()).await.unwrap();
    assert_eq!((suspended, reinstated), (0, 0));
    let org = f.db.organization_by_id(f.org.id).await.unwrap();
    assert!(org.suspended_at.is_some());
    assert!(org.dunning_suspended_at.is_none());
}

/// Switching the automation off hands the suspended organization back to the
/// operator: the sweep's own suspension lifts on the next pass, since no grace
/// stands to count against.
#[tokio::test]
async fn switching_grace_off_reinstates() {
    let url = db_or_skip!();
    let f = fixture(&url, "off").await;
    f.db.set_dunning_grace(f.org.id, Some(0)).await.unwrap();
    owing_statement(&f, 3 * ONE, 30).await;
    let sweep_day = today() + Duration::days(1);
    assert_eq!(f.db.sweep_dunning(sweep_day).await.unwrap(), (1, 0));

    // The admin turns the automation off — the sweep's own suspension lifts.
    f.db.set_dunning_grace(f.org.id, None).await.unwrap();
    let (suspended, reinstated) = f.db.sweep_dunning(sweep_day).await.unwrap();
    assert_eq!((suspended, reinstated), (0, 1));
    let org = f.db.organization_by_id(f.org.id).await.unwrap();
    assert!(org.suspended_at.is_none());
    assert!(org.dunning_suspended_at.is_none());
}

/// A statement inside its grace does not suspend — the sweep counts
/// `due_date + grace` strictly before `today`.
#[tokio::test]
async fn a_statement_inside_grace_stands() {
    let url = db_or_skip!();
    let f = fixture(&url, "grace").await;
    f.db.set_dunning_grace(f.org.id, Some(10)).await.unwrap();
    owing_statement(&f, 3 * ONE, 30).await;
    // Due today + 10 days of grace is not past yet on day 5.
    let (suspended, reinstated) =
        f.db.sweep_dunning(today() + Duration::days(5))
            .await
            .unwrap();
    assert_eq!((suspended, reinstated), (0, 0));
    let org = f.db.organization_by_id(f.org.id).await.unwrap();
    assert!(org.suspended_at.is_none());
}

/// The sweep's suspension fans the `org.suspended` webhook out inside the same
/// transaction, and names its reason — an organization watching for the signal
/// hears it the same way it hears the operator's.
#[tokio::test]
async fn the_suspension_webhooks_like_the_operators() {
    let url = db_or_skip!();
    let f = fixture(&url, "hook").await;
    let endpoint =
        f.db.create_webhook(
            f.org.id,
            "https://receiver.example/dunning",
            &[ORG_SUSPENDED.to_owned()],
            &SecretKey::from_bytes([9; 32]),
        )
        .await
        .unwrap();
    f.db.set_dunning_grace(f.org.id, Some(0)).await.unwrap();
    owing_statement(&f, 3 * ONE, 30).await;
    f.db.sweep_dunning(today() + Duration::days(1))
        .await
        .unwrap();

    let deliveries =
        f.db.webhook_deliveries(f.org.id, endpoint.endpoint.id)
            .await
            .unwrap()
            .expect("the endpoint is the organization's");
    assert_eq!(deliveries.len(), 1);
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM oxsum.webhook_deliveries WHERE delivery_id = $1")
            .bind(deliveries[0].id)
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(payload["type"], "org.suspended");
    assert_eq!(payload["data"]["reason"], "dunning");
}
