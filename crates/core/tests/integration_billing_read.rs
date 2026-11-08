//! #788 N21.b: the billingRead scope resolves exactly one granted customer's
//! tenant for a balance/bill read, and every denial fails closed.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod support;
use oxsum_core::{
    ActingService, CreatedIntegrationCredential, Db, IntegrationScope, NewUser, Registration,
    Tenants, WalletError,
};
use time::OffsetDateTime;
use uuid::Uuid;
const PASSWORD: &str = "billing read fixture password";
async fn fixture() -> Option<(Db, String, Registration, Registration)> {
    let url = oxsum_testkit::suite_db_url("integration_billing_read")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = support::owner(&db).await;
    let a = user(&db).await;
    let b = user(&db).await;
    Some((db, owner, a, b))
}
async fn user(db: &Db) -> Registration {
    db.register(NewUser {
        email: format!("{}@example.com", Uuid::new_v4()),
        password: PASSWORD.into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
async fn service(
    db: &Db,
    customers: Vec<Uuid>,
    scopes: Vec<IntegrationScope>,
) -> (CreatedIntegrationCredential, ActingService) {
    let minted = support::mint(db, customers, scopes).await;
    let actor = db
        .authenticate_service(minted.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    (minted, actor)
}
#[tokio::test]
async fn billing_read_resolves_the_granted_customers_tenant() {
    let Some((db, _owner, a, _b)) = fixture().await else {
        return;
    };
    let tenants = Tenants::new(db.pool().clone());
    let wa = tenants.get(&a.organization.tenant_id).await.unwrap();
    wa.top_up("fund", 1_000, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    let (_cred, reader) = service(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::BillingRead],
    )
    .await;
    let tenant = db
        .integration_customer_tenant(&reader, a.organization.id)
        .await
        .unwrap();
    assert_eq!(tenant, a.organization.tenant_id);
    // The returned tenant opens the very wallet whose balance the route reports.
    let w = tenants.get(&tenant).await.unwrap();
    assert_eq!(w.settled().await.unwrap(), 1_000);
}
#[tokio::test]
async fn billing_read_denies_ungranted_and_foreign_customers() {
    let Some((db, _owner, a, b)) = fixture().await else {
        return;
    };
    // Granted only `a`; `b` is a real business customer outside the grant.
    let (_cred, reader) = service(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::BillingRead],
    )
    .await;
    let denied = db
        .integration_customer_tenant(&reader, b.organization.id)
        .await;
    assert!(matches!(denied, Err(WalletError::NotFound(_))));
    // A different credential that does grant `b` cannot read `a`.
    let (_other, other) = service(
        &db,
        vec![b.organization.id],
        vec![IntegrationScope::BillingRead],
    )
    .await;
    assert!(matches!(
        db.integration_customer_tenant(&other, a.organization.id)
            .await,
        Err(WalletError::NotFound(_))
    ));
    // An unknown customer id is not found either.
    assert!(matches!(
        db.integration_customer_tenant(&reader, Uuid::new_v4())
            .await,
        Err(WalletError::NotFound(_))
    ));
}
#[tokio::test]
async fn other_scopes_cannot_read_billing() {
    let Some((db, _owner, a, _b)) = fixture().await else {
        return;
    };
    // customersRead, meteringWrite and customersProvision credentials all
    // fail billingRead — the provision scope mints accounts but cannot read
    // their money.
    for scope in [
        IntegrationScope::CustomersRead,
        IntegrationScope::MeteringWrite,
        IntegrationScope::CustomersProvision,
    ] {
        let (_cred, svc) = service(&db, vec![a.organization.id], vec![scope]).await;
        assert!(matches!(
            db.integration_customer_tenant(&svc, a.organization.id)
                .await,
            Err(WalletError::Forbidden(_))
        ));
    }
}
#[tokio::test]
async fn revoked_and_expired_billing_read_fail_closed() {
    let Some((db, owner, a, _b)) = fixture().await else {
        return;
    };
    // Revoked credential: mint, revoke, then the read must fail.
    let (cred, reader) = service(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::BillingRead],
    )
    .await;
    db.revoke_integration_credential(
        &owner,
        cred.credential.id,
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "revoke for test".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.integration_customer_tenant(&reader, a.organization.id)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    // Expired credential: backdate created_at/expires_at, then the read fails.
    let (cred2, reader2) = service(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::BillingRead],
    )
    .await;
    sqlx::query(
        "UPDATE oxsum.integration_credentials SET created_at=clock_timestamp()-interval '2 days', expires_at=clock_timestamp()-interval '1 day' WHERE credential_id=$1",
    )
    .bind(cred2.credential.id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.integration_customer_tenant(&reader2, a.organization.id)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}
