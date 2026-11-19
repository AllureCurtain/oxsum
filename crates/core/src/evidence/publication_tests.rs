//! Controlled same-scope descriptor races on the actual PostgreSQL indexes.
// This suite owns its isolated database and setup OnceCell.
#![allow(clippy::duplicate_mod, clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Collector, EvidenceKind};
use crate::{Db, NewUser, Wallet};

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn fixture() -> Option<(Db, Wallet, Uuid)> {
    let url = oxsum_testkit::suite_db_url("evidence_publication")?;
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "evidence publication fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let wallet = Wallet::open(db.pool().clone(), &user.organization.tenant_id)
        .await
        .unwrap();
    Some((db, wallet, user.organization.id))
}

async fn publish(
    db: &Db,
    wallet: &Wallet,
    org: Uuid,
    statement: Option<Uuid>,
) -> super::EvidenceBundleRow {
    let day = OffsetDateTime::now_utc().date() - time::Duration::days(1);
    db.commit_bundle(
        wallet,
        Collector::new(org, false),
        if statement.is_some() {
            EvidenceKind::Statement
        } else {
            EvidenceKind::Period
        },
        statement.is_none().then_some(day),
        statement.is_none().then_some(day),
        statement,
        None,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn simultaneous_statement_and_period_publishers_return_identical_retained_bytes() {
    let _guard = TEST_LOCK.lock().await;
    let Some((db, wallet, org)) = fixture().await else {
        return;
    };
    let barrier: i64 = 891_001;
    // This suite uses one database per nextest process. Both actual publishers
    // must reach the final INSERT after their early descriptor reads.
    sqlx::raw_sql(&format!(
        "CREATE FUNCTION oxsum.wait_evidence_publish() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock_shared({barrier}); RETURN NEW; END $$;
         CREATE TRIGGER wait_evidence_publish BEFORE INSERT ON oxsum.evidence_bundles
         FOR EACH ROW EXECUTE FUNCTION oxsum.wait_evidence_publish();"
    ))
    .execute(db.pool())
    .await
    .unwrap();
    for statement in [Some(Uuid::new_v4()), None] {
        let mut gate = db.pool().acquire().await.unwrap();
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(barrier)
            .execute(&mut *gate)
            .await
            .unwrap();
        let one_db = db.clone();
        let one_wallet = Wallet::open_existing(db.pool().clone(), wallet.tenant_id())
            .await
            .unwrap();
        let two_db = db.clone();
        let two_wallet = Wallet::open_existing(db.pool().clone(), wallet.tenant_id())
            .await
            .unwrap();
        let one = tokio::spawn(async move { publish(&one_db, &one_wallet, org, statement).await });
        let two = tokio::spawn(async move { publish(&two_db, &two_wallet, org, statement).await });
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND objid=$1::bigint::oid AND NOT granted")
                    .bind(barrier).fetch_one(&mut *gate).await.unwrap();
                if count == 2 { break }
                tokio::task::yield_now().await;
            }
        }).await;
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(barrier)
            .execute(&mut *gate)
            .await
            .unwrap();
        ready.expect("both publishers reached the actual insertion barrier");
        let (a, b) = tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(one, two) })
            .await
            .unwrap();
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(a.bundle_id, b.bundle_id);
        assert_eq!(a.manifest_digest, b.manifest_digest);
        assert_eq!(a.created_at, b.created_at);
        let bytes = db.evidence_bundle_json(a.bundle_id).await.unwrap().unwrap();
        let replay = publish(&db, &wallet, org, statement).await;
        assert_eq!(replay.created_at, a.created_at);
        assert_eq!(
            db.evidence_bundle_json(replay.bundle_id)
                .await
                .unwrap()
                .unwrap(),
            bytes
        );
        let row = sqlx::query(
            "SELECT count(*) AS n FROM oxsum.evidence_bundles WHERE organization_id=$1 AND kind=$2",
        )
        .bind(org)
        .bind(if statement.is_some() {
            "statement"
        } else {
            "period"
        })
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(row.get::<i64, _>("n"), 1);
    }
    sqlx::raw_sql("DROP TRIGGER wait_evidence_publish ON oxsum.evidence_bundles; DROP FUNCTION oxsum.wait_evidence_publish();").execute(db.pool()).await.unwrap();
}

