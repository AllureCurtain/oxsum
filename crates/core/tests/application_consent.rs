//! Application consent boundaries, immutable replay and atomic first ownership.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod support;
use oxsum_core::{
    ActingService, ApplicationCustomerArchiveRequest, ApplicationLinkRequest, Db,
    IntegrationApplicationRequest, IntegrationCredentialRevokeRequest, IntegrationScope,
    LoginOutcome, NewUser, ProvisionCustomerRequest, SecretKey, WalletError,
};
use uuid::Uuid;

const PASSWORD: &str = "application consent fixture password";
async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("application_consent")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
async fn service(db: &Db, app: Uuid, scopes: Vec<IntegrationScope>) -> ActingService {
    let mut req = support::request(vec![], scopes);
    req.application_id = Some(app);
    let made = db
        .create_integration_credential(&support::owner(db).await, &req)
        .await
        .unwrap();
    db.authenticate_service(made.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap()
}
async fn fixture(db: &Db) -> (Uuid, ActingService, Uuid) {
    let app = db
        .create_integration_application(
            &support::owner(db).await,
            &IntegrationApplicationRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                key: Uuid::new_v4().simple().to_string(),
                name: "External application".into(),
                customer_limit: 10,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap();
    let actor = service(
        db,
        app.application_id,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::CustomerLinkWrite,
            IntegrationScope::BillingRead,
        ],
    )
    .await;
    let org = db
        .provision_application_customer(
            &actor,
            &ProvisionCustomerRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                external_id: Uuid::new_v4().to_string(),
                name: "Application account".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    (app.application_id, actor, org)
}
async fn person(db: &Db, verified: bool) -> (Uuid, String, String) {
    let made = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    if verified {
        db.mark_email_verified(made.user.id).await.unwrap();
    }
    let token = db
        .login(&made.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token;
    (made.user.id, made.user.email, token)
}
fn request(email: &str) -> ApplicationLinkRequest {
    ApplicationLinkRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        intended_email: email.into(),
        reason: "Recipient requested portal ownership".into(),
    }
}
fn revoke() -> ApplicationCustomerArchiveRequest {
    ApplicationCustomerArchiveRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "Replace lost link".into(),
    }
}
async fn members(db: &Db, org: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM oxsum.memberships WHERE organization_id=$1")
        .bind(org)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn verified_consent_preserves_accounts_context_money_and_terminal_authority() {
    let Some(db) = db().await else { return };
    let (_, actor, org) = fixture(&db).await;
    let (user, email, session) = person(&db, false).await;
    let before = serde_json::to_value(db.session_context(&session).await.unwrap()).unwrap();
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant)
        .await
        .unwrap();
    wallet
        .top_up(
            "before-consent",
            12345,
            time::OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let log_before = wallet.log_size().await.unwrap();
    let req = request(&format!(" {} ", email.to_uppercase()));
    let made = db.create_application_link(&actor, org, &req).await.unwrap();
    let secret = made.token.as_deref().unwrap();
    assert_eq!(secret.len(), 72);
    assert!(
        db.create_application_link(&actor, org, &req)
            .await
            .unwrap()
            .token
            .is_none()
    );
    assert!(matches!(
        db.preview_application_link(&session, secret).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.accept_application_link(&session, secret).await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(members(&db, org).await, 0);
    db.mark_email_verified(user).await.unwrap();
    let (_, _, wrong) = person(&db, true).await;
    assert!(matches!(
        db.preview_application_link(&wrong, secret).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.accept_application_link(&wrong, secret).await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(
        db.preview_application_link(&session, secret)
            .await
            .unwrap()
            .organization_id,
        org
    );
    assert_eq!(members(&db, org).await, 0);
    let (a, b) = tokio::join!(
        db.accept_application_link(&session, secret),
        db.accept_application_link(&session, secret)
    );
    assert_eq!(a.unwrap().accepted_at, b.unwrap().accepted_at);
    assert_eq!(members(&db, org).await, 1);
    let role: String = sqlx::query_scalar(
        "SELECT role FROM oxsum.memberships WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(org)
    .bind(user)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(role, "owner");
    let after = serde_json::to_value(db.session_context(&session).await.unwrap()).unwrap();
    assert_eq!(wallet.available().await.unwrap(), 12345);
    assert_eq!(wallet.log_size().await.unwrap(), log_before);
    let personal_org: Uuid = serde_json::from_value(before["organizationId"].clone()).unwrap();
    assert!(matches!(
        db.integration_customer_tenant(&actor, personal_org).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(db.integration_customer_tenant(&actor, org).await.is_ok());
    assert_eq!(before["context"], after["context"]);
    assert_eq!(before["organizationId"], after["organizationId"]);
    let cash: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.api_keys WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(cash, 0);
    let staff: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.business_staff WHERE user_id=$1")
            .bind(user)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(staff, 0);
    // Simulate membership removed later: immutable acceptance must not restore it.
    sqlx::query("DELETE FROM oxsum.memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(org)
        .bind(user)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.accept_application_link(&session, secret)
            .await
            .unwrap()
            .accepted_at
            .is_some()
    );
    assert_eq!(members(&db, org).await, 0);
    assert!(matches!(
        db.create_application_link(&actor, org, &request(&email))
            .await,
        Err(WalletError::Conflict(_))
    ));
    for table in [
        "application_link_invitations",
        "application_link_resolutions",
    ] {
        assert!(
            sqlx::query(&format!("DELETE FROM oxsum.{table} WHERE invitation_id=$1"))
                .bind(made.invitation_id)
                .execute(db.pool())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn scopes_rotation_exact_replay_and_concurrent_creation() {
    let Some(db) = db().await else { return };
    let (app, actor, org) = fixture(&db).await;
    let (_, foreign, _) = fixture(&db).await;
    let (_, email, session) = person(&db, true).await;
    let req = request(&email);
    assert!(matches!(
        db.create_application_link(&foreign, org, &req).await,
        Err(WalletError::NotFound(_))
    ));
    let narrow = service(&db, app, vec![IntegrationScope::CustomersRead]).await;
    assert!(matches!(
        db.create_application_link(&narrow, org, &req).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(
        db.create_integration_credential(
            &support::owner(&db).await,
            &support::request(vec![org], vec![IntegrationScope::CustomerLinkWrite])
        )
        .await
        .is_err()
    );
    let (a, b) = tokio::join!(
        db.create_application_link(&actor, org, &req),
        db.create_application_link(&actor, org, &req)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.invitation_id, b.invitation_id);
    assert_ne!(a.token.is_some(), b.token.is_some());
    let secret = a.token.or(b.token).unwrap();
    let mut changed = request(&email);
    changed.idempotency_key = req.idempotency_key.clone();
    changed.reason = "Changed terms".into();
    assert!(matches!(
        db.create_application_link(&actor, org, &changed).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.create_application_link(&actor, org, &request(&email))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let rotated = service(&db, app, vec![IntegrationScope::CustomerLinkWrite]).await;
    db.revoke_integration_credential(
        &support::owner(&db).await,
        actor.credential_id(),
        &IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotate".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.create_application_link(&actor, org, &req).await,
        Err(WalletError::InvalidCredentials)
    ));
    assert!(matches!(
        db.accept_application_link(&session, &secret).await,
        Err(WalletError::InvalidCredentials)
    ));
    assert!(
        db.create_application_link(&rotated, org, &req)
            .await
            .unwrap()
            .token
            .is_none()
    );
    let revoke = revoke();
    assert!(
        db.revoke_application_link(&foreign, org, a.invitation_id, &revoke)
            .await
            .is_err()
    );
    assert!(
        db.revoke_application_link(&rotated, org, a.invitation_id, &revoke)
            .await
            .unwrap()
            .revoked_at
            .is_some()
    );
    assert!(
        db.revoke_application_link(&rotated, org, a.invitation_id, &revoke)
            .await
            .unwrap()
            .token
            .is_none()
    );
    assert!(matches!(
        db.accept_application_link(&session, &secret).await,
        Err(WalletError::Conflict(_))
    ));
    let x = request(&email);
    let y = request("other@example.com");
    let (x, y) = tokio::join!(
        db.create_application_link(&rotated, org, &x),
        db.create_application_link(&rotated, org, &y)
    );
    assert_ne!(x.is_ok(), y.is_ok());
}

#[tokio::test]
async fn current_second_factor_existing_membership_and_creator_expiry_fail_closed() {
    let Some(db) = db().await else { return };
    let (_, actor, org) = fixture(&db).await;
    let (user, email, session) = person(&db, true).await;
    let made = db
        .create_application_link(&actor, org, &request(&email))
        .await
        .unwrap();
    let secret = made.token.as_deref().unwrap();
    let key = SecretKey::from_bytes([87; 32]);
    let seed = oxsum_core::totp::generate_secret();
    let sealed = key
        .seal(&oxsum_core::totp::base32_encode_pub(&seed))
        .unwrap();
    db.enroll_totp(user, &sealed).await.unwrap();
    let code = oxsum_core::totp::code(&seed, time::OffsetDateTime::now_utc().unix_timestamp());
    let backup = db.confirm_totp(user, &code, &key).await.unwrap();
    assert!(matches!(
        db.accept_application_link(&session, secret).await,
        Err(WalletError::Forbidden(_))
    ));
    let LoginOutcome::TotpRequired { challenge_token } = db.login(&email, PASSWORD).await.unwrap()
    else {
        panic!("second factor required")
    };
    let session = db
        .verify_totp(&challenge_token, &backup[0], &key)
        .await
        .unwrap()
        .token;
    sqlx::query(
        "INSERT INTO oxsum.memberships(organization_id,user_id,role) VALUES($1,$2,'owner')",
    )
    .bind(org)
    .bind(user)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.accept_application_link(&session, secret).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.create_application_link(&actor, org, &request(&email))
            .await,
        Err(WalletError::Conflict(_))
    ));
    sqlx::query("DELETE FROM oxsum.memberships WHERE organization_id=$1")
        .bind(org)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=clock_timestamp()-interval '2 days',expires_at=clock_timestamp()-interval '1 day' WHERE credential_id=$1").bind(actor.credential_id()).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.accept_application_link(&session, secret).await,
        Err(WalletError::InvalidCredentials)
    ));
    assert!(matches!(
        db.revoke_application_link(&actor, org, made.invitation_id, &revoke())
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn audit_failure_rolls_back_first_owner_and_revoke_races_accept() {
    let Some(db) = db().await else { return };
    let (_, actor, org) = fixture(&db).await;
    let (user, email, session) = person(&db, true).await;
    let made = db
        .create_application_link(&actor, org, &request(&email))
        .await
        .unwrap();
    let secret = made.token.as_deref().unwrap();
    let trigger = format!("fail_link_{}", org.simple());
    sqlx::raw_sql(&format!("CREATE FUNCTION oxsum.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.target='{}' AND NEW.action='customer.application.link.accept' THEN RAISE EXCEPTION 'fixture audit fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.{trigger}();",org)).execute(db.pool()).await.unwrap();
    assert!(db.accept_application_link(&session, secret).await.is_err());
    assert_eq!(members(&db, org).await, 0);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.application_link_resolutions WHERE invitation_id=$1",
    )
    .bind(made.invitation_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON oxsum.admin_audit; DROP FUNCTION oxsum.{trigger}();"
    ))
    .execute(db.pool())
    .await
    .unwrap();
    let revoke = revoke();
    let (accepted, revoked) = tokio::join!(
        db.accept_application_link(&session, secret),
        db.revoke_application_link(&actor, org, made.invitation_id, &revoke)
    );
    assert_ne!(accepted.is_ok(), revoked.is_ok());
    assert_eq!(members(&db, org).await, i64::from(accepted.is_ok()));
    let audit:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.admin_audit WHERE target=$1 AND actor=$2 AND action='customer.application.link.accept'").bind(org.to_string()).bind(format!("user:{user}")).fetch_one(db.pool()).await.unwrap();
    assert_eq!(audit, i64::from(accepted.is_ok()));
}

#[tokio::test]
async fn immutable_expired_link_and_malformed_secret_cannot_claim() {
    let Some(db) = db().await else { return };
    let (app, actor, org) = fixture(&db).await;
    let (_, email, session) = person(&db, true).await;
    use sha2::{Digest, Sha256};
    let secret = format!(
        "oxs-lnk-{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    // Insert an already expired immutable fixture; production invitations cannot be edited.
    sqlx::query("INSERT INTO oxsum.application_link_invitations(invitation_id,business_id,application_id,organization_id,credential_id,token_hash,intended_email,created_at,expires_at) SELECT $1,business_id,$2,$3,$4,$5,$6,clock_timestamp()-interval '8 days',clock_timestamp()-interval '1 day' FROM oxsum.integration_applications WHERE application_id=$2")
        .bind(Uuid::new_v4()).bind(app).bind(org).bind(actor.credential_id()).bind(Sha256::digest(secret.as_bytes()).to_vec()).bind(&email).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.accept_application_link(&session, &secret).await,
        Err(WalletError::Conflict(_))
    ));
    for token in ["", "oxs-lnk-", &format!("oxs-lnk-{}", "💰".repeat(16))] {
        assert!(matches!(
            db.accept_application_link(&session, token).await,
            Err(WalletError::NotFound(_))
        ));
    }
    let new = db
        .create_application_link(&actor, org, &request(&email))
        .await
        .unwrap();
    assert!(new.token.is_some());
}
