//! T05/A01/A02/A06: real owner issuance, scoped reads, money and replay boundaries.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod support;
use oxsum_core::{
    ActingService, BillingMode, BusinessOwnershipRequest, BusinessRole, BusinessStaffRequest,
    CreatedIntegrationCredential, Db, IntegrationCredentialRevokeRequest, IntegrationScope,
    MeteredEvent, NewUser, Price, Registration, SecretKey, SessionContext, SessionContextRequest,
    Tenants, UsageRecord, WalletError, totp,
};
use sqlx::Executor;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
const PASSWORD: &str = "scoped integration fixture password";
async fn fixture() -> Option<(Db, String, Registration, Registration)> {
    let url = oxsum_testkit::suite_db_url("integration_credentials")?;
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
async fn role_context_expiry_revocation_and_selected_customer_reads() {
    let Some((db, owner, a, b)) = fixture().await else {
        return;
    };
    let request = support::request(
        vec![a.organization.id],
        vec![IntegrationScope::CustomersRead],
    );
    assert!(
        db.create_integration_credential(&a.api_key.secret, &request)
            .await
            .is_err()
    );
    let session = db
        .login(&a.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session();
    assert!(
        db.create_integration_credential(&session.token, &request)
            .await
            .is_err()
    );
    for role in [
        BusinessRole::Admin,
        BusinessRole::Finance,
        BusinessRole::Support,
        BusinessRole::Viewer,
    ] {
        let staff = user(&db).await;
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                user_id: staff.user.id,
                role,
                reason: "test staff".into(),
            },
        )
        .await
        .unwrap();
        let session = db
            .login(&staff.user.email, PASSWORD)
            .await
            .unwrap()
            .into_session();
        db.switch_session_context(
            &session.token,
            &SessionContextRequest {
                idempotency_key: "staff-context".into(),
                context: SessionContext::Business,
                organization_id: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            db.create_integration_credential(&session.token, &request)
                .await,
            Err(WalletError::Forbidden(_))
        ));
    }
    let minted = db
        .create_integration_credential(&owner, &request)
        .await
        .unwrap();
    let secret = minted.secret.as_deref().unwrap();
    let actor = db.authenticate_service(secret).await.unwrap().unwrap();
    let list = db.integration_customers(&actor, None, 1).await.unwrap();
    assert_eq!(list.customers.len(), 1);
    assert_eq!(list.customers[0].organization_id, a.organization.id);
    assert!(list.next_cursor.is_none());
    assert!(matches!(
        db.integration_customer(&actor, b.organization.id).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(db.authenticate(secret).await.unwrap().is_none());
    assert!(db.authenticate_session(secret).await.unwrap().is_none());
    let bytes: Vec<u8> = sqlx::query_scalar(
        "SELECT secret_hash FROM oxsum.integration_credentials WHERE credential_id=$1",
    )
    .bind(actor.credential_id())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_ne!(bytes, secret.as_bytes());
    let audit: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT detail FROM oxsum.admin_audit WHERE action LIKE 'business.integration.%'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert!(!serde_json::to_string(&audit).unwrap().contains(secret));
    let replay = db
        .create_integration_credential(&owner, &request)
        .await
        .unwrap();
    assert!(replay.secret.is_none());
    assert_eq!(replay.credential.id, minted.credential.id);
    let mut changed = request;
    changed.customer_ids.push(b.organization.id);
    assert!(matches!(
        db.create_integration_credential(&owner, &changed).await,
        Err(WalletError::Conflict(_))
    ));
    db.revoke_integration_credential(
        &owner,
        minted.credential.id,
        &IntegrationCredentialRevokeRequest {
            idempotency_key: "revoke".into(),
            reason: "test revoke".into(),
        },
    )
    .await
    .unwrap();
    assert!(db.authenticate_service(secret).await.unwrap().is_none());
    assert!(
        db.integration_customer(&actor, a.organization.id)
            .await
            .is_err()
    );
    let (expired, captured) = service(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::CustomersRead],
    )
    .await;
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=now()-interval '2 days',expires_at=now()-interval '1 day' WHERE credential_id=$1").bind(captured.credential_id()).execute(db.pool()).await.unwrap();
    assert!(
        db.authenticate_service(expired.secret.as_deref().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(db.integration_customers(&captured, None, 10).await.is_err());
    let mut invalid = support::request(vec![], vec![IntegrationScope::CustomersRead]);
    assert!(
        db.create_integration_credential(&owner, &invalid)
            .await
            .is_err()
    );
    invalid.customer_ids = vec![a.organization.id, a.organization.id];
    assert!(
        db.create_integration_credential(&owner, &invalid)
            .await
            .is_err()
    );
    invalid.customer_ids = vec![a.organization.id];
    invalid.expires_at = OffsetDateTime::now_utc() - Duration::seconds(1);
    assert!(
        db.create_integration_credential(&owner, &invalid)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn credential_audit_failure_rolls_back_issuance_and_revocation() {
    let Some((db, owner, a, _)) = fixture().await else {
        return;
    };
    let (minted, actor) = service(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::MeteringWrite],
    )
    .await;
    db.pool().execute("CREATE FUNCTION oxsum.reject_integration_audit() RETURNS trigger AS $$ BEGIN IF NEW.action LIKE 'business.integration.%' THEN RAISE EXCEPTION 'injected integration audit failure'; END IF; RETURN NEW; END; $$ LANGUAGE plpgsql; CREATE TRIGGER fail_integration_audit BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_integration_audit()").await.unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.integration_credentials")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let req = support::request(
        vec![a.organization.id],
        vec![IntegrationScope::CustomersRead],
    );
    assert!(
        db.create_integration_credential(&owner, &req)
            .await
            .is_err()
    );
    assert_eq!(
        before,
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM oxsum.integration_credentials")
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert!(
        db.revoke_integration_credential(
            &owner,
            actor.credential_id(),
            &IntegrationCredentialRevokeRequest {
                idempotency_key: "failed-revoke".into(),
                reason: "test rollback".into()
            }
        )
        .await
        .is_err()
    );
    assert!(
        db.authenticate_service(minted.secret.as_deref().unwrap())
            .await
            .unwrap()
            .is_some()
    );
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.business_mutations WHERE action LIKE 'business.integration.%' AND idempotency_key=ANY($1)").bind(vec![req.idempotency_key,"failed-revoke".into()]).fetch_one(db.pool()).await.unwrap();
    assert_eq!(count, 0);
    db.pool()
        .execute("DROP TRIGGER fail_integration_audit ON oxsum.admin_audit")
        .await
        .unwrap();
}
fn usage(n: i64) -> UsageRecord {
    UsageRecord {
        input_tokens: n,
        event_type: Some("event".into()),
        ..UsageRecord::default()
    }
}
#[tokio::test]
async fn metering_customer_credential_finalization_and_label_isolation() {
    let Some((db, owner, a, b)) = fixture().await else {
        return;
    };
    let tenants = Tenants::new(db.pool().clone());
    let wa = tenants.get(&a.organization.tenant_id).await.unwrap();
    let wb = tenants.get(&b.organization.tenant_id).await.unwrap();
    let on = OffsetDateTime::now_utc().date();
    wa.top_up("fund", 1000, on).await.unwrap();
    wb.top_up("fund", 1000, on).await.unwrap();
    let channel = "integration-events";
    db.set_channel(
        channel,
        "http://127.0.0.1:9/unused",
        "secret",
        "openai",
        &SecretKey::from_bytes([6; 32]),
    )
    .await
    .unwrap();
    let mut price = Price {
        input_price_per_million: 1_000_000,
        mode: BillingMode::Event,
        output_price_per_million: 0,
        max_output_tokens: 0,
        cache_read_price_per_million: None,
        cache_write_5m_price_per_million: None,
        cache_write_1h_price_per_million: None,
        reasoning_price_per_million: None,
        cost_per_request: None,
        upstream: None,
        rules: vec![],
    };
    db.append_price(channel, "event", price.clone(), 100)
        .await
        .unwrap();
    let (_, reader) = service(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::CustomersRead],
    )
    .await;
    let (first, s1) = service(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::MeteringWrite],
    )
    .await;
    let (_, s2) = service(
        &db,
        vec![a.organization.id, b.organization.id],
        vec![IntegrationScope::MeteringWrite],
    )
    .await;
    let event = MeteredEvent {
        channel: channel.into(),
        billable_code: "event".into(),
        event_id: Some("caller-shared-label".into()),
        usage: usage(100),
    };
    assert!(
        db.metering_hold(&tenants, &reader, a.organization.id, "key", &event, on)
            .await
            .is_err()
    );
    assert!(
        db.metering_hold(&tenants, &s1, b.organization.id, "key", &event, on)
            .await
            .is_err()
    );
    assert_eq!(wa.settled().await.unwrap(), 1000);
    assert_eq!(wb.settled().await.unwrap(), 1000);
    assert_eq!(wa.reserved().await.unwrap(), 0);
    let hold = db
        .metering_hold(&tenants, &s1, a.organization.id, "key", &event, on)
        .await
        .unwrap();
    for customer in [a.organization.id, b.organization.id] {
        assert!(matches!(
            db.metering_release(&tenants, &s2, customer, &hold.hold_key, on)
                .await,
            Err(WalletError::NotFound(_))
        ));
        assert!(matches!(
            db.metering_settle(&tenants, &s2, customer, &hold.hold_key, None, &usage(1), on)
                .await,
            Err(WalletError::NotFound(_))
        ));
    }
    assert_eq!(wa.reserved().await.unwrap(), 100);
    let same_customer = db
        .metering_hold(&tenants, &s2, a.organization.id, "key", &event, on)
        .await
        .unwrap();
    assert_ne!(same_customer.hold_key, hold.hold_key);
    db.metering_release(
        &tenants,
        &s2,
        a.organization.id,
        &same_customer.hold_key,
        on,
    )
    .await
    .unwrap();
    let h2 = db
        .metering_hold(&tenants, &s2, b.organization.id, "key", &event, on)
        .await
        .unwrap();
    assert_ne!(hold.hold_key, h2.hold_key);
    price.input_price_per_million = 2_000_000;
    db.append_price(channel, "event", price, 100).await.unwrap();
    let replay = db
        .metering_hold(&tenants, &s1, a.organization.id, "key", &event, on)
        .await
        .unwrap();
    assert_eq!(replay.freeze_minor, 100);
    assert_eq!(replay.price_version, 1);
    let settled = db
        .metering_settle(
            &tenants,
            &s1,
            a.organization.id,
            &hold.hold_key,
            None,
            &usage(30),
            on,
        )
        .await
        .unwrap();
    assert_eq!(settled.charged_minor, 30);
    assert!(matches!(
        db.metering_settle(
            &tenants,
            &s1,
            a.organization.id,
            &hold.hold_key,
            None,
            &usage(31),
            on
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        db.metering_settle(
            &tenants,
            &s2,
            a.organization.id,
            &hold.hold_key,
            None,
            &usage(30),
            on
        )
        .await
        .is_err()
    );
    assert_eq!(
        db.metering_settle(
            &tenants,
            &s1,
            a.organization.id,
            &hold.hold_key,
            None,
            &usage(30),
            on
        )
        .await
        .unwrap()
        .settlement_id,
        settled.settlement_id
    );
    db.metering_settle(
        &tenants,
        &s2,
        b.organization.id,
        &h2.hold_key,
        None,
        &usage(40),
        on,
    )
    .await
    .unwrap();
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.usage_records WHERE request_id=ANY($1)")
            .bind(vec![hold.hold_key.clone(), h2.hold_key])
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rows, 2, "caller labels cannot collide across customers");
    db.metering_hold(&tenants, &s1, a.organization.id, "key", &event, on)
        .await
        .unwrap();
    assert!(db.open_hold(&hold.hold_key).await.unwrap().is_none());
    let changed = MeteredEvent {
        usage: usage(101),
        ..event.clone()
    };
    assert!(matches!(
        db.metering_hold(&tenants, &s1, a.organization.id, "key", &changed, on)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let once = db
        .metering_settle_once(&tenants, &s1, a.organization.id, "once", &event, on)
        .await
        .unwrap();
    assert!(
        db.metering_settle_once(&tenants, &s1, a.organization.id, "once", &changed, on)
            .await
            .is_err()
    );
    assert_eq!(
        db.metering_settle_once(&tenants, &s1, a.organization.id, "once", &event, on)
            .await
            .unwrap()
            .settlement_id,
        once.settlement_id
    );
    db.revoke_integration_credential(
        &owner,
        first.credential.id,
        &IntegrationCredentialRevokeRequest {
            idempotency_key: "rotate".into(),
            reason: "test".into(),
        },
    )
    .await
    .unwrap();
    let before = wa.settled().await.unwrap();
    assert!(
        db.metering_hold(&tenants, &s1, a.organization.id, "after-revoke", &event, on)
            .await
            .is_err()
    );
    assert!(
        db.metering_settle_once(&tenants, &s1, a.organization.id, "once", &event, on)
            .await
            .is_err()
    );
    assert_eq!(wa.settled().await.unwrap(), before);
}

#[tokio::test]
async fn concurrent_issuance_and_rotation_preserve_one_secret_and_current_authority() {
    let Some((db, owner, a, _)) = fixture().await else {
        return;
    };
    let request = support::request(
        vec![a.organization.id],
        vec![IntegrationScope::CustomersRead],
    );
    let (one, two) = tokio::join!(
        db.create_integration_credential(&owner, &request),
        db.create_integration_credential(&owner, &request)
    );
    let one = one.unwrap();
    let two = two.unwrap();
    assert_eq!(one.credential.id, two.credential.id);
    assert_eq!(
        usize::from(one.secret.is_some()) + usize::from(two.secret.is_some()),
        1
    );
    let secret = one.secret.or(two.secret).unwrap();
    let actor = db.authenticate_service(&secret).await.unwrap().unwrap();
    let revoke = IntegrationCredentialRevokeRequest {
        idempotency_key: "race-revoke".into(),
        reason: "test rotate".into(),
    };
    let (read, revoked) = tokio::join!(
        db.integration_customer(&actor, a.organization.id),
        db.revoke_integration_credential(&owner, actor.credential_id(), &revoke)
    );
    revoked.unwrap();
    assert!(read.is_ok() || matches!(read, Err(WalletError::InvalidCredentials)));
    assert!(
        db.integration_customer(&actor, a.organization.id)
            .await
            .is_err()
    );
    let unrelated = support::request(
        vec![a.organization.id],
        vec![IntegrationScope::MeteringWrite],
    );
    let rotation = IntegrationCredentialRevokeRequest {
        idempotency_key: "rotate-again".into(),
        reason: "test".into(),
    };
    let (created, rotated) = tokio::join!(
        db.create_integration_credential(&owner, &unrelated),
        db.revoke_integration_credential(&owner, actor.credential_id(), &rotation)
    );
    created.unwrap();
    rotated.unwrap();
    let target = user(&db).await;
    db.set_business_staff(
        &owner,
        &BusinessStaffRequest {
            idempotency_key: "target-staff".into(),
            user_id: target.user.id,
            role: BusinessRole::Admin,
            reason: "test transfer".into(),
        },
    )
    .await
    .unwrap();
    db.transfer_business_ownership(
        &owner,
        &BusinessOwnershipRequest {
            idempotency_key: "transfer".into(),
            user_id: target.user.id,
            reason: "test transfer".into(),
        },
    )
    .await
    .unwrap();
    // Replay is a record, never permission to act after owner transfer.
    assert!(matches!(
        db.create_integration_credential(&owner, &request).await,
        Err(WalletError::Forbidden(_))
    ));
}

#[tokio::test]
async fn confirmed_totp_and_logout_cannot_issue_credentials() {
    let Some((db, owner, a, _)) = fixture().await else {
        return;
    };
    let request = support::request(
        vec![a.organization.id],
        vec![IntegrationScope::MeteringWrite],
    );
    let identity = db.session_user(&owner).await.unwrap();
    let secret_key = SecretKey::from_bytes([4; 32]);
    let secret = totp::generate_secret();
    let sealed = secret_key.seal(&totp::base32_encode_pub(&secret)).unwrap();
    db.enroll_totp(identity.id, &sealed).await.unwrap();
    let code = totp::code(&secret, OffsetDateTime::now_utc().unix_timestamp());
    db.confirm_totp(identity.id, &code, &secret_key)
        .await
        .unwrap();
    assert!(matches!(
        db.create_integration_credential(&owner, &request).await,
        Err(WalletError::Forbidden(_))
    ));
    db.logout(&owner).await.unwrap();
    assert!(
        db.create_integration_credential(&owner, &request)
            .await
            .is_err()
    );
}
