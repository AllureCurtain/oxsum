#![allow(dead_code)]
use oxsum_core::{
    BusinessSetupRequest, BusinessSetupTokenRequest, CreatedIntegrationCredential, Db,
    IntegrationCredentialRequest, IntegrationScope, NewUser, SessionContext, SessionContextRequest,
    SetupAuthority,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Production setup/login/issuance; once per isolated test process.
pub async fn owner(db: &Db) -> String {
    static OWNER: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    OWNER
        .get_or_init(|| async {
            let password = "integration owner fixture password";
            let user = db
                .register(NewUser {
                    email: format!("{}@example.com", Uuid::new_v4()),
                    password: password.into(),
                    organization_name: None,
                })
                .await
                .unwrap();
            let session = db
                .login(&user.user.email, password)
                .await
                .unwrap()
                .into_session();
            let setup = db
                .mint_business_setup(
                    SetupAuthority::Deployment,
                    &BusinessSetupTokenRequest {
                        idempotency_key: "integration-setup".into(),
                        owner_email: user.user.email,
                    },
                )
                .await
                .unwrap();
            db.accept_business_setup(
                &session.token,
                &BusinessSetupRequest {
                    token: setup.token.unwrap(),
                    business_name: "Integration fixture".into(),
                },
            )
            .await
            .unwrap();
            db.switch_session_context(
                &session.token,
                &SessionContextRequest {
                    idempotency_key: "business-context".into(),
                    context: SessionContext::Business,
                    organization_id: None,
                },
            )
            .await
            .unwrap();
            session.token
        })
        .await
        .clone()
}
pub fn request(
    customer_ids: Vec<Uuid>,
    scopes: Vec<IntegrationScope>,
) -> IntegrationCredentialRequest {
    IntegrationCredentialRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        name: Some("metering fixture".into()),
        scopes,
        customer_ids,
        expires_at: OffsetDateTime::now_utc() + Duration::days(1),
        reason: "test integration issuance".into(),
    }
}
pub async fn mint(
    db: &Db,
    customers: Vec<Uuid>,
    scopes: Vec<IntegrationScope>,
) -> CreatedIntegrationCredential {
    db.create_integration_credential(&owner(db).await, &request(customers, scopes))
        .await
        .unwrap()
}
