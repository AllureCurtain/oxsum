//! Stable application isolation, exact replay, rotation and archival boundaries.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod support;

use oxsum_core::{
    ActingService, ApplicationCustomerArchiveRequest, Db, IntegrationApplication,
    IntegrationApplicationRequest, IntegrationScope, ProvisionCustomerRequest, WalletError,
};
use uuid::Uuid;

async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("application_accounts")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
async fn application(db: &Db, limit: i32) -> IntegrationApplication {
    db.create_integration_application(
        &support::owner(db).await,
        &IntegrationApplicationRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            key: Uuid::new_v4().simple().to_string(),
            name: "Application fixture".into(),
            customer_limit: limit,
            reason: "test isolated application".into(),
        },
    )
    .await
    .unwrap()
}
async fn service(db: &Db, app: Uuid, scopes: Vec<IntegrationScope>) -> (ActingService, Uuid) {
    let mut req = support::request(vec![], scopes);
    req.application_id = Some(app);
    let made = db
        .create_integration_credential(&support::owner(db).await, &req)
        .await
        .unwrap();
    (
        db.authenticate_service(made.secret.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap(),
        made.credential.id,
    )
}
fn provision(external: &str) -> ProvisionCustomerRequest {
    ProvisionCustomerRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        external_id: external.into(),
        name: "Service account".into(),
        contact_email: None,
    }
}
fn scopes() -> Vec<IntegrationScope> {
    vec![
        IntegrationScope::CustomersProvision,
        IntegrationScope::CustomersRead,
        IntegrationScope::BillingRead,
        IntegrationScope::CustomersLifecycleWrite,
    ]
}

