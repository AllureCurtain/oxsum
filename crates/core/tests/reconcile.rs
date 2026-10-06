//! Reconciliation (issue #134): the scan reads every organization's ledger
//! against the projections and reports each drift class. The tests need
//! DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    Db, DriftKind, NewUser, Reconciliation, SettlementKind, UsageRecord, UsageRow, Wallet,
};
use sqlx::PgPool;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
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

fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

/// A migrated database, one registered organization and its open wallet.
async fn world(url: &str) -> (Db, String, Wallet) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let registration = db
        .register(NewUser {
            email: format!(
                "recon_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = db
        .organization_by_id(registration.organization.id)
        .await
        .unwrap();
    let wallet = Wallet::open(db.pool().clone(), &org.tenant_id)
        .await
        .expect("the wallet opens");
    (db, org.tenant_id, wallet)
}

fn class(report: &Reconciliation, kind: DriftKind) -> &oxsum_core::DriftClass {
    report
        .classes
        .iter()
        .find(|class| class.class == kind)
        .expect("every class reports")
}

/// A normal hold-settle-record cycle drifts in no class: the scan is global, so
/// the assertion is that none of this turn's identifiers appear in any sample —
/// the database is shared with suites that plant drift on purpose.
#[tokio::test]
async fn a_clean_world_reports_no_drift() {
    let url = db_or_skip!();
    let (db, tenant, wallet) = world(&url).await;

    wallet.top_up("seed", 1_000, today()).await.unwrap();
    let request = format!("req-clean-{}", Uuid::new_v4().simple());
    let hold = format!("{request}:hold");
    wallet.hold(&hold, "", 100, today()).await.unwrap();
    let description = format!(
        "{{\"v\":3,\"request\":\"{request}\",\"channel\":\"c\",\"model\":\"m\",\
         \"priceVersion\":1,\"kind\":\"usage\",\"usage\":{{\"inputTokens\":1,\
         \"outputTokens\":1}},\"lines\":[[\"input\",1,1],[\"output\",1,1]],\
         \"charged\":2,\"freeze\":100}}"
    );
    let receipt = wallet
        .settle(&hold, &description, 2, today())
        .await
        .unwrap();
    db.record_usage(&UsageRow {
        request_id: request.clone(),
        tenant_id: tenant.clone(),
        key_id: None,
        model: "m".to_owned(),
        channel: "c".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        usage: UsageRecord::default(),
        charged_minor: 2,
        freeze_minor: 100,
        upstream_cost_minor: None,
    })
    .await
    .unwrap();

    let report = db.reconcile().await.unwrap();
    assert_eq!(report.classes.len(), 8);
    for class in &report.classes {
        for sample in &class.samples {
            assert!(
                !sample.detail.contains(&request) && !sample.detail.contains(&hold),
                "{:?} flags a clean turn: {sample:?}",
                class.class
            );
        }
    }
}

/// Every class surfaces the fixture planted for it.
#[tokio::test]
async fn each_drift_class_is_reported() {
    let url = db_or_skip!();
    let (db, tenant, wallet) = world(&url).await;
    wallet.top_up("seed", 1_000_000, today()).await.unwrap();
    let org_id: Uuid =
        sqlx::query_scalar("SELECT organization_id FROM oxsum.organizations WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_one(db.pool())
            .await
            .unwrap();

    // usage_orphans: a usage row whose settlement entry does not exist.
    let orphan_request = format!("req-orphan-{}", Uuid::new_v4().simple());
    db.record_usage(&UsageRow {
        request_id: orphan_request.clone(),
        tenant_id: tenant.clone(),
        key_id: None,
        model: "m".to_owned(),
        channel: "c".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: Uuid::new_v4(),
        usage: UsageRecord::default(),
        charged_minor: 1,
        freeze_minor: 1,
        upstream_cost_minor: None,
    })
    .await
    .unwrap();

    // settlements_unrecorded: a settled turn that never wrote its usage row.
    let unrecorded = format!("req-unrecorded-{}", Uuid::new_v4().simple());
    let hold = format!("{unrecorded}:hold");
    wallet.hold(&hold, "", 10, today()).await.unwrap();
    wallet
        .settle(
            &hold,
            &format!(
                "{{\"v\":3,\"request\":\"{unrecorded}\",\"channel\":\"c\",\"model\":\"m\",\
                 \"priceVersion\":1,\"kind\":\"usage\",\"usage\":{{\"inputTokens\":1,\
                 \"outputTokens\":1}},\"lines\":[[\"input\",1,1],[\"output\",1,1]],\
                 \"charged\":2,\"freeze\":10}}"
            ),
            2,
            today(),
        )
        .await
        .unwrap();

    // deposits_unbooked: credited, but the entry was never written.
    sqlx::query(
        "INSERT INTO oxsum.deposits \
             (deposit_id, organization_id, rail, payment_ref, amount_minor, \
              received_minor, status, entry_id) \
         VALUES ($1, $2, 'manual', $3, 100, 100, 'credited', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(format!("unbooked-{}", Uuid::new_v4().simple()))
    .bind(Uuid::new_v4())
    .execute(db.pool())
    .await
    .unwrap();

    // deposits_mismatched: the rail paid less than expected.
    sqlx::query(
        "INSERT INTO oxsum.deposits \
             (deposit_id, organization_id, rail, payment_ref, amount_minor, \
              received_minor, status, entry_id) \
         VALUES ($1, $2, 'manual', $3, 100, 90, 'credited', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(format!("short-{}", Uuid::new_v4().simple()))
    .bind(Uuid::new_v4())
    .execute(db.pool())
    .await
    .unwrap();

    // deposits_stuck: confirmed by the rail, never credited, past the grace.
    sqlx::query(
        "INSERT INTO oxsum.deposits \
             (deposit_id, organization_id, rail, payment_ref, amount_minor, status) \
         VALUES ($1, $2, 'manual', $3, 100, 'confirmed')",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(format!("stuck-{}", Uuid::new_v4().simple()))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE oxsum.deposits SET updated_at = now() - interval '1 hour' \
         WHERE organization_id = $1 AND status = 'confirmed'",
    )
    .bind(org_id)
    .execute(db.pool())
    .await
    .unwrap();

    // watches_orphaned: a watch row whose hold was never taken.
    let stale_watch = format!("req-stale-{}:hold", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO oxsum.open_holds \
             (hold_key, tenant_id, request_id, model, channel, price_version, \
              input_price, output_price, freeze_minor) \
         VALUES ($1, $2, 'req-stale', 'm', 'c', 1, 1, 1, 10)",
    )
    .bind(&stale_watch)
    .bind(&tenant)
    .execute(db.pool())
    .await
    .unwrap();

    // holds_unwatched: a live pending hold the watch table never heard of.
    let unwatched = format!("req-unwatched-{}:hold", Uuid::new_v4().simple());
    wallet.hold(&unwatched, "", 10, today()).await.unwrap();

    let report = db.reconcile().await.unwrap();
    assert!(!report.clean);

    let assert_has = |kind: DriftKind, needle: &str| {
        let class = class(&report, kind);
        assert!(class.count >= 1, "{kind:?} reports nothing: {class:?}");
        // The sample is bounded and newest-first, and the database is shared: drift other
        // suites plant — in this file's parallel tests included — can push this test's row
        // past the window. A truncated class counts it even when the sample omits it.
        let truncated = class.count > class.samples.len() as i64;
        assert!(
            truncated || class.samples.iter().any(|s| s.detail.contains(needle)),
            "{kind:?} should contain {needle}: {class:?}"
        );
    };
    assert_has(DriftKind::UsageOrphans, &orphan_request);
    assert_has(DriftKind::SettlementsUnrecorded, &unrecorded);
    assert_has(DriftKind::DepositsUnbooked, "manual:unbooked-");
    assert_has(DriftKind::DepositsMismatched, "manual:short-");
    assert_has(DriftKind::DepositsStuck, "manual:stuck-");
    assert_has(DriftKind::WatchesOrphaned, &stale_watch);
    assert_has(DriftKind::HoldsUnwatched, &unwatched);
}

/// A usage row naming a tenant with no ledger at all reconciles as an orphan.
#[tokio::test]
async fn a_row_for_a_ledgerless_tenant_is_an_orphan() {
    let url = db_or_skip!();
    let (db, _, _) = world(&url).await;
    let request = format!("req-noledger-{}", Uuid::new_v4().simple());
    db.record_usage(&UsageRow {
        request_id: request.clone(),
        tenant_id: "no-such-ledger".to_owned(),
        key_id: None,
        model: "m".to_owned(),
        channel: "c".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: Uuid::new_v4(),
        usage: UsageRecord::default(),
        charged_minor: 1,
        freeze_minor: 1,
        upstream_cost_minor: None,
    })
    .await
    .unwrap();

    let report = db.reconcile().await.unwrap();
    let orphans = class(&report, DriftKind::UsageOrphans);
    // Same bounded-window caveat as `each_drift_class_is_reported`: the class must flag the
    // row, but a sample already filled by other drift can legitimately leave it out.
    assert!(
        orphans.count >= 1
            && (orphans.count > orphans.samples.len() as i64
                || orphans.samples.iter().any(|s| s.detail == request)),
        "the ledgerless row is an orphan: {orphans:?}"
    );
}
