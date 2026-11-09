//! Bounded application publication, durable authorization and original-byte recovery.
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
    let url = oxsum_testkit::suite_db_url("application_publication")?;
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
            IntegrationScope::EvidenceWrite,
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
fn request(days: i64) -> oxsum_core::ApplicationEvidencePublishRequest {
    let day = (OffsetDateTime::now_utc().date() - Duration::days(days)).to_string();
    oxsum_core::ApplicationEvidencePublishRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        kind: oxsum_core::EvidenceKind::Period,
        from: Some(day.clone()),
        to: Some(day),
        statement_id: None,
    }
}
#[tokio::test]
async fn publication_retries_original_bytes_across_rotation_and_one_connection() {
    let Some(db) = db().await else { return };
    let (app, service, org, tenant) = fixture(&db).await;
    let req = request(1);
    let key = oxsum_core::signing_key([61; 32]).unwrap();
    let (a, b) = tokio::join!(
        db.publish_application_evidence(&service, org, &req, Some(&key)),
        db.publish_application_evidence(&service, org, &req, Some(&key))
    );
    let first = a.unwrap();
    let second = b.unwrap();
    assert_eq!(first.request_id, second.request_id);
    assert_eq!(first.bundle.bundle_id, second.bundle.bundle_id);
    let bytes = db
        .application_evidence_json(&service, org, first.bundle.bundle_id)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["scope"]["upstreamCostIncluded"], false);
    assert!(value["note"].is_string());
    let wallet = Wallet::open_existing(db.pool().clone(), &tenant)
        .await
        .unwrap();
    let before = wallet.log_size().await.unwrap();
    let mut changed = req.clone();
    changed.from = request(2).from;
    assert!(matches!(
        db.publish_application_evidence(&service, org, &changed, Some(&key))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let new = grant(
        &db,
        app,
        vec![
            IntegrationScope::EvidenceWrite,
            IntegrationScope::EvidenceRead,
        ],
    )
    .await;
    db.revoke_integration_credential(
        &setup::owner(&db).await,
        service.credential_id(),
        &IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotation".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.publish_application_evidence(&service, org, &req, Some(&key))
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    let single = Db::from_pool(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(db.pool().connect_options().as_ref().clone())
            .await
            .unwrap(),
    );
    let replay = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        single.publish_application_evidence(&new, org, &req, Some(&key)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(replay.request_id, first.request_id);
    assert_eq!(
        single
            .application_evidence_json(&new, org, replay.bundle.bundle_id)
            .await
            .unwrap(),
        bytes
    );
    assert_eq!(wallet.log_size().await.unwrap(), before);
    let next = request(2);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        single.publish_application_evidence(&new, org, &next, Some(&key)),
    )
    .await
    .unwrap()
    .unwrap();
}
#[tokio::test]
async fn publication_scope_dates_private_capture_and_archive_fail_closed() {
    let Some(db) = db().await else { return };
    let (app, service, org, tenant) = fixture(&db).await;
    let req = request(1);
    let read = grant(&db, app, vec![IntegrationScope::EvidenceRead]).await;
    assert!(matches!(
        db.publish_application_evidence(&read, org, &req, None)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(
        db.create_integration_credential(
            &setup::owner(&db).await,
            &setup::request(vec![org], vec![IntegrationScope::EvidenceWrite])
        )
        .await
        .is_err()
    );
    let (_, foreign, other, _) = fixture(&db).await;
    assert!(matches!(
        db.publish_application_evidence(&foreign, org, &req, None)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.publish_application_evidence(&service, other, &req, None)
            .await,
        Err(WalletError::NotFound(_))
    ));
    for days in [0, 32] {
        let mut bad = req.clone();
        if days == 0 {
            bad.to = Some(OffsetDateTime::now_utc().date().to_string());
        } else {
            bad.from = request(days).from;
        }
        assert!(matches!(
            db.publish_application_evidence(&service, org, &bad, None)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    let missing = oxsum_core::ApplicationEvidencePublishRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        kind: oxsum_core::EvidenceKind::Statement,
        from: None,
        to: None,
        statement_id: Some(Uuid::new_v4()),
    };
    assert!(matches!(
        db.publish_application_evidence(&service, org, &missing, None)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let day = OffsetDateTime::now_utc().date() - Duration::days(1);
    let private = db
        .mint_period_evidence(&wallet, org, &tenant, day, day, true, None)
        .await
        .unwrap();
    let bytes = db
        .evidence_bundle_json(private.bundle_id)
        .await
        .unwrap()
        .unwrap();
    let size = wallet.log_size().await.unwrap();
    assert!(matches!(
        db.publish_application_evidence(&service, org, &req, None)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(
        db.evidence_bundle_json(private.bundle_id)
            .await
            .unwrap()
            .unwrap(),
        bytes
    );
    assert_eq!(wallet.log_size().await.unwrap(), size);
    db.archive_application_customer(
        &service,
        org,
        &ApplicationCustomerArchiveRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "archive".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.publish_application_evidence(&service, org, &request(2), None)
            .await,
        Err(WalletError::Conflict(_))
    ));
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=clock_timestamp()-interval '2 days',expires_at=clock_timestamp()-interval '1 day' WHERE credential_id=$1").bind(service.credential_id()).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.publish_application_evidence(&service, org, &req, None)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}
#[tokio::test]
async fn failed_descriptor_recovers_same_marker_and_rejects_oversized_capture() {
    let Some(db) = db().await else { return };
    let (_, service, org, tenant) = fixture(&db).await;
    let req = request(1);
    let constraint = format!("fixture_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("ALTER TABLE oxsum.evidence_bundles ADD CONSTRAINT {constraint} CHECK (organization_id<>'{org}'::uuid) NOT VALID")).execute(db.pool()).await.unwrap();
    assert!(
        db.publish_application_evidence(&service, org, &req, None)
            .await
            .is_err()
    );
    let wallet = Wallet::open_existing(db.pool().clone(), &tenant)
        .await
        .unwrap();
    let size = wallet.log_size().await.unwrap();
    assert_eq!(size, 1);
    sqlx::query(&format!(
        "ALTER TABLE oxsum.evidence_bundles DROP CONSTRAINT {constraint}"
    ))
    .execute(db.pool())
    .await
    .unwrap();
    let result = db
        .publish_application_evidence(&service, org, &req, None)
        .await
        .unwrap();
    assert_eq!(wallet.log_size().await.unwrap(), size);
    assert!(
        db.application_evidence_json(&service, org, result.bundle.bundle_id)
            .await
            .is_ok()
    );
    let oversized = request(2);
    let scope = format!(
        "period/{org}/{}/{}",
        oversized.from.as_ref().unwrap(),
        oversized.to.as_ref().unwrap()
    );
    let value = serde_json::json!({"organization_id":org,"include_upstream_cost":false,"manifest":[],"artifacts":{"oversized":"a".repeat(4*1024*1024)},"entry_ids":[]});
    sqlx::query("INSERT INTO oxsum.evidence_mint_inputs(scope_key,organization_id,collector) VALUES($1,$2,$3)").bind(&scope).bind(org).bind(value).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.publish_application_evidence(&service, org, &oversized, None)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(wallet.log_size().await.unwrap(), size);
}
#[tokio::test]
async fn statement_publication_retains_real_paid_plan_evidence() {
    let Some(db) = db().await else { return };
    let (_, service, org, _) = fixture(&db).await;
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: "Evidence publication plan".into(),
                price_minor: 2_500_000,
                currency: "USD".into(),
                quota_minor: Some(10_000_000),
                period_kind: oxsum_core::PeriodKind::Anniversary,
                overage_enabled: false,
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
    let req = oxsum_core::ApplicationEvidencePublishRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        kind: oxsum_core::EvidenceKind::Statement,
        from: None,
        to: None,
        statement_id: Some(statement.statement_id),
    };
    let result = db
        .publish_application_evidence(
            &service,
            org,
            &req,
            Some(&oxsum_core::signing_key([62; 32]).unwrap()),
        )
        .await
        .unwrap();
    let bytes = db
        .application_evidence_json(&service, org, result.bundle.bundle_id)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(!value["manifest"].as_array().unwrap().is_empty());
    assert!(value["entries"].as_array().unwrap().len() > 1);
}

#[tokio::test]
async fn publication_bounds_actual_collection_before_marker_and_keeps_private_frozen_inputs() {
    let Some(db) = db().await else { return };
    let (_, service, org, tenant) = fixture(&db).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    // Customer adjustment history is included by the existing period contract.
    // More than the bound must refuse, never return a truncated complete claim.
    sqlx::query("INSERT INTO oxsum.credit_notes(credit_note_id,organization_id,amount_minor,reason,state) SELECT gen_random_uuid(),$1,1,'bounded fixture','issued' FROM generate_series(1,1001)")
        .bind(org).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.publish_application_evidence(&service, org, &request(1), None)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(wallet.log_size().await.unwrap(), 0);
    let (_, second, other, other_tenant) = fixture(&db).await;
    let req = request(2);
    let scope = format!(
        "period/{other}/{}/{}",
        req.from.as_ref().unwrap(),
        req.to.as_ref().unwrap()
    );
    let value = serde_json::json!({"organization_id":other,"include_upstream_cost":true,"manifest":[],"artifacts":{},"entry_ids":[]});
    sqlx::query("INSERT INTO oxsum.evidence_mint_inputs(scope_key,organization_id,collector) VALUES($1,$2,$3)").bind(&scope).bind(other).bind(value.clone()).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.publish_application_evidence(&second, other, &req, None)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let retained: Value =
        sqlx::query_scalar("SELECT collector FROM oxsum.evidence_mint_inputs WHERE scope_key=$1")
            .bind(&scope)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(retained, value);
    let other_wallet = Wallet::open_existing(db.pool().clone(), &other_tenant)
        .await
        .unwrap();
    assert_eq!(other_wallet.log_size().await.unwrap(), 0);
}

#[tokio::test]
async fn revocation_after_durable_authorization_blocks_response_not_accepted_work() {
    let Some(db) = db().await else { return };
    let (app, service, org, _) = fixture(&db).await;
    let req = request(1);
    let barrier: i64 = Uuid::new_v4().as_u128() as i64;
    let function = format!("wait_{}", Uuid::new_v4().simple());
    sqlx::raw_sql(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.organization_id='{org}'::uuid THEN PERFORM pg_advisory_xact_lock_shared({barrier}); END IF; RETURN NEW; END $$; CREATE TRIGGER {function} BEFORE INSERT ON oxsum.evidence_bundles FOR EACH ROW EXECUTE FUNCTION oxsum.{function}();")).execute(db.pool()).await.unwrap();
    let mut blocker = db.pool().begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(barrier)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let running_db = db.clone();
    let running_service = service.clone();
    let running_req = req.clone();
    let pending = tokio::spawn(async move {
        running_db
            .publish_application_evidence(&running_service, org, &running_req, None)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(pid)
            .fetch_one(db.pool())
            .await
            .unwrap();
            if blocked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let accepted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.application_evidence_publications WHERE organization_id=$1",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(accepted, 1);
    db.revoke_integration_credential(
        &setup::owner(&db).await,
        service.credential_id(),
        &IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "revoke accepted publication response".into(),
        },
    )
    .await
    .unwrap();
    blocker.commit().await.unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(WalletError::InvalidCredentials)
    ));
    let published: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.evidence_bundles WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(published, 1);
    let rotated = grant(
        &db,
        app,
        vec![
            IntegrationScope::EvidenceWrite,
            IntegrationScope::EvidenceRead,
        ],
    )
    .await;
    let result = db
        .publish_application_evidence(&rotated, org, &req, None)
        .await
        .unwrap();
    assert!(
        db.application_evidence_json(&rotated, org, result.bundle.bundle_id)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn concurrent_archive_before_request_commit_cannot_authorize_publication() {
    let Some(db) = db().await else { return };
    let (_, service, org, _) = fixture(&db).await;
    let req = request(1);
    let mut locked = db.pool().begin().await.unwrap();
    sqlx::query("SELECT state FROM oxsum.customer_profiles WHERE organization_id=$1 FOR UPDATE")
        .bind(org)
        .execute(&mut *locked)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *locked)
        .await
        .unwrap();
    let running_db = db.clone();
    let running_service = service.clone();
    let pending = tokio::spawn(async move {
        running_db
            .publish_application_evidence(&running_service, org, &req, None)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(pid)
            .fetch_one(db.pool())
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    sqlx::query("UPDATE oxsum.customer_profiles SET state='archived',archived_at=clock_timestamp() WHERE organization_id=$1").bind(org).execute(&mut *locked).await.unwrap();
    locked.commit().await.unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(WalletError::Conflict(_))
    ));
    let accepted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.application_evidence_publications WHERE organization_id=$1",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(accepted, 0);
}

#[tokio::test]
async fn oversized_source_field_is_refused_before_transfer_or_frozen_input() {
    let Some(db) = db().await else { return };
    let (_, service, org, tenant) = fixture(&db).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    sqlx::query("INSERT INTO oxsum.credit_notes(credit_note_id,organization_id,amount_minor,reason,state) VALUES($1,$2,1,repeat('x',4194305),'issued')")
        .bind(Uuid::new_v4()).bind(org).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.publish_application_evidence(&service, org, &request(1), None)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(wallet.log_size().await.unwrap(), 0);
    let frozen: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.evidence_mint_inputs WHERE organization_id=$1",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(frozen, 0);
}

#[tokio::test]
async fn oversized_statement_header_is_bounded_for_statement_and_period_capture() {
    let Some(db) = db().await else { return };
    let (_, service, org, tenant) = fixture(&db).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let statement = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.plan_statements(statement_id,organization_id,business_id,state,total_minor,currency,void_reason) VALUES($1,$2,$3,'void',0,'USD',repeat('x',4194305))")
        .bind(statement).bind(org).bind(db.default_business().await.unwrap()).execute(db.pool()).await.unwrap();
    let mut statement_request = request(1);
    statement_request.kind = oxsum_core::EvidenceKind::Statement;
    statement_request.from = None;
    statement_request.to = None;
    statement_request.statement_id = Some(statement);
    for req in [statement_request, request(1)] {
        assert!(matches!(
            db.publish_application_evidence(&service, org, &req, None)
                .await,
            Err(WalletError::Conflict(_))
        ));
    }
    assert_eq!(wallet.log_size().await.unwrap(), 0);
    let frozen: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.evidence_mint_inputs WHERE organization_id=$1",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(frozen, 0);
}
