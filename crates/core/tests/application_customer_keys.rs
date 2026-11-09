//! Application-only bounded keys, exact operation replay and current authority.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod support;

use oxsum_core::{
    ActingService, ApplicationCustomerArchiveRequest, ApplicationCustomerKeyRequest, Db,
    IntegrationApplicationRequest, IntegrationCredentialRevokeRequest, IntegrationScope,
    KeyConstraints, ProvisionCustomerRequest, WalletError,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("application_customer_keys")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
async fn grant(db: &Db, app: Uuid, scopes: Vec<IntegrationScope>) -> (ActingService, Uuid) {
    let mut request = support::request(vec![], scopes);
    request.application_id = Some(app);
    let created = db
        .create_integration_credential(&support::owner(db).await, &request)
        .await
        .unwrap();
    (
        db.authenticate_service(created.secret.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap(),
        created.credential.id,
    )
}
async fn setup(db: &Db) -> (ActingService, Uuid, Uuid, Uuid) {
    let app = db
        .create_integration_application(
            &support::owner(db).await,
            &IntegrationApplicationRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                key: Uuid::new_v4().simple().to_string(),
                name: "Key fixture".into(),
                customer_limit: 10,
                reason: "bounded key fixture".into(),
            },
        )
        .await
        .unwrap();
    let (service, credential) = grant(
        db,
        app.application_id,
        vec![
            IntegrationScope::CustomerKeysWrite,
            IntegrationScope::CustomersProvision,
            IntegrationScope::CustomersLifecycleWrite,
        ],
    )
    .await;
    let customer = db
        .provision_application_customer(
            &service,
            &ProvisionCustomerRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                external_id: "key-customer".into(),
                name: "Key customer".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap();
    (
        service,
        app.application_id,
        customer.customer.organization_id,
        credential,
    )
}
fn request() -> ApplicationCustomerKeyRequest {
    ApplicationCustomerKeyRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        name: "Runtime key".into(),
        reason: "customer inference".into(),
        expires_at: OffsetDateTime::now_utc() + Duration::hours(1),
        spend_limit_minor: 1_000_000,
        model_allowlist: vec!["allowed-b".into(), "allowed-a".into()],
        requests_per_minute: 30,
        max_concurrent_holds: 2,
    }
}
fn revoke() -> ApplicationCustomerArchiveRequest {
    ApplicationCustomerArchiveRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "replace customer secret".into(),
    }
}