#[tokio::test]
async fn same_external_identity_isolated_across_apps_rotation_preserves_mapping() {
    let Some(db) = db().await else { return };
    let a = application(&db, 10).await;
    let b = application(&db, 10).await;
    let (alice, _) = service(&db, a.application_id, scopes()).await;
    let (bob, _) = service(&db, b.application_id, scopes()).await;
    let request = provision("same-user");
    let x = db
        .provision_application_customer(&alice, &request)
        .await
        .unwrap();
    let y = db
        .provision_application_customer(&bob, &request)
        .await
        .unwrap();
    assert_ne!(x.customer.organization_id, y.customer.organization_id);
    assert_eq!(x.external_id, y.external_id);
    assert_ne!(x.customer.external_id, y.customer.external_id);
    assert!(matches!(
        db.integration_customer(&bob, x.customer.organization_id)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.integration_customer_tenant(&bob, x.customer.organization_id)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let (rotated, _) = service(&db, a.application_id, scopes()).await;
    assert_eq!(
        db.application_customer(&rotated, "same-user")
            .await
            .unwrap()
            .customer
            .organization_id,
        x.customer.organization_id
    );
    assert_eq!(
        db.provision_application_customer(&rotated, &request)
            .await
            .unwrap()
            .customer
            .organization_id,
        x.customer.organization_id
    );
    let page = db.integration_customers(&rotated, None, 10).await.unwrap();
    assert_eq!(page.customers.len(), 1);
    assert_eq!(
        page.customers[0].organization_id,
        x.customer.organization_id
    );
    let members: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.memberships WHERE organization_id=ANY($1)")
            .bind(vec![x.customer.organization_id, y.customer.organization_id])
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(members, 0);
    assert!(
        db.integration_customer_tenant(&rotated, x.customer.organization_id)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn exact_keys_concurrency_changed_reuse_and_capacity_are_atomic() {
    let Some(db) = db().await else { return };
    let app = application(&db, 1).await;
    let (actor, _) = service(&db, app.application_id, scopes()).await;
    let mut req = provision("one");
    let (a, b, c) = tokio::join!(
        db.provision_application_customer(&actor, &req),
        db.provision_application_customer(&actor, &req),
        db.provision_application_customer(&actor, &req)
    );
    let a = a.unwrap();
    assert_eq!(
        a.customer.organization_id,
        b.unwrap().customer.organization_id
    );
    assert_eq!(
        a.customer.organization_id,
        c.unwrap().customer.organization_id
    );
    req.name = "changed".into();
    assert!(matches!(
        db.provision_application_customer(&actor, &req).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.provision_application_customer(&actor, &provision("one"))
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.provision_application_customer(&actor, &provision("two"))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(a.customer.organization_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant)
        .await
        .unwrap();
    let day = time::OffsetDateTime::now_utc().date();
    wallet.top_up("fixture", 100, day).await.unwrap();
    wallet.hold("prior", "", 50, day).await.unwrap();
    let archive = ApplicationCustomerArchiveRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "close application user".into(),
    };
    let closed = db
        .archive_application_customer(&actor, a.customer.organization_id, &archive)
        .await
        .unwrap();
    assert_eq!(closed.customer.state, "archived");
    assert!(matches!(
        wallet.hold("after-archive", "", 1, day).await,
        Err(WalletError::Forbidden(_))
    ));
    wallet.settle("prior", "", 20, day).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 80);
    assert_eq!(wallet.reserved().await.unwrap(), 0);

    assert!(closed.customer.suspended);
    assert!(
        db.archive_application_customer(&actor, a.customer.organization_id, &archive)
            .await
            .unwrap()
            .customer
            .suspended
    );
    assert!(matches!(
        db.provision_application_customer(&actor, &provision("two"))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.application_customers WHERE application_id=$1",
    )
    .bind(app.application_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn scope_revocation_and_owner_selected_grants_never_adopt_existing_customers() {
    let Some(db) = db().await else { return };
    let app = application(&db, 10).await;
    let (writer, id) = service(
        &db,
        app.application_id,
        vec![IntegrationScope::CustomersProvision],
    )
    .await;
    let made = db
        .provision_application_customer(&writer, &provision("source"))
        .await
        .unwrap();
    assert!(matches!(
        db.application_customer(&writer, "source").await,
        Err(WalletError::Forbidden(_))
    ));
    let archive = ApplicationCustomerArchiveRequest {
        idempotency_key: "forbidden".into(),
        reason: "no archive scope".into(),
    };
    assert!(matches!(
        db.archive_application_customer(&writer, made.customer.organization_id, &archive)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    let (reader, _) = service(
        &db,
        app.application_id,
        vec![IntegrationScope::CustomersRead],
    )
    .await;
    assert!(matches!(
        db.provision_application_customer(&reader, &provision("denied"))
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.provision_integration_customer(&writer, &provision("wrong-route"))
            .await,
        Err(WalletError::Forbidden(_))
    ));
    let mut request = support::request(vec![made.customer.organization_id], scopes());
    request.application_id = Some(app.application_id);
    assert!(matches!(
        db.create_integration_credential(&support::owner(&db).await, &request)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    request.application_id = None;
    request.scopes = vec![IntegrationScope::CustomersLifecycleWrite];
    assert!(matches!(
        db.create_integration_credential(&support::owner(&db).await, &request)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    db.revoke_integration_credential(
        &support::owner(&db).await,
        id,
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotate".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.provision_application_customer(&writer, &provision("revoked"))
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn legacy_provisioning_now_pins_request_keys_and_cannot_claim_allowlisted_identity() {
    let Some(db) = db().await else { return };
    let made = support::mint(&db, vec![], vec![IntegrationScope::CustomersProvision]).await;
    let service = db
        .authenticate_service(made.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut req = provision(&Uuid::new_v4().to_string());
    let first = db
        .provision_integration_customer(&service, &req)
        .await
        .unwrap();
    req.name = "changed terms".into();
    assert!(matches!(
        db.provision_integration_customer(&service, &req).await,
        Err(WalletError::Conflict(_))
    ));
    req.name = "Service account".into();
    req.external_id = Uuid::new_v4().to_string();
    assert!(matches!(
        db.provision_integration_customer(&service, &req).await,
        Err(WalletError::Conflict(_))
    ));
    let other = support::mint(
        &db,
        vec![first.organization_id],
        vec![IntegrationScope::CustomersProvision],
    )
    .await;
    let other = db
        .authenticate_service(other.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        db.provision_integration_customer(&other, &provision(&first.external_id))
            .await,
        Err(WalletError::Conflict(_))
    ));
}

#[tokio::test]
async fn operation_insert_failure_rolls_back_account_and_namespace_slot() {
    let Some(db) = db().await else { return };
    let app = application(&db, 1).await;
    let (actor, _) = service(&db, app.application_id, scopes()).await;
    sqlx::query("CREATE FUNCTION oxsum.test_application_abort() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN IF NEW.namespace_id::text=TG_ARGV[0] THEN RAISE EXCEPTION ''controlled failure''; END IF; RETURN NEW; END'").execute(db.pool()).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER test_application_abort BEFORE INSERT ON oxsum.integration_customer_operations FOR EACH ROW EXECUTE FUNCTION oxsum.test_application_abort('{}')",app.application_id)).execute(db.pool()).await.unwrap();
    let req = provision("rollback");
    assert!(
        db.provision_application_customer(&actor, &req)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.application_customers WHERE application_id=$1",
    )
    .bind(app.application_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DROP TRIGGER test_application_abort ON oxsum.integration_customer_operations")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.provision_application_customer(&actor, &req)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn metering_operation_identity_survives_application_credential_rotation() {
    let Some(db) = db().await else { return };
    let app = application(&db, 10).await;
    let capabilities = vec![
        IntegrationScope::CustomersProvision,
        IntegrationScope::CustomersRead,
        IntegrationScope::MeteringWrite,
    ];
    let (first, credential) = service(&db, app.application_id, capabilities.clone()).await;
    let customer = db
        .provision_application_customer(&first, &provision("metered"))
        .await
        .unwrap();
    let org = customer.customer.organization_id;
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let wallet = tenants.get(&tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();
    wallet.top_up("fixture", 1000, on).await.unwrap();
    let channel = Uuid::new_v4().simple().to_string();
    db.set_channel(
        &channel,
        "http://127.0.0.1:9/unused",
        "fixture",
        "openai",
        &oxsum_core::SecretKey::from_bytes([63; 32]),
    )
    .await
    .unwrap();
    let price:oxsum_core::Price=serde_json::from_value(serde_json::json!({"inputPricePerMillion":1000000,"outputPricePerMillion":0,"maxOutputTokens":0,"mode":"event"})).unwrap();
    db.append_price(&channel, "event", price, 1).await.unwrap();
    let usage = oxsum_core::UsageRecord {
        input_tokens: 100,
        event_type: Some("event".into()),
        ..Default::default()
    };
    let event = oxsum_core::MeteredEvent {
        channel,
        billable_code: "event".into(),
        event_id: None,
        usage: usage.clone(),
    };
    let hold = db
        .metering_hold(&tenants, &first, org, "operation", &event, on)
        .await
        .unwrap();
    db.revoke_integration_credential(
        &support::owner(&db).await,
        credential,
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "replace runtime credential".into(),
        },
    )
    .await
    .unwrap();
    let (replacement, _) = service(&db, app.application_id, capabilities).await;
    let same = db
        .metering_hold(&tenants, &replacement, org, "operation", &event, on)
        .await
        .unwrap();
    assert_eq!(hold.hold_key, same.hold_key);
    assert_eq!(wallet.reserved().await.unwrap(), 100);
    let settled = db
        .metering_settle(
            &tenants,
            &replacement,
            org,
            &hold.hold_key,
            None,
            &usage,
            on,
        )
        .await
        .unwrap();
    let replay = db
        .metering_settle(
            &tenants,
            &replacement,
            org,
            &hold.hold_key,
            None,
            &usage,
            on,
        )
        .await
        .unwrap();
    assert_eq!(settled.settlement_id, replay.settlement_id);
    assert_eq!(wallet.available().await.unwrap(), 900);
    let provenance: String = sqlx::query_scalar(&format!(
        "SELECT provenance_actor FROM ledger_{tenant}.entries WHERE entry_id=$1"
    ))
    .bind(oxsum_core::entry_id_for(&hold.hold_key).as_uuid())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(provenance, format!("svc-{}", credential.as_simple()));
    let frozen: serde_json::Value =
        sqlx::query_scalar("SELECT admission FROM oxsum.metering_authorizations WHERE hold_key=$1")
            .bind(&hold.hold_key)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(frozen["source_credential"], credential.to_string());
    assert_eq!(frozen["source_name"], "metering fixture");

    let mut changed = event;
    changed.usage.input_tokens = 99;
    assert!(matches!(
        db.metering_hold(&tenants, &replacement, org, "operation", &changed, on)
            .await,
        Err(WalletError::Conflict(_))
    ));
}

#[tokio::test]
async fn historical_provision_receipt_requires_original_actor_terms_and_identity() {
    let Some(db) = db().await else { return };
    let minted = support::mint(&db, vec![], vec![IntegrationScope::CustomersProvision]).await;
    let actor = db
        .authenticate_service(minted.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    let other = support::mint(&db, vec![], vec![IntegrationScope::CustomersProvision]).await;
    let other = db
        .authenticate_service(other.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    let id = Uuid::new_v4();
    let req = provision(&Uuid::new_v4().to_string());
    let business: Uuid =
        sqlx::query_scalar("SELECT business_id FROM oxsum.businesses WHERE singleton")
            .fetch_one(db.pool())
            .await
            .unwrap();
    // Reconstruct the pre-0114 application rows, never a human account or ledger.
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("INSERT INTO oxsum.organizations(organization_id,name,tenant_id,kind) VALUES($1,$2,$3,'personal')")
        .bind(id).bind(&req.name).bind(id.to_string()).execute(&mut *tx).await.unwrap();
    sqlx::query("DELETE FROM oxsum.customer_profiles WHERE organization_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO oxsum.customer_profiles(organization_id,business_id,external_id) VALUES($1,$2,$3)").bind(id).bind(business).bind(&req.external_id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO oxsum.integration_credential_customers(business_id,credential_id,organization_id) VALUES($1,$2,$3)").bind(business).bind(actor.credential_id()).bind(id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO oxsum.admin_audit(action,actor,target,detail,idempotency_key) VALUES('business.integration.provision',$1,$2,$3,$4)").bind(format!("service:{}",actor.credential_id())).bind(id.to_string()).bind(serde_json::json!(req)).bind(format!("provisioning:{}",req.idempotency_key)).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        db.provision_integration_customer(&other, &req).await,
        Err(WalletError::Conflict(_))
    ));
    let mut changed = provision(&req.external_id);
    changed.idempotency_key = req.idempotency_key.clone();
    changed.name = "changed".into();
    assert!(matches!(
        db.provision_integration_customer(&actor, &changed).await,
        Err(WalletError::Conflict(_))
    ));
    let recovered = db
        .provision_integration_customer(&actor, &req)
        .await
        .unwrap();
    assert_eq!(recovered.organization_id, id);
    assert_eq!(
        db.provision_integration_customer(&actor, &req)
            .await
            .unwrap()
            .organization_id,
        id
    );
    let operations:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.integration_customer_operations WHERE namespace_id=$1 AND idempotency_key=$2").bind(actor.credential_id()).bind(&req.idempotency_key).fetch_one(db.pool()).await.unwrap();
    assert_eq!(operations, 1);
}