#[tokio::test]
async fn a_different_marker_identity_is_not_adopted_after_publication() {
    let _guard = TEST_LOCK.lock().await;
    let Some((db, wallet, org)) = fixture().await else {
        return;
    };
    // Inject a competing identity at the publication boundary, after the
    // initial lookup. The reread must demand the expected marker, not any row.
    let foreign = Uuid::new_v4();
    sqlx::raw_sql(&format!(
        "CREATE FUNCTION oxsum.replace_evidence_id() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN NEW.bundle_id := '{foreign}'::uuid; RETURN NEW; END $$;
        CREATE TRIGGER replace_evidence_id BEFORE INSERT ON oxsum.evidence_bundles
        FOR EACH ROW EXECUTE FUNCTION oxsum.replace_evidence_id();"
    ))
    .execute(db.pool())
    .await
    .unwrap();
    let day = OffsetDateTime::now_utc().date() - time::Duration::days(1);
    let result = db
        .commit_bundle(
            &wallet,
            Collector::new(org, false),
            EvidenceKind::Period,
            Some(day),
            Some(day),
            None,
            None,
        )
        .await;
    assert!(result.is_err());
    assert!(db.evidence_bundle(org, foreign).await.unwrap().is_some());
    sqlx::raw_sql("DROP TRIGGER replace_evidence_id ON oxsum.evidence_bundles; DROP FUNCTION oxsum.replace_evidence_id();").execute(db.pool()).await.unwrap();
}

#[path = "../../tests/support/integration.rs"]
mod correction_setup;

#[tokio::test]
async fn posting_period_correction_capture_contains_both_ledger_sources() {
    let _guard = TEST_LOCK.lock().await;
    let (db, wallet, org) = fixture()
        .await
        .expect("correction evidence acceptance requires PostgreSQL");
    let owner = correction_setup::owner(&db).await;
    let day = OffsetDateTime::now_utc().date();
    wallet
        .top_up("credit-evidence-fund", 100, day)
        .await
        .unwrap();
    let request = Uuid::new_v4();
    let key = format!("req-{request}:hold");
    wallet
        .hold(
            &key,
            &crate::hold_description(&request.to_string(), "model", 100).unwrap(),
            100,
            day,
        )
        .await
        .unwrap();
    let description=serde_json::json!({"v":3,"request":request.to_string(),"channel":"fixture","model":"model","priceVersion":1,"kind":"usage","usage":{"inputTokens":100},"lines":[["input",100,1000000],["output",0,1000000]],"charged":100,"freeze":100}).to_string();
    let source = wallet.settle(&key, &description, 100, day).await.unwrap();
    let preview = db
        .preview_request_correction(&owner, org, *source.entry_id.as_uuid())
        .await
        .unwrap();
    let credit = db
        .apply_request_correction(
            &owner,
            org,
            &crate::RequestCorrectionInput {
                idempotency_key: "evidence-credit".into(),
                source_entry_id: *source.entry_id.as_uuid(),
                source_hash: preview.source_hash,
                expected_corrected_minor: 0,
                amount_minor: 30,
                reason: "Evidence fixture adjustment".into(),
            },
        )
        .await
        .unwrap();
    let mut collector = Collector::new(org, false);
    collector.bounded = true;
    // Exercise the same capture/publication boundary with today's fixture; the
    // public mint API separately refuses periods whose end has not closed.
    db.collect_orders_and_adjustments(&mut collector, org, day, day)
        .await
        .unwrap();
    assert_eq!(
        collector.artifacts["requestCorrection"][credit.id.to_string()]["row"]["amountMinor"],
        30
    );
    let bundle = db
        .commit_bundle(
            &wallet,
            collector,
            EvidenceKind::Period,
            Some(day),
            Some(day),
            None,
            None,
        )
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(
        &db.evidence_bundle_json(bundle.bundle_id)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let serialized = value["entries"].to_string();
    assert!(serialized.contains(&source.entry_id.to_string()));
    assert!(serialized.contains(&credit.entry_id.to_string()));
}