#[tokio::test]
async fn concurrent_issuance_and_rotation_replay_one_secret_and_original_constraints() {
    let Some(db) = db().await else { return };
    let (service, app, customer, credential) = setup(&db).await;
    let request = request();
    let (a, b, c) = tokio::join!(
        db.create_application_customer_key(&service, customer, &request),
        db.create_application_customer_key(&service, customer, &request),
        db.create_application_customer_key(&service, customer, &request)
    );
    let results = [a.unwrap(), b.unwrap(), c.unwrap()];
    assert_eq!(results.iter().filter(|r| r.secret.is_some()).count(), 1);
    let original = &results[0].key;
    assert!(results.iter().all(|r| r.key.id == original.id));
    assert!(original.created_by.is_none());
    assert_eq!(original.spend_limit_minor, Some(1_000_000));
    assert_eq!(
        original.model_allowlist.as_ref().unwrap(),
        &vec!["allowed-a".to_owned(), "allowed-b".to_owned()]
    );
    let secret = results.iter().find_map(|r| r.secret.as_deref()).unwrap();
    assert_eq!(
        db.authenticate(secret).await.unwrap().unwrap().0.id,
        customer
    );
    let (rotated, _) = grant(&db, app, vec![IntegrationScope::CustomerKeysWrite]).await;
    db.revoke_integration_credential(
        &support::owner(&db).await,
        credential,
        &IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotate".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.create_application_customer_key(&service, customer, &request)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    let replay = db
        .create_application_customer_key(&rotated, customer, &request)
        .await
        .unwrap();
    assert!(replay.secret.is_none());
    assert_eq!(replay.key.id, original.id);
    assert!(
        db.authenticate(secret).await.unwrap().is_some(),
        "management revocation does not silently revoke the independently expiring customer key"
    );
    let page = db
        .application_customer_keys(&rotated, customer, None, 1)
        .await
        .unwrap();
    assert_eq!(page.keys.len(), 1);
    assert!(page.next_cursor.is_none());
    let receipt:serde_json::Value=sqlx::query_scalar("SELECT result FROM oxsum.integration_customer_operations WHERE namespace_id=$1 AND idempotency_key=$2").bind(app).bind(&request.idempotency_key).fetch_one(db.pool()).await.unwrap();
    assert!(!receipt.to_string().contains(secret));
    assert!(receipt.get("secret").is_none());
    let mut changed = request;
    changed.spend_limit_minor += 1;
    assert!(matches!(
        db.create_application_customer_key(&rotated, customer, &changed)
            .await,
        Err(WalletError::Conflict(_))
    ));
}

#[tokio::test]
async fn foreign_apps_portal_keys_and_missing_scopes_do_not_gain_authority() {
    let Some(db) = db().await else { return };
    let (service, app, customer, _) = setup(&db).await;
    let (foreign, _, foreign_customer, _) = setup(&db).await;
    let issued = db
        .create_application_customer_key(&service, customer, &request())
        .await
        .unwrap();
    assert!(matches!(
        db.application_customer_keys(&foreign, customer, None, 10)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.revoke_application_customer_key(&foreign, foreign_customer, issued.key.id, &revoke())
            .await,
        Err(WalletError::NotFound(_))
    ));
    let (read_only, _) = grant(&db, app, vec![IntegrationScope::CustomersRead]).await;
    assert!(matches!(
        db.create_application_customer_key(&read_only, customer, &request())
            .await,
        Err(WalletError::Forbidden(_))
    ));
    let portal = db
        .create_key(
            customer,
            Some("existing portal key".into()),
            None,
            None,
            KeyConstraints::default(),
        )
        .await
        .unwrap();
    assert!(matches!(
        db.revoke_application_customer_key(&service, customer, portal.key.id, &revoke())
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert_eq!(
        db.application_customer_keys(&service, customer, None, 100)
            .await
            .unwrap()
            .keys
            .len(),
        1
    );
    let legacy = support::request(vec![customer], vec![IntegrationScope::CustomerKeysWrite]);
    assert!(matches!(
        db.create_integration_credential(&support::owner(&db).await, &legacy)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn archived_customers_refuse_new_keys_but_can_revoke_exact_receipts() {
    let Some(db) = db().await else { return };
    let (service, _, customer, _) = setup(&db).await;
    let request = request();
    let issued = db
        .create_application_customer_key(&service, customer, &request)
        .await
        .unwrap();
    db.archive_application_customer(
        &service,
        customer,
        &ApplicationCustomerArchiveRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "archive".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.create_application_customer_key(&service, customer, &self::request())
            .await,
        Err(WalletError::Forbidden(_))
    ));
    let receipt = db
        .create_application_customer_key(&service, customer, &request)
        .await
        .unwrap();
    assert!(receipt.secret.is_none());
    assert_eq!(receipt.key.id, issued.key.id);
    let revoke = revoke();
    let closed = db
        .revoke_application_customer_key(&service, customer, issued.key.id, &revoke)
        .await
        .unwrap();
    assert!(closed.revoked_at.is_some());
    let repeated = db
        .revoke_application_customer_key(&service, customer, issued.key.id, &revoke)
        .await
        .unwrap();
    assert_eq!(closed.revoked_at, repeated.revoked_at);
    assert!(
        db.authenticate(issued.secret.as_deref().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let changed = ApplicationCustomerArchiveRequest {
        reason: "changed".into(),
        ..revoke
    };
    assert!(matches!(
        db.revoke_application_customer_key(&service, customer, issued.key.id, &changed)
            .await,
        Err(WalletError::Conflict(_))
    ));
}

#[tokio::test]
async fn issuance_limits_expiry_and_cross_operation_identity_fail_before_keys_exist() {
    let Some(db) = db().await else { return };
    let (service, _, customer, credential) = setup(&db).await;
    for case in 0..8 {
        let mut request = request();
        match case {
            0 => request.expires_at = OffsetDateTime::now_utc() - Duration::seconds(1),
            1 => request.expires_at = OffsetDateTime::now_utc() + Duration::days(2),
            2 => request.spend_limit_minor = 0,
            3 => request.model_allowlist.clear(),
            4 => request.model_allowlist = vec!["same".into(), "same".into()],
            5 => request.requests_per_minute = 10001,
            6 => request.max_concurrent_holds = 0,
            _ => request.name = " ".into(),
        };
        assert!(
            matches!(
                db.create_application_customer_key(&service, customer, &request)
                    .await,
                Err(WalletError::InvalidInput(_))
            ),
            "case {case}"
        );
    }
    assert!(
        db.application_customer_keys(&service, customer, None, 100)
            .await
            .unwrap()
            .keys
            .is_empty()
    );
    let customer_request = ProvisionCustomerRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        external_id: "second".into(),
        name: "second".into(),
        contact_email: None,
    };
    db.provision_application_customer(&service, &customer_request)
        .await
        .unwrap();
    let mut collision = request();
    collision.idempotency_key = customer_request.idempotency_key;
    assert!(matches!(
        db.create_application_customer_key(&service, customer, &collision)
            .await,
        Err(WalletError::Conflict(_))
    ));
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=clock_timestamp()-interval '2 days',expires_at=clock_timestamp()-interval '1 day' WHERE credential_id=$1").bind(credential).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.application_customer_keys(&service, customer, None, 10)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn manual_suspension_blocks_issuance_and_same_app_cannot_rebind_a_key_operation() {
    let Some(db) = db().await else { return };
    let (service, _, customer, _) = setup(&db).await;
    let second = db
        .provision_application_customer(
            &service,
            &ProvisionCustomerRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                external_id: "second".into(),
                name: "Second".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    let request = request();
    let key = db
        .create_application_customer_key(&service, customer, &request)
        .await
        .unwrap();
    assert!(matches!(
        db.create_application_customer_key(&service, second, &request)
            .await,
        Err(WalletError::Conflict(_))
    ));
    sqlx::query(
        "UPDATE oxsum.organizations SET suspended_at=clock_timestamp() WHERE organization_id=$1",
    )
    .bind(customer)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.create_application_customer_key(&service, customer, &self::request())
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(
        db.revoke_application_customer_key(&service, customer, key.key.id, &revoke())
            .await
            .unwrap()
            .revoked_at
            .is_some()
    );
}

#[tokio::test]
async fn issued_key_drives_the_existing_wallet_budget_and_model_admission() {
    let Some(db) = db().await else { return };
    let (service, _, customer, _) = setup(&db).await;
    let mut request = request();
    request.spend_limit_minor = 100;
    request.max_concurrent_holds = 1;
    let made = db
        .create_application_customer_key(&service, customer, &request)
        .await
        .unwrap();
    let (organization, acting) = db
        .authenticate(made.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    let access = db
        .serving_access(customer, Some(acting.key_id))
        .await
        .unwrap();
    assert!(access.allows_model("allowed-a"));
    assert!(!access.allows_model("other"));
    let wallet = oxsum_core::Tenants::new(db.pool().clone())
        .get(&organization.tenant_id)
        .await
        .unwrap();
    let day = OffsetDateTime::now_utc().date();
    wallet.top_up("fund-key-boundary", 1000, day).await.unwrap();
    wallet
        .hold_for_key(&acting, None, "first", "", 80, day, None)
        .await
        .unwrap();
    assert!(
        wallet
            .hold_for_key(&acting, None, "concurrent", "", 1, day, None)
            .await
            .is_err()
    );
    wallet.settle("first", "", 80, day).await.unwrap();
    assert!(
        wallet
            .hold_for_key(&acting, None, "over-budget", "", 21, day, None)
            .await
            .is_err()
    );
    wallet
        .hold_for_key(&acting, None, "remaining", "", 20, day, None)
        .await
        .unwrap();
    wallet.settle("remaining", "", 20, day).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 900);
    db.archive_application_customer(
        &service,
        customer,
        &ApplicationCustomerArchiveRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "archive after use".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        wallet
            .hold_for_key(&acting, None, "archived", "", 1, day, None)
            .await,
        Err(WalletError::Forbidden(_))
    ));
}
