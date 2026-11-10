//! A04: initial ownership requires deployment invitation and intended owner consent.
use oxsum_core::{
    BusinessSetupRegistrationRequest, BusinessSetupTokenRequest, Db, SetupAuthority, WalletError,
};

async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("business_setup_fresh")?;
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
#[tokio::test]
async fn fresh_install_registers_only_the_invited_owner_atomically() {
    let Some(db) = db().await else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let created = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &invitation("new@example.com", "fresh"),
        )
        .await
        .unwrap();
    let mut request = BusinessSetupRegistrationRequest {
        token: created.token.unwrap(),
        business_name: "Fresh business".into(),
        email: "wrong@example.com".into(),
        password: "business setup fixture password".into(),
    };
    assert!(matches!(
        db.register_business_owner(&request).await,
        Err(WalletError::NotFound(_))
    ));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(users, 0);
    request.email = "new@example.com".into();
    // Fail at the final audit insert, after registration and membership writes.
    // The transaction must roll all of them back and leave consent reusable.
    sqlx::raw_sql("CREATE FUNCTION oxsum.fail_setup_test() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='business.initialized' THEN RAISE EXCEPTION 'test interruption'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_setup_test BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.fail_setup_test();").execute(db.pool()).await.unwrap();
    assert!(db.register_business_owner(&request).await.is_err());
    let partial: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM oxsum.users)+(SELECT count(*) FROM oxsum.business_staff)+(SELECT count(*) FROM oxsum.business_setup_tokens WHERE accepted_at IS NOT NULL)").fetch_one(db.pool()).await.unwrap();
    assert_eq!(partial, 0);
    assert!(
        db.business_ownership()
            .await
            .unwrap()
            .business
            .initialized_at
            .is_none()
    );
    sqlx::raw_sql(
        "DROP TRIGGER fail_setup_test ON oxsum.admin_audit; DROP FUNCTION oxsum.fail_setup_test();",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let result = db.register_business_owner(&request).await.unwrap();
    let registration = result.registration.unwrap();
    assert_eq!(registration.user.email, "new@example.com");
    assert!(
        db.authenticate(&registration.api_key.secret)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        db.login(&registration.user.email, &request.password)
            .await
            .is_ok()
    );
    let profile = db
        .customer_profile(result.ownership.business.id, registration.organization.id)
        .await
        .unwrap();
    assert_eq!(profile.tenant_id, registration.organization.tenant_id);
    assert!(matches!(
        db.register_business_owner(&request).await,
        Err(WalletError::Conflict(_))
    ));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(users, 1);
    assert_eq!(
        db.business_migration_preview()
            .await
            .unwrap()
            .ledger_schemas,
        0
    );
}
