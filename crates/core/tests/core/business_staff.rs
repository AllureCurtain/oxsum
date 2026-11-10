//! A01/A04: real setup and session flows; SQL only for fault/expiry injection.
use oxsum_core::{
    BusinessCapability, BusinessOwnershipRequest, BusinessRegistrationRequest, BusinessRole,
    BusinessSetupRequest, BusinessSetupTokenRequest, BusinessStaffRequest, Db, LoginOutcome,
    NewUser, OAuthIdentity, Registration, RegistrationMode, SecretKey, SessionContext,
    SessionContextRequest, SetupAuthority, WalletError,
};
use uuid::Uuid;
const PASSWORD: &str = "business staff fixture password";
async fn user(db: &Db, name: &str) -> Registration {
    db.register(NewUser {
        email: format!("{name}@example.com"),
        password: PASSWORD.into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
async fn login(db: &Db, email: &str) -> String {
    let LoginOutcome::Session(session) = db.login(email, PASSWORD).await.unwrap() else {
        panic!("session")
    };
    session.token
}
fn context(key: &str, context: SessionContext) -> SessionContextRequest {
    SessionContextRequest {
        idempotency_key: key.into(),
        context,
        organization_id: None,
    }
}
fn staff_req(user: Uuid, role: BusinessRole, key: &str) -> BusinessStaffRequest {
    BusinessStaffRequest {
        user_id: user,
        role,
        idempotency_key: key.into(),
        reason: "test appointment".into(),
    }
}
fn target(user: Uuid, key: &str) -> BusinessOwnershipRequest {
    BusinessOwnershipRequest {
        user_id: user,
        idempotency_key: key.into(),
        reason: "test lifecycle change".into(),
    }
}
fn signup(name: &str) -> NewUser {
    NewUser {
        email: format!("{name}@example.com"),
        password: PASSWORD.into(),
        organization_name: None,
    }
}
fn policy(key: &str, mode: RegistrationMode) -> BusinessRegistrationRequest {
    BusinessRegistrationRequest {
        idempotency_key: key.into(),
        mode,
        reason: "test registration policy".into(),
    }
}

#[test]
fn explicit_role_matrix_keeps_secrets_money_and_staff_separate() {
    use BusinessCapability::*;
    let matrix = [
        (BusinessRole::Owner, BusinessCapability::ALL.to_vec()),
        (
            BusinessRole::Admin,
            vec![
                OverviewRead,
                CustomersRead,
                CustomersWrite,
                CustomersSuspend,
                RequestsRead,
                CatalogWrite,
                RoutingWrite,
                ReportsRead,
            ],
        ),
        (
            BusinessRole::Finance,
            vec![
                OverviewRead,
                CustomersRead,
                RequestsRead,
                CommercialWrite,
                FundsWrite,
                RefundsWrite,
                ReportsRead,
            ],
        ),
        (
            BusinessRole::Support,
            vec![OverviewRead, CustomersRead, CustomersSuspend, RequestsRead],
        ),
        (BusinessRole::Viewer, vec![OverviewRead, ReportsRead]),
    ];
    for (role, allowed) in matrix {
        for capability in BusinessCapability::ALL {
            assert_eq!(
                role.allows(*capability),
                allowed.contains(capability),
                "{role:?} {capability:?}"
            );
        }
    }
}

#[tokio::test]
async fn business_authority_policy_revocation_and_ownership_are_atomic() {
    let Some(url) = oxsum_testkit::suite_db_url("business_staff") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    assert!(matches!(
        db.register_customer(signup("closed")).await,
        Err(WalletError::Forbidden(_))
    ));
    let oauth = OAuthIdentity {
        provider: "github".into(),
        provider_user_id: "new-provider-id".into(),
        email: "oauth@example.com".into(),
    };
    assert!(matches!(
        db.oauth_login(&oauth, true).await,
        Err(WalletError::Forbidden(_))
    ));
    let owner = user(&db, "owner").await;
    let owner_token = login(&db, &owner.user.email).await;
    let outsider = user(&db, "outsider").await;
    let outsider_token = login(&db, &outsider.user.email).await;
    let setup = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &BusinessSetupTokenRequest {
                idempotency_key: "setup".into(),
                owner_email: owner.user.email.clone(),
            },
        )
        .await
        .unwrap();
    db.accept_business_setup(
        &owner_token,
        &BusinessSetupRequest {
            token: setup.token.unwrap(),
            business_name: "Business".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.business_capability(&owner_token, BusinessCapability::StaffManage)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.switch_session_context(
            &outsider_token,
            &context("outsider", SessionContext::Business)
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.switch_session_context(
            &owner.api_key.secret,
            &context("key", SessionContext::Business)
        )
        .await,
        Err(WalletError::InvalidCredentials)
    ));
    let selected = db
        .switch_session_context(&owner_token, &context("business", SessionContext::Business))
        .await
        .unwrap();
    assert_eq!(selected.business.unwrap().role, BusinessRole::Owner);
    assert!(
        db.authenticate_session(&owner_token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.authenticate(&owner.api_key.secret)
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        db.delete_account(owner.user.id, PASSWORD).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.revoke_business_staff(&owner_token, &target(owner.user.id, "revoke-owner"))
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.set_business_staff(
            &owner_token,
            &staff_req(owner.user.id, BusinessRole::Viewer, "demote-owner")
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.set_business_staff(
            &owner_token,
            &staff_req(outsider.user.id, BusinessRole::Owner, "promote-owner")
        )
        .await,
        Err(WalletError::InvalidInput(_))
    ));
    let mut people = Vec::new();
    for (name, role) in [
        ("admin", BusinessRole::Admin),
        ("finance", BusinessRole::Finance),
        ("support", BusinessRole::Support),
        ("viewer", BusinessRole::Viewer),
    ] {
        let person = user(&db, name).await;
        let token = login(&db, &person.user.email).await;
        let req = staff_req(person.user.id, role, name);
        let first = db.set_business_staff(&owner_token, &req).await.unwrap();
        assert_eq!(
            first.id,
            db.set_business_staff(&owner_token, &req).await.unwrap().id
        );
        db.switch_session_context(&token, &context(name, SessionContext::Business))
            .await
            .unwrap();
        for capability in BusinessCapability::ALL {
            assert_eq!(
                db.business_capability(&token, *capability).await.is_ok(),
                role.allows(*capability),
                "{role:?} {capability:?}"
            );
        }
        assert!(matches!(
            db.set_business_registration(&token, &policy(name, RegistrationMode::Open))
                .await,
            Err(WalletError::Forbidden(_))
        ));
        people.push((person, token));
    }
    assert_eq!(db.business_staff(&owner_token).await.unwrap().len(), 5);
    let changed = staff_req(people[0].0.user.id, BusinessRole::Viewer, "admin");
    assert!(matches!(
        db.set_business_staff(&owner_token, &changed).await,
        Err(WalletError::Conflict(_))
    ));
    // A confirmed second factor also gates OAuth-created or older sessions;
    // only the existing TOTP login completion stamps a verified session.
    let secret_key = SecretKey::from_bytes([23; 32]);
    let totp_secret = oxsum_core::totp::generate_secret();
    let sealed = secret_key
        .seal(&oxsum_core::totp::base32_encode_pub(&totp_secret))
        .unwrap();
    db.enroll_totp(people[3].0.user.id, &sealed).await.unwrap();
    let code = oxsum_core::totp::code(
        &totp_secret,
        time::OffsetDateTime::now_utc().unix_timestamp(),
    );
    let backup = db
        .confirm_totp(people[3].0.user.id, &code, &secret_key)
        .await
        .unwrap();
    assert!(matches!(
        db.business_capability(&people[3].1, BusinessCapability::OverviewRead)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.enroll_totp(people[3].0.user.id, &sealed).await,
        Err(WalletError::Conflict(_))
    ));
    let LoginOutcome::TotpRequired { challenge_token } =
        db.login(&people[3].0.user.email, PASSWORD).await.unwrap()
    else {
        panic!("second factor required")
    };
    let verified = db
        .verify_totp(&challenge_token, &backup[0], &secret_key)
        .await
        .unwrap();
    db.switch_session_context(
        &verified.token,
        &context("verified-business", SessionContext::Business),
    )
    .await
    .unwrap();
    assert!(
        db.business_capability(&verified.token, BusinessCapability::OverviewRead)
            .await
            .is_ok()
    );
    // Policy is closed after setup, then opens only through an owner mutation.
    assert!(matches!(
        db.register_customer(signup("after-setup-closed")).await,
        Err(WalletError::Forbidden(_))
    ));
    db.set_business_registration(&owner_token, &policy("open", RegistrationMode::Open))
        .await
        .unwrap();
    let customer = db.register_customer(signup("open-customer")).await.unwrap();
    let new_oauth = db.oauth_login(&oauth, true).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.business_staff WHERE user_id IN ($1,$2)")
            .bind(customer.user.id)
            .bind(new_oauth.principal.as_ref().unwrap().user.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
    db.set_business_registration(&owner_token, &policy("close", RegistrationMode::Invite))
        .await
        .unwrap();
    assert!(
        db.oauth_login(&oauth, true).await.is_ok(),
        "existing identity can log in under closed registration"
    );
    assert!(matches!(
        db.register_customer(signup("closed-again")).await,
        Err(WalletError::Forbidden(_))
    ));
    // A late audit failure rolls back the staff write and the idempotency receipt.
    sqlx::raw_sql("CREATE FUNCTION oxsum.fail_staff_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='business.staff.set' THEN RAISE EXCEPTION 'injected audit fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_staff_audit BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.fail_staff_audit();").execute(db.pool()).await.unwrap();
    let fault = staff_req(outsider.user.id, BusinessRole::Support, "fault");
    assert!(db.set_business_staff(&owner_token, &fault).await.is_err());
    assert_eq!(db.business_staff(&owner_token).await.unwrap().len(), 5);
    sqlx::query("DROP TRIGGER fail_staff_audit ON oxsum.admin_audit")
        .execute(db.pool())
        .await
        .unwrap();
    db.set_business_staff(&owner_token, &fault).await.unwrap();
    // Revoking staff does not revoke their independent customer membership.
    db.revoke_business_staff(&owner_token, &target(people[2].0.user.id, "revoke-support"))
        .await
        .unwrap();
    assert!(matches!(
        db.business_capability(&people[2].1, BusinessCapability::OverviewRead)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    db.switch_session_context(&people[2].1, &context("back", SessionContext::Customer))
        .await
        .unwrap();
    assert!(
        db.authenticate_session(&people[2].1)
            .await
            .unwrap()
            .is_some()
    );
    // A session cannot choose another customer's identity even after staff revocation.
    let foreign = SessionContextRequest {
        idempotency_key: "foreign".into(),
        context: SessionContext::Customer,
        organization_id: Some(owner.organization.id),
    };
    assert!(matches!(
        db.switch_session_context(&people[2].1, &foreign).await,
        Err(WalletError::NotFound(_))
    ));
    let bad_selection = SessionContextRequest {
        idempotency_key: "bad-selection".into(),
        context: SessionContext::Business,
        organization_id: Some(owner.organization.id),
    };
    assert!(matches!(
        db.switch_session_context(&owner_token, &bad_selection)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    // Expiry is checked on business use, independently of membership or cookie shape.
    let expired = login(&db, &people[0].0.user.email).await;
    db.switch_session_context(&expired, &context("expire", SessionContext::Business))
        .await
        .unwrap();
    use sha2::{Digest, Sha256};
    sqlx::query(
        "UPDATE oxsum.sessions SET expires_at=now()-interval '1 second' WHERE token_hash=$1",
    )
    .bind(Sha256::digest(expired.as_bytes()).to_vec())
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.business_capability(&expired, BusinessCapability::OverviewRead)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    // Reset and logout affect business sessions exactly like customer sessions.
    db.reset_password(people[1].0.user.id, PASSWORD)
        .await
        .unwrap();
    assert!(matches!(
        db.business_capability(&people[1].1, BusinessCapability::OverviewRead)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    db.logout(&people[3].1).await.unwrap();
    assert!(matches!(
        db.business_capability(&people[3].1, BusinessCapability::OverviewRead)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    // Transfer racing account deletion cannot appoint a tombstoned owner or
    // remove the owner who just received the business. Both use business-first locks.
    let candidate = user(&db, "deletion-race").await;
    db.set_business_staff(
        &owner_token,
        &staff_req(candidate.user.id, BusinessRole::Admin, "race-candidate"),
    )
    .await
    .unwrap();
    let transfer = target(candidate.user.id, "transfer-deletion-race");
    let (transferred, deleted) = tokio::join!(
        db.transfer_business_ownership(&owner_token, &transfer),
        db.delete_account(candidate.user.id, PASSWORD)
    );
    assert_eq!(
        usize::from(transferred.is_ok()) + usize::from(deleted.is_ok()),
        1
    );
    if transferred.is_ok() {
        let candidate_token = login(&db, &candidate.user.email).await;
        db.switch_session_context(
            &candidate_token,
            &context("business", SessionContext::Business),
        )
        .await
        .unwrap();
        db.transfer_business_ownership(
            &candidate_token,
            &target(owner.user.id, "return-ownership"),
        )
        .await
        .unwrap();
    }
    let LoginOutcome::TotpRequired {
        challenge_token: pending,
    } = db.login(&people[3].0.user.email, PASSWORD).await.unwrap()
    else {
        panic!("challenge")
    };
    db.reset_password(people[3].0.user.id, PASSWORD)
        .await
        .unwrap();
    assert!(
        db.verify_totp(&pending, &backup[1], &secret_key)
            .await
            .is_err(),
        "reset invalidates pending proof of the old password"
    );
    let LoginOutcome::TotpRequired {
        challenge_token: racing_challenge,
    } = db.login(&people[3].0.user.email, PASSWORD).await.unwrap()
    else {
        panic!("challenge")
    };
    let (verify_a, verify_b) = tokio::join!(
        db.verify_totp(&racing_challenge, &backup[2], &secret_key),
        db.verify_totp(&racing_challenge, &backup[2], &secret_key)
    );
    assert_eq!(
        usize::from(verify_a.is_ok()) + usize::from(verify_b.is_ok()),
        1,
        "one challenge and backup code create exactly one session"
    );
    let LoginOutcome::TotpRequired {
        challenge_token: reset_race,
    } = db.login(&people[3].0.user.email, PASSWORD).await.unwrap()
    else {
        panic!("challenge")
    };
    let (verified_race, reset_race) = tokio::join!(
        db.verify_totp(&reset_race, &backup[3], &secret_key),
        db.reset_password(people[3].0.user.id, PASSWORD)
    );
    reset_race.unwrap();
    if let Ok(created) = verified_race {
        assert!(
            db.session_user(&created.token).await.is_err(),
            "reset cannot leave a session minted from the old password challenge"
        );
    }
    // Two ownership transfers from the same owner cannot both win.
    let a = target(people[0].0.user.id, "transfer-a");
    let b = target(outsider.user.id, "transfer-b");
    let (a, b) = tokio::join!(
        db.transfer_business_ownership(&owner_token, &a),
        db.transfer_business_ownership(&owner_token, &b)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let owners: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.business_staff WHERE role='owner'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(owners, 1);
    assert!(matches!(
        db.business_capability(&owner_token, BusinessCapability::OwnershipTransfer)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    db.delete_account(owner.user.id, PASSWORD).await.unwrap();
    let dead: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.business_staff WHERE user_id=$1")
            .bind(owner.user.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(dead, 0);
    assert!(
        sqlx::query("DELETE FROM oxsum.admin_audit WHERE action='business.staff.set'")
            .execute(db.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE oxsum.business_mutations SET action='forged'")
            .execute(db.pool())
            .await
            .is_err()
    );
}
