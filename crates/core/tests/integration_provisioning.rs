//! `customersProvision` grants namespace-bounded service-managed customer
//! creation; every other authority is refused.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod support;
use oxsum_core::{
    ActingService, CreatedIntegrationCredential, Db, IntegrationScope, NewUser,
    ProvisionCustomerRequest, Registration, WalletError,
};
use uuid::Uuid;
const PASSWORD: &str = "provisioning fixture password";
async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("integration_provisioning")?;
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
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
fn provision(external: &str) -> ProvisionCustomerRequest {
    ProvisionCustomerRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        external_id: external.into(),
        name: "Acme tenant".into(),
        contact_email: Some("ops@acme.test".into()),
    }
}

#[tokio::test]
async fn provision_creates_a_scoped_service_managed_customer() {
    let Some(db) = db().await else { return };
    let existing = user(&db).await;
    let (_minted, actor) = service(
        &db,
        vec![existing.organization.id],
        vec![IntegrationScope::CustomersProvision],
    )
    .await;
    let created = db
        .provision_integration_customer(&actor, &provision("acme-001"))
        .await
        .unwrap();
    // The new account is a real customer billing account under the business,
    // bound to the caller's externalId — not the pre-existing granted one.
    assert_eq!(created.external_id, "acme-001");
    assert_ne!(created.organization_id, existing.organization.id);
    // Namespace grant: the credential can read what it created without the
    // owner adding it to the allowlist by hand — even though it holds no
    // customersRead scope, the grant row is the read's allowlist source.
    let read = db
        .integration_customer(&actor, created.organization_id)
        .await;
    assert!(
        matches!(read, Err(WalletError::Forbidden(_))),
        "no customersRead scope → still refused: {read:?}"
    );
    // A second credential holding only customersRead over the pre-existing
    // customer cannot see the provisioned account — the provision grant is
    // scoped to the credential that created it, not every credential.
    let (_m2, reader) = service(
        &db,
        vec![existing.organization.id],
        vec![IntegrationScope::CustomersRead],
    )
    .await;
    assert!(matches!(
        db.integration_customer(&reader, created.organization_id)
            .await,
        Err(WalletError::NotFound(_))
    ));
}

#[tokio::test]
async fn provision_is_idempotent_and_rejects_foreign_external_id() {
    let Some(db) = db().await else { return };
    let (_m, actor) = service(&db, vec![], vec![IntegrationScope::CustomersProvision]).await;
    let req = provision("dup-001");
    let first = db
        .provision_integration_customer(&actor, &req)
        .await
        .unwrap();
    // Exact replay returns the standing account — same organization.
    let replay = db
        .provision_integration_customer(&actor, &req)
        .await
        .unwrap();
    assert_eq!(replay.organization_id, first.organization_id);
    // A different credential cannot claim the same externalId, and the error
    // does not leak that the id is taken by another namespace.
    let (_m2, other) = service(&db, vec![], vec![IntegrationScope::CustomersProvision]).await;
    let foreign = db.provision_integration_customer(&other, &req).await;
    assert!(
        matches!(
            foreign,
            Err(WalletError::Conflict(_)) | Err(WalletError::NotFound(_))
        ),
        "foreign externalId must not be claimable: {foreign:?}"
    );
}

#[tokio::test]
async fn provision_refuses_wrong_scope_and_lapsed_credentials() {
    let Some(db) = db().await else { return };
    let existing = user(&db).await;
    // customersRead cannot provision.
    let (_r, reader) = service(
        &db,
        vec![existing.organization.id],
        vec![IntegrationScope::CustomersRead],
    )
    .await;
    assert!(matches!(
        db.provision_integration_customer(&reader, &provision("x"))
            .await,
        Err(WalletError::Forbidden(_))
    ));
    // meteringWrite cannot provision.
    let (_m, meter) = service(
        &db,
        vec![existing.organization.id],
        vec![IntegrationScope::MeteringWrite],
    )
    .await;
    assert!(matches!(
        db.provision_integration_customer(&meter, &provision("y"))
            .await,
        Err(WalletError::Forbidden(_))
    ));
    // A revoked credential fails closed.
    let (minted, revoked) = service(&db, vec![], vec![IntegrationScope::CustomersProvision]).await;
    let owner = support::owner(&db).await;
    db.revoke_integration_credential(
        &owner,
        minted.credential.id,
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: "rev".into(),
            reason: "test".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.provision_integration_customer(&revoked, &provision("z"))
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    // An expired credential fails closed.
    let (_e, expired) = service(&db, vec![], vec![IntegrationScope::CustomersProvision]).await;
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=now()-interval '2 days', expires_at=now()-interval '1 day' WHERE credential_id=$1")
        .bind(expired.credential_id())
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.provision_integration_customer(&expired, &provision("w"))
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn provision_grants_no_customer_authority_beyond_its_namespace() {
    let Some(db) = db().await else { return };
    let existing = user(&db).await;
    let (_m, actor) = service(&db, vec![], vec![IntegrationScope::CustomersProvision]).await;
    let created = db
        .provision_integration_customer(&actor, &provision("scoped-001"))
        .await
        .unwrap();
    // The credential cannot meter or read a customer outside its grant —
    // `existing` was never provisioned by nor allowlisted to this credential.
    assert!(matches!(
        db.integration_customer(&actor, existing.organization.id)
            .await,
        Err(WalletError::Forbidden(_)) | Err(WalletError::NotFound(_))
    ));
    // The provisioned account carries no wallet balance or staff seat.
    let members: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.memberships WHERE organization_id=$1")
            .bind(created.organization_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(members, 0, "a service-managed account has no human member");
}
