//! Original-byte application evidence boundaries and zero-write reads.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    ActingService, ApplicationCustomerArchiveRequest, Db, IntegrationApplicationRequest,
    IntegrationCredentialRevokeRequest, IntegrationScope, ProvisionCustomerRequest, Wallet,
    WalletError,
};
use serde_json::Value;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("application_evidence")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
async fn grant(db: &Db, app: Uuid, scopes: Vec<IntegrationScope>) -> ActingService {
    let mut req = setup::request(vec![], scopes);
    req.application_id = Some(app);
    let made = db
        .create_integration_credential(&setup::owner(db).await, &req)
        .await
        .unwrap();
    db.authenticate_service(made.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap()
}
async fn fixture(db: &Db) -> (Uuid, ActingService, Uuid, String) {
    let app = db
        .create_integration_application(
            &setup::owner(db).await,
            &IntegrationApplicationRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                key: Uuid::new_v4().simple().to_string(),
                name: "Evidence application".into(),
                customer_limit: 10,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let service = grant(
        db,
        app,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::EvidenceRead,
            IntegrationScope::CustomersLifecycleWrite,
        ],
    )
    .await;
    let org = db
        .provision_application_customer(
            &service,
            &ProvisionCustomerRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                external_id: Uuid::new_v4().to_string(),
                name: "Evidence account".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    let tenant =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    (app, service, org, tenant)
}
async fn mint(db: &Db, org: Uuid, tenant: &str, days: i64, private: bool) -> Uuid {
    let wallet = Wallet::open(db.pool().clone(), tenant).await.unwrap();
    let day = OffsetDateTime::now_utc().date() - Duration::days(days);
    let key = oxsum_core::signing_key([54; 32]).unwrap();
    db.mint_period_evidence(&wallet, org, tenant, day, day, private, Some(&key))
        .await
        .unwrap()
        .bundle_id
}
async fn counts(db: &Db, org: Uuid) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM oxsum.evidence_bundles WHERE organization_id=$1),(SELECT count(*) FROM oxsum.evidence_share_tokens WHERE organization_id=$1),(SELECT count(*) FROM oxsum.admin_audit),(SELECT count(*) FROM oxsum.integration_customer_operations)").bind(org).fetch_one(db.pool()).await.unwrap()
}
#[tokio::test]
async fn statement_export_keeps_nonempty_original_financial_artifacts() {
    let Some(db) = db().await else { return };
    let (_, service, org, tenant) = fixture(&db).await;
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: "Evidence plan".into(),
                price_minor: 2_500_000,
                currency: "USD".into(),
                quota_minor: Some(10_000_000),
                period_kind: oxsum_core::PeriodKind::Anniversary,
                overage_enabled: true,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    let plan = db
        .bind_customer_plan(
            org,
            sold.plan_version_id,
            oxsum_core::PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    let order =
        payment::paid_order(&db, &setup::owner(&db).await, org, plan.customer_plan_id).await;
    payment::fulfill(&db, &order).await;
    let statement = db
        .plan_statements(org, None, 20)
        .await
        .unwrap()
        .statements
        .remove(0);
    let wallet = Wallet::open_existing(db.pool().clone(), &tenant)
        .await
        .unwrap();
    let key = oxsum_core::signing_key([55; 32]).unwrap();
    let bundle = db
        .mint_statement_evidence(
            &wallet,
            org,
            &tenant,
            statement.statement_id,
            false,
            Some(&key),
        )
        .await
        .unwrap();
    let original = db
        .evidence_bundle_json(bundle.bundle_id)
        .await
        .unwrap()
        .unwrap();
    let value: Value = serde_json::from_slice(&original).unwrap();
    assert!(!value["manifest"].as_array().unwrap().is_empty());
    assert!(value["entries"].as_array().unwrap().len() > 1);
    let before = counts(&db, org).await;
    let size = wallet.log_size().await.unwrap();
    assert_eq!(
        db.application_evidence_json(&service, org, bundle.bundle_id)
            .await
            .unwrap(),
        original
    );
    assert_eq!(counts(&db, org).await, before);
    assert_eq!(wallet.log_size().await.unwrap(), size);
}
#[tokio::test]
async fn original_signed_bytes_paginate_archive_and_one_connection_without_writes() {
    let Some(db) = db().await else { return };
    let (app, service, org, tenant) = fixture(&db).await;
    assert!(
        db.application_evidence_bundles(&service, org, None, 20)
            .await
            .unwrap()
            .bundles
            .is_empty()
    );
    let absent: Option<String> = sqlx::query_scalar("SELECT to_regnamespace($1)::text")
        .bind(format!("ledger_{tenant}"))
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(absent.is_none());
    let mut ids = [
        mint(&db, org, &tenant, 2, false).await,
        mint(&db, org, &tenant, 3, false).await,
        mint(&db, org, &tenant, 4, false).await,
    ];
    ids.sort();
    let first = db
        .application_evidence_bundles(&service, org, None, 2)
        .await
        .unwrap();
    assert_eq!(first.application_id, app);
    assert_eq!(first.organization_id, org);
    assert_eq!(first.next_cursor, Some(ids[1]));
    assert_eq!(
        first
            .bundles
            .iter()
            .map(|b| b.bundle_id)
            .collect::<Vec<_>>(),
        ids[..2]
    );
    let second = db
        .application_evidence_bundles(&service, org, first.next_cursor, 2)
        .await
        .unwrap();
    assert_eq!(second.bundles[0].bundle_id, ids[2]);
    assert!(second.next_cursor.is_none());
    let expected = db.evidence_bundle_json(ids[0]).await.unwrap().unwrap();
    let mut retained = expected.clone();
    retained.extend_from_slice(b"\n \n");
    sqlx::query("UPDATE oxsum.evidence_bundles SET bundle_json=$2 WHERE bundle_id=$1")
        .bind(ids[0])
        .bind(&retained)
        .execute(db.pool())
        .await
        .unwrap();
    let before = counts(&db, org).await;
    let wallet = Wallet::open_existing(db.pool().clone(), &tenant)
        .await
        .unwrap();
    let size = wallet.log_size().await.unwrap();
    let url = oxsum_testkit::suite_db_url("application_evidence").unwrap();
    let single = Db::connect(&url, 1).await.unwrap();
    let actual = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        single.application_evidence_json(&service, org, ids[0]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(actual, retained);
    assert_eq!(counts(&db, org).await, before);
    assert_eq!(wallet.log_size().await.unwrap(), size);
    db.archive_application_customer(
        &service,
        org,
        &ApplicationCustomerArchiveRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "archive retains proofs".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        single
            .application_evidence_json(&service, org, ids[0])
            .await
            .unwrap(),
        retained
    );
}
#[tokio::test]
async fn exact_scope_foreign_accounts_bundles_and_current_credentials() {
    let Some(db) = db().await else { return };
    let (app, service, org, tenant) = fixture(&db).await;
    let (_, foreign, other, other_tenant) = fixture(&db).await;
    let id = mint(&db, org, &tenant, 2, false).await;
    let foreign_id = mint(&db, other, &other_tenant, 2, false).await;
    assert!(matches!(
        db.application_evidence_json(&foreign, org, id).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.application_evidence_json(&service, org, foreign_id)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.application_evidence_bundles(&service, other, None, 20)
            .await,
        Err(WalletError::NotFound(_))
    ));
    for scope in [
        IntegrationScope::BillingRead,
        IntegrationScope::EntitlementsRead,
        IntegrationScope::CustomerLinkWrite,
    ] {
        let narrow = grant(&db, app, vec![scope]).await;
        assert!(matches!(
            db.application_evidence_json(&narrow, org, id).await,
            Err(WalletError::Forbidden(_))
        ));
    }
    assert!(
        db.create_integration_credential(
            &setup::owner(&db).await,
            &setup::request(vec![org], vec![IntegrationScope::EvidenceRead])
        )
        .await
        .is_err()
    );
    for limit in [0, 51] {
        assert!(matches!(
            db.application_evidence_bundles(&service, org, None, limit)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    let rotated = grant(&db, app, vec![IntegrationScope::EvidenceRead]).await;
    db.revoke_integration_credential(
        &setup::owner(&db).await,
        service.credential_id(),
        &IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotate".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.application_evidence_json(&service, org, id).await,
        Err(WalletError::InvalidCredentials)
    ));
    assert!(
        db.application_evidence_json(&rotated, org, id)
            .await
            .is_ok()
    );
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=clock_timestamp()-interval '2 days',expires_at=clock_timestamp()-interval '1 day' WHERE credential_id=$1").bind(rotated.credential_id()).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.application_evidence_bundles(&rotated, org, None, 20)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}
#[tokio::test]
async fn private_cost_malformed_identity_proof_and_oversized_retention_refuse() {
    let Some(db) = db().await else { return };
    let (_, service, org, tenant) = fixture(&db).await;
    let private = mint(&db, org, &tenant, 2, true).await;
    assert!(matches!(
        db.application_evidence_json(&service, org, private).await,
        Err(WalletError::Conflict(_))
    ));
    let id = mint(&db, org, &tenant, 3, false).await;
    let bytes = db.evidence_bundle_json(id).await.unwrap().unwrap();
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    for change in [
        "identity",
        "digest",
        "cost",
        "extra",
        "entry",
        "signature",
        "artifact",
        "optional-type",
        "key-type",
    ] {
        let mut value = original.clone();
        match change {
            "identity" => value["organizationId"] = serde_json::json!(Uuid::new_v4()),
            "digest" => value["manifestDigest"] = serde_json::json!("0".repeat(64)),
            "cost" => {
                value["artifacts"]["usageRecord"] =
                    serde_json::json!({"extra":{"upstreamCostMinor":1}})
            }
            "extra" => value["privateSupplierSecret"] = serde_json::json!("must not be served"),
            "entry" => value["entries"][0]["entry"]["description"] = serde_json::json!("tampered"),
            "signature" => value["note"] = serde_json::json!("invalid signature"),
            "optional-type" => value["statementId"] = serde_json::json!(123),
            "key-type" => {
                value["note"] = Value::Null;
                value["keyName"] = serde_json::json!(123);
                value["publicKey"] = Value::Null;
                value["keyHash"] = Value::Null;
            }
            _ => {
                value["artifacts"]["usageRecord"] = serde_json::json!({"extra":{"chargedMinor":2}})
            }
        }
        sqlx::query("UPDATE oxsum.evidence_bundles SET bundle_json=$2 WHERE bundle_id=$1")
            .bind(id)
            .bind(serde_json::to_vec(&value).unwrap())
            .execute(db.pool())
            .await
            .unwrap();
        assert!(
            matches!(
                db.application_evidence_json(&service, org, id).await,
                Err(WalletError::Conflict(_))
            ),
            "{change}"
        );
    }
    let duplicate = String::from_utf8(bytes.clone()).unwrap().replacen(
        "\"upstreamCostIncluded\":false",
        "\"upstreamCostIncluded\":true,\"upstreamCostIncluded\":false",
        1,
    );
    assert_ne!(duplicate.as_bytes(), bytes);
    sqlx::query("UPDATE oxsum.evidence_bundles SET bundle_json=$2 WHERE bundle_id=$1")
        .bind(id)
        .bind(duplicate.as_bytes())
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.application_evidence_json(&service, org, id).await,
        Err(WalletError::Conflict(_))
    ));
    for bad in [b"not json".to_vec(), vec![b' '; 16 * 1024 * 1024 + 1]] {
        sqlx::query("UPDATE oxsum.evidence_bundles SET bundle_json=$2 WHERE bundle_id=$1")
            .bind(id)
            .bind(bad)
            .execute(db.pool())
            .await
            .unwrap();
        assert!(matches!(
            db.application_evidence_json(&service, org, id).await,
            Err(WalletError::Conflict(_))
        ));
    }
    sqlx::query("UPDATE oxsum.evidence_bundles SET bundle_json=$2 WHERE bundle_id=$1")
        .bind(id)
        .bind(&bytes)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        db.application_evidence_json(&service, org, id)
            .await
            .unwrap(),
        bytes
    );
}
