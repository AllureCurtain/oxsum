//! A04: initial ownership requires deployment invitation and intended owner consent.
use oxsum_core::{
    BusinessSetupRequest, BusinessSetupTokenRequest, Db, NewUser, SetupAuthority, WalletError,
};
use uuid::Uuid;

async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("business_setup")?;
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
fn invitation(email: &str, key: &str) -> BusinessSetupTokenRequest {
    BusinessSetupTokenRequest {
        idempotency_key: key.into(),
        owner_email: email.into(),
    }
}
async fn user(db: &Db, email: &str) -> oxsum_core::Registration {
    db.register(NewUser {
        email: email.into(),
        password: "business setup fixture password".into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
async fn session(db: &Db, email: &str) -> String {
    let oxsum_core::LoginOutcome::Session(s) = db
        .login(email, "business setup fixture password")
        .await
        .unwrap()
    else {
        panic!("session");
    };
    s.token
}
#[tokio::test]
async fn setup_is_bound_expiring_single_use_and_durable_across_races() {
    let Some(db) = db().await else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let owner = user(&db, "owner@example.com").await;
    let other = user(&db, "other@example.com").await;
    let owner_session = session(&db, &owner.user.email).await;
    let other_session = session(&db, &other.user.email).await;
    let initial = db.business_ownership().await.unwrap();
    let created = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &invitation("OWNER@example.com", "init"),
        )
        .await
        .unwrap();
    let token = created.token.as_ref().unwrap();
    let replay = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &invitation("owner@example.com", "init"),
        )
        .await
        .unwrap();
    assert_eq!(created.id, replay.id);
    assert!(replay.token.is_none());
    assert!(matches!(
        db.mint_business_setup(
            SetupAuthority::Deployment,
            &invitation("other@example.com", "init")
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    let request = BusinessSetupRequest {
        token: token.clone(),
        business_name: "Example business".into(),
    };
    assert!(matches!(
        db.accept_business_setup(&other_session, &request).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.accept_business_setup(&owner.api_key.secret, &request)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM oxsum.business_setup_tokens WHERE setup_id=$1")
            .bind(created.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_ne!(stored, token.as_bytes());
    let audit: String =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(a))::text FROM oxsum.admin_audit a")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(!audit.contains(token));
    sqlx::query("UPDATE oxsum.business_setup_tokens SET expires_at=now()-interval '1 second' WHERE setup_id=$1").bind(created.id).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.accept_business_setup(&owner_session, &request).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(
        db.business_ownership()
            .await
            .unwrap()
            .business
            .initialized_at
            .is_none()
    );
    let created = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &invitation(&owner.user.email, "init-again"),
        )
        .await
        .unwrap();
    let request = BusinessSetupRequest {
        token: created.token.unwrap(),
        business_name: "Example business".into(),
    };
    let (a, b) = tokio::join!(
        db.accept_business_setup(&owner_session, &request),
        db.accept_business_setup(&owner_session, &request)
    );
    assert_ne!(a.is_ok(), b.is_ok(), "one setup wins");
    let business = db.business_ownership().await.unwrap();
    assert_eq!(business.business.id, initial.business.id);
    assert_eq!(business.business.registration_mode, "invite");
    assert!(business.business.initialized_at.is_some());
    assert_eq!(business.business.name, "Example business");
    let staff: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT user_id,role FROM oxsum.business_staff")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(staff, vec![(owner.user.id, "owner".into())]);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.admin_audit WHERE action='business.initialized'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert!(
        sqlx::query("DELETE FROM oxsum.admin_audit WHERE action='business.initialized'")
            .execute(db.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE oxsum.admin_audit SET actor='forged' WHERE action='business.setup.authorized'"
        )
        .execute(db.pool())
        .await
        .is_err()
    );
    let resumed = Db::from_pool(db.pool().clone());
    resumed.migrate().await.unwrap();
    assert!(
        resumed
            .business_ownership()
            .await
            .unwrap()
            .business
            .initialized_at
            .is_some()
    );
    assert!(matches!(
        resumed
            .mint_business_setup(
                SetupAuthority::Deployment,
                &invitation("other@example.com", "new")
            )
            .await,
        Err(WalletError::Conflict(_))
    ));
}
