//! A04/A05/A06: consent, identity preservation and atomic invitation transitions.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{
    BusinessRole, BusinessSetupRequest, BusinessSetupTokenRequest, BusinessStaffRequest,
    CustomerInvitationRequest, Db, InvitationRevokeRequest, Kind, LoginOutcome, MembershipActor,
    NewUser, Registration, Role, SecretKey, SessionContext, SessionContextRequest, SetupAuthority,
    TeamInvitationRequest, User, WalletError,
};
use serde_json::json;
use sqlx::Executor;
const PASSWORD: &str = "customer invitation test password";
async fn database() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("invitation_consent")?;
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
async fn user(db: &Db, name: &str) -> Registration {
    db.register(NewUser {
        email: format!("{name}@example.com"),
        password: PASSWORD.into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
async fn login(db: &Db, user: &User) -> String {
    db.login(&user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token
}
async fn business(db: &Db) -> (Registration, String) {
    let owner = user(db, "owner").await;
    let token = login(db, &owner.user).await;
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
        &token,
        &BusinessSetupRequest {
            token: setup.token.unwrap(),
            business_name: "Invitations business".into(),
        },
    )
    .await
    .unwrap();
    db.switch_session_context(
        &token,
        &SessionContextRequest {
            idempotency_key: "business".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    (owner, token)
}
fn customer(key: &str, email: &str) -> CustomerInvitationRequest {
    CustomerInvitationRequest {
        send_email: false,
        idempotency_key: key.into(),
        email: email.into(),
        name: "Invited account".into(),
        external_id: format!("external-{key}"),
        reason: "Agreed customer enrollment".into(),
    }
}
fn team(key: &str, email: Option<&str>) -> TeamInvitationRequest {
    TeamInvitationRequest {
        idempotency_key: key.into(),
        intended_email: email.map(str::to_owned),
        email: None,
    }
}
fn revoke(key: &str) -> InvitationRevokeRequest {
    InvitationRevokeRequest {
        idempotency_key: key.into(),
        reason: "Withdraw pending invitation".into(),
    }
}
async fn count(db: &Db, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM oxsum.{table}"))
        .fetch_one(db.pool())
        .await
        .unwrap()
}
#[tokio::test]
async fn business_invites_preserve_existing_identity_and_bind_the_new_customer() {
    let Some(db) = database().await else { return };
    let (_, owner) = business(&db).await;
    let recipient = user(&db, "recipient").await;
    let stranger = user(&db, "stranger").await;
    let session = login(&db, &recipient.user).await;
    let before_org = count(&db, "organizations").await;
    let before_keys = count(&db, "api_keys").await;
    let terms = customer("consent", &recipient.user.email.to_uppercase());
    assert!(matches!(
        db.create_customer_invitation(&session, &terms).await,
        Err(WalletError::Forbidden(_))
    ));
    let invite = db.create_customer_invitation(&owner, &terms).await.unwrap();
    let secret = invite.token.as_deref().unwrap();
    assert_eq!(count(&db, "organizations").await, before_org);
    assert_eq!(count(&db, "api_keys").await, before_keys);
    let preview = db.invitation_preview(secret).await.unwrap();
    assert_eq!(preview.organization_id, invite.organization_id);
    assert_eq!(preview.role, Role::Owner);
    let replay = db.create_customer_invitation(&owner, &terms).await.unwrap();
    assert_eq!(replay.id, invite.id);
    assert!(replay.token.is_none());
    let mut changed = customer("consent", &recipient.user.email);
    changed.name = "Different".into();
    assert!(matches!(
        db.create_customer_invitation(&owner, &changed).await,
        Err(WalletError::Conflict(_))
    ));
    let mut duplicate = customer("duplicate", &stranger.user.email);
    duplicate.external_id = terms.external_id.clone();
    assert!(matches!(
        db.create_customer_invitation(&owner, &duplicate).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.accept_invitation(&login(&db, &stranger.user).await, secret)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.accept_invitation(&recipient.api_key.secret, secret)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    let (a, b) = tokio::join!(
        db.accept_invitation(&session, secret),
        db.accept_invitation(&session, secret)
    );
    assert_eq!(a.unwrap().id, b.unwrap().id);
    assert_eq!(count(&db, "organizations").await, before_org + 1);
    assert_eq!(count(&db, "api_keys").await, before_keys);
    let profile = db
        .business_customer(&owner, invite.organization_id)
        .await
        .unwrap();
    assert_eq!(profile.external_id, terms.external_id);
    assert_eq!(
        profile.contact_email.as_deref(),
        Some(recipient.user.email.as_str())
    );
    let original = db
        .authenticate(&recipient.api_key.secret)
        .await
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(original.id, recipient.organization.id);
    assert_eq!(original.tenant_id, recipient.organization.tenant_id);
    assert_eq!(
        db.authenticate_session(&session)
            .await
            .unwrap()
            .unwrap()
            .organization
            .id,
        recipient.organization.id
    );
    assert_eq!(
        db.organizations_of(recipient.user.id).await.unwrap().len(),
        2
    );
    assert_eq!(count(&db, "business_staff").await, 1);
    assert!(db.invitation_preview(secret).await.is_err());
    assert!(matches!(
        db.revoke_invitation(&owner, invite.id, true, &revoke("too-late"))
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.create_customer_invitation(&owner, &duplicate).await,
        Err(WalletError::Conflict(_))
    ));
    let audit: serde_json::Value =
        sqlx::query_scalar("SELECT jsonb_agg(detail) FROM oxsum.admin_audit")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(!audit.to_string().contains(secret));
}
#[tokio::test]
async fn team_consent_replay_does_not_restore_removed_access_and_authority_is_current() {
    let Some(db) = database().await else { return };
    let owner = user(&db, "teamowner").await;
    let token = login(&db, &owner.user).await;
    let recipient = user(&db, "teammember").await;
    let session = login(&db, &recipient.user).await;
    let stranger = user(&db, "stranger").await;
    let other = login(&db, &stranger.user).await;
    let invite = db
        .create_invitation(&token, &team("first", Some(&recipient.user.email)))
        .await
        .unwrap();
    let secret = invite.token.unwrap();
    assert_eq!(db.members(owner.organization.id).await.unwrap().len(), 1);
    assert!(matches!(
        db.revoke_invitation(&other, invite.id, false, &revoke("foreign"))
            .await,
        Err(WalletError::NotFound(_))
    ));
    db.accept_invitation(&session, &secret).await.unwrap();
    assert_eq!(
        db.organization_by_id(owner.organization.id)
            .await
            .unwrap()
            .kind,
        Kind::Team
    );
    let actor = MembershipActor {
        user_id: owner.user.id,
        role: Role::Owner,
    };
    db.switch_session_context(
        &session,
        &SessionContextRequest {
            idempotency_key: "select".into(),
            context: SessionContext::Customer,
            organization_id: Some(owner.organization.id),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.create_invitation(&session, &team("member", None)).await,
        Err(WalletError::Forbidden(_))
    ));
    db.change_member_role(owner.organization.id, actor, recipient.user.id, Role::Admin)
        .await
        .unwrap();
    let admin_invite = db
        .create_invitation(&session, &team("admin", None))
        .await
        .unwrap();
    db.change_member_role(
        owner.organization.id,
        actor,
        recipient.user.id,
        Role::Member,
    )
    .await
    .unwrap();
    assert!(matches!(
        db.create_invitation(&session, &team("admin", None)).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.revoke_invitation(&session, admin_invite.id, false, &revoke("demoted"))
            .await,
        Err(WalletError::Forbidden(_))
    ));
    let stale = MembershipActor {
        user_id: recipient.user.id,
        role: Role::Admin,
    };
    assert!(matches!(
        db.change_member_role(owner.organization.id, stale, owner.user.id, Role::Member)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.remove_member(owner.organization.id, stale, owner.user.id)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    db.remove_member(owner.organization.id, actor, recipient.user.id)
        .await
        .unwrap();
    let receipt = db.accept_invitation(&session, &secret).await.unwrap();
    assert_eq!(receipt.organization_id, owner.organization.id);
    assert_eq!(db.members(owner.organization.id).await.unwrap().len(), 1);
    assert!(db.authenticate_session(&session).await.unwrap().is_none());
    let id = db
        .revoke_invitation(&token, admin_invite.id, false, &revoke("revoke"))
        .await
        .unwrap()
        .id;
    assert_eq!(
        db.revoke_invitation(&token, admin_invite.id, false, &revoke("revoke"))
            .await
            .unwrap()
            .id,
        id
    );
    assert!(
        db.accept_invitation(&other, admin_invite.token.as_deref().unwrap())
            .await
            .is_err()
    );
    let expires = db
        .create_invitation(&token, &team("expires", None))
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.invitations SET expires_at=clock_timestamp()-interval '1 second' WHERE invitation_id=$1").bind(expires.id).execute(db.pool()).await.unwrap();
    assert!(
        db.accept_invitation(&other, expires.token.as_deref().unwrap())
            .await
            .is_err()
    );
    // A confirmed factor invalidates older sessions for personal consent.
    let factor = SecretKey::from_bytes([43; 32]);
    let seed = oxsum_core::totp::generate_secret();
    let sealed = factor
        .seal(&oxsum_core::totp::base32_encode_pub(&seed))
        .unwrap();
    db.enroll_totp(stranger.user.id, &sealed).await.unwrap();
    let code = oxsum_core::totp::code(&seed, time::OffsetDateTime::now_utc().unix_timestamp());
    let backup = db
        .confirm_totp(stranger.user.id, &code, &factor)
        .await
        .unwrap();
    let second = db
        .create_invitation(&token, &team("second-factor", Some(&stranger.user.email)))
        .await
        .unwrap();
    assert!(matches!(
        db.accept_invitation(&other, second.token.as_deref().unwrap())
            .await,
        Err(WalletError::Forbidden(_))
    ));
    let LoginOutcome::TotpRequired { challenge_token } =
        db.login(&stranger.user.email, PASSWORD).await.unwrap()
    else {
        panic!("factor required")
    };
    let verified = db
        .verify_totp(&challenge_token, &backup[0], &factor)
        .await
        .unwrap();
    db.accept_invitation(&verified.token, second.token.as_deref().unwrap())
        .await
        .unwrap();
    db.logout(&token).await.unwrap();
    assert!(matches!(
        db.create_invitation(&token, &team("logout", None)).await,
        Err(WalletError::InvalidCredentials)
    ));
}
#[tokio::test]
async fn registration_revoke_races_and_audit_failures_leave_no_partial_customer() {
    let Some(db) = database().await else { return };
    let (_, owner) = business(&db).await;
    let terms = customer("fresh", "fresh@example.com");
    let invite = db.create_customer_invitation(&owner, &terms).await.unwrap();
    let secret = invite.token.as_deref().unwrap();
    let new = || NewUser {
        email: terms.email.clone(),
        password: PASSWORD.into(),
        organization_name: None,
    };
    assert!(
        db.register_customer(new()).await.is_err(),
        "invite-only remains enforced"
    );
    let before = (
        count(&db, "organizations").await,
        count(&db, "users").await,
        count(&db, "api_keys").await,
    );
    db.pool().execute("CREATE FUNCTION oxsum.reject_invite_audit() RETURNS trigger AS $$ BEGIN IF NEW.action='invitation.accept' THEN RAISE EXCEPTION 'forced audit rollback'; END IF; RETURN NEW; END $$ LANGUAGE plpgsql; CREATE TRIGGER reject_invite_audit BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_invite_audit();").await.unwrap();
    assert!(db.redeem_invitation(secret, new()).await.is_err());
    assert_eq!(
        (
            count(&db, "organizations").await,
            count(&db, "users").await,
            count(&db, "api_keys").await
        ),
        before
    );
    assert!(db.invitation_preview(secret).await.is_ok());
    db.pool().execute("DROP TRIGGER reject_invite_audit ON oxsum.admin_audit; DROP FUNCTION oxsum.reject_invite_audit();").await.unwrap();
    let (a, b) = tokio::join!(
        db.redeem_invitation(secret, new()),
        db.redeem_invitation(secret, new())
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let registered = a.or(b).unwrap();
    assert_eq!(registered.organization.id, invite.organization_id);
    assert_eq!(
        db.members(invite.organization_id).await.unwrap()[0].role,
        Role::Owner
    );
    assert_eq!(count(&db, "business_staff").await, 1);
    for index in 0..6 {
        let email = format!("race{index}@example.com");
        let terms = customer(&format!("race{index}"), &email);
        let invite = db.create_customer_invitation(&owner, &terms).await.unwrap();
        let secret = invite.token.unwrap();
        let rev = revoke(&format!("revoke-race{index}"));
        let new = NewUser {
            email,
            password: PASSWORD.into(),
            organization_name: None,
        };
        let (accepted, revoked) = tokio::join!(
            db.redeem_invitation(&secret, new),
            db.revoke_invitation(&owner, invite.id, true, &rev)
        );
        assert_ne!(accepted.is_ok(), revoked.is_ok());
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.organizations WHERE organization_id=$1)",
        )
        .bind(invite.organization_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(exists, accepted.is_ok());
    }
    // Revoke releases the external ID reservation. The old secret stays dead.
    let terms = customer("release", "release@example.com");
    let old = db.create_customer_invitation(&owner, &terms).await.unwrap();
    db.revoke_invitation(&owner, old.id, true, &revoke("revoke-release"))
        .await
        .unwrap();
    let mut replacement = terms;
    replacement.idempotency_key = "replacement".into();
    assert!(
        db.create_customer_invitation(&owner, &replacement)
            .await
            .is_ok()
    );
    assert!(
        db.invitation_preview(old.token.as_deref().unwrap())
            .await
            .is_err()
    );
    // Expiry frees the reservation but never makes the expired secret valid.
    let expiring = customer("expiring", "expiring@example.com");
    let old = db
        .create_customer_invitation(&owner, &expiring)
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.invitations SET expires_at=clock_timestamp()-interval '1 second' WHERE invitation_id=$1").bind(old.id).execute(db.pool()).await.unwrap();
    let mut renewed = expiring;
    renewed.idempotency_key = "renewed".into();
    let live = db
        .create_customer_invitation(&owner, &renewed)
        .await
        .unwrap();
    assert!(
        db.redeem_invitation(
            old.token.as_deref().unwrap(),
            NewUser {
                email: renewed.email,
                password: PASSWORD.into(),
                organization_name: None
            }
        )
        .await
        .is_err()
    );
    // Creation and revocation fail atomically if their audit insert fails.
    db.pool().execute("CREATE FUNCTION oxsum.reject_invitation_writes() RETURNS trigger AS $$ BEGIN IF NEW.action IN ('business.customer.invitation.create','invitation.revoke') THEN RAISE EXCEPTION 'forced write rollback'; END IF; RETURN NEW; END $$ LANGUAGE plpgsql; CREATE TRIGGER reject_invitation_writes BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_invitation_writes();").await.unwrap();
    let before = count(&db, "invitations").await;
    assert!(
        db.create_customer_invitation(&owner, &customer("rollback", "rollback@example.com"))
            .await
            .is_err()
    );
    assert_eq!(count(&db, "invitations").await, before);
    assert!(
        db.revoke_invitation(&owner, live.id, true, &revoke("failed-revoke"))
            .await
            .is_err()
    );
    assert!(
        db.invitation_preview(live.token.as_deref().unwrap())
            .await
            .is_ok()
    );
    db.pool().execute("DROP TRIGGER reject_invitation_writes ON oxsum.admin_audit; DROP FUNCTION oxsum.reject_invitation_writes();").await.unwrap();
    assert!(
        sqlx::query("DELETE FROM oxsum.admin_audit WHERE action='invitation.accept'")
            .execute(db.pool())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn role_matrix_and_same_external_id_creation_race_fail_closed() {
    let Some(db) = database().await else { return };
    let (_, owner) = business(&db).await;
    for (name, role, allowed) in [
        ("admin", BusinessRole::Admin, true),
        ("finance", BusinessRole::Finance, false),
        ("support", BusinessRole::Support, false),
        ("viewer", BusinessRole::Viewer, false),
    ] {
        let person = user(&db, name).await;
        let token = login(&db, &person.user).await;
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                idempotency_key: name.into(),
                user_id: person.user.id,
                role,
                reason: "Role matrix fixture".into(),
            },
        )
        .await
        .unwrap();
        db.switch_session_context(
            &token,
            &SessionContextRequest {
                idempotency_key: "select".into(),
                context: SessionContext::Business,
                organization_id: None,
            },
        )
        .await
        .unwrap();
        let invite = db
            .create_customer_invitation(&token, &customer(name, &format!("new-{name}@example.com")))
            .await;
        assert_eq!(invite.is_ok(), allowed, "{name}");
        if let Ok(invite) = invite {
            db.revoke_invitation(&token, invite.id, true, &revoke("revoke"))
                .await
                .unwrap();
        }
    }
    let first = customer("collision", "a@example.com");
    let mut second = customer("other-key", "b@example.com");
    second.external_id = first.external_id.clone();
    let (a, b) = tokio::join!(
        db.create_customer_invitation(&owner, &first),
        db.create_customer_invitation(&owner, &second)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let stored:serde_json::Value=sqlx::query_scalar("SELECT request FROM oxsum.business_mutations WHERE action='business.customer.invitation.create' AND idempotency_key=$1").bind(if a.is_ok(){"collision"}else{"other-key"}).fetch_one(db.pool()).await.unwrap();
    assert_eq!(stored["externalId"], json!(first.external_id));
}

#[tokio::test]
async fn customer_invitation_history_is_scoped_and_never_contains_a_secret() {
    let Some(db) = database().await else { return };
    let (_, owner) = business(&db).await;
    let recipient = user(&db, "history-recipient").await;
    let recipient_token = login(&db, &recipient.user).await;
    let accepted = db
        .create_customer_invitation(&owner, &customer("accepted", &recipient.user.email))
        .await
        .unwrap();
    db.accept_invitation(&recipient_token, accepted.token.as_deref().unwrap())
        .await
        .unwrap();
    let revoked = db
        .create_customer_invitation(&owner, &customer("revoked", "revoked@example.com"))
        .await
        .unwrap();
    db.revoke_invitation(&owner, revoked.id, true, &revoke("revoke-history"))
        .await
        .unwrap();
    let expired = db
        .create_customer_invitation(&owner, &customer("expired", "expired@example.com"))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.invitations SET expires_at=now()-interval '1 second' WHERE invitation_id=$1",
    )
    .bind(expired.id)
    .execute(db.pool())
    .await
    .unwrap();
    let pending = db
        .create_customer_invitation(&owner, &customer("pending", "pending@example.com"))
        .await
        .unwrap();
    let team = db
        .create_invitation(&recipient_token, &team("team-hidden", None))
        .await
        .unwrap();
    let page = db.customer_invitations(&owner, None, 2).await.unwrap();
    assert_eq!(page.registration_mode, "invite");
    assert_eq!(page.invitations.len(), 2);
    assert_eq!(page.invitations[0].id, pending.id);
    assert_eq!(page.invitations[0].state, "pending");
    assert_eq!(page.invitations[1].state, "expired");
    let older = db
        .customer_invitations(&owner, page.next_cursor, 2)
        .await
        .unwrap();
    assert_eq!(older.invitations.len(), 2);
    assert!(older.next_cursor.is_none());
    assert_eq!(older.invitations[0].state, "revoked");
    assert_eq!(older.invitations[1].state, "accepted");
    let wire = serde_json::to_string(&page).unwrap();
    assert!(!wire.contains("token"));
    assert!(!wire.contains(pending.token.as_deref().unwrap()));
    assert!(matches!(
        db.customer_invitations(&owner, Some(team.id), 2).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.customer_invitations(&recipient_token, None, 2).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(db.customer_invitations(&owner, None, 0).await.is_err());
}

#[tokio::test]
async fn invitation_mail_claim_survives_replay_and_crash_without_a_second_send() {
    let Some(db) = database().await else { return };
    let (_, owner) = business(&db).await;
    let mut terms = customer("mail-once", "Receiver@Example.com");
    terms.send_email = true;
    let invite = db.create_customer_invitation(&owner, &terms).await.unwrap();
    let secret = invite.token.as_deref().unwrap();
    assert!(
        db.begin_customer_invitation_mail(&owner, invite.id, "wrong secret", true)
            .await
            .unwrap()
            .is_none()
    );
    let (a, b) = tokio::join!(
        db.begin_customer_invitation_mail(&owner, invite.id, secret, true),
        db.begin_customer_invitation_mail(&owner, invite.id, secret, true)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.is_some(), b.is_some());
    let target = a.or(b).unwrap();
    assert_eq!(target.email, "receiver@example.com");
    assert_eq!(
        db.customer_invitation_mail_state(&owner, invite.id)
            .await
            .unwrap(),
        oxsum_core::InvitationDeliveryState::Unknown
    );
    // Simulate interruption after the durable claim: a reconnect still cannot
    // claim another send, and replay returns no plaintext token.
    let replay = db.create_customer_invitation(&owner, &terms).await.unwrap();
    assert!(replay.token.is_none());
    assert!(
        db.begin_customer_invitation_mail(&owner, invite.id, secret, true)
            .await
            .unwrap()
            .is_none()
    );
    db.complete_customer_invitation_mail(&target).await.unwrap();
    db.complete_customer_invitation_mail(&target).await.unwrap();
    assert_eq!(
        db.customer_invitation_mail_state(&owner, invite.id)
            .await
            .unwrap(),
        oxsum_core::InvitationDeliveryState::Submitted
    );
    assert!(
        sqlx::query(
            "UPDATE oxsum.invitations SET mail_state='not_attempted' WHERE invitation_id=$1"
        )
        .bind(invite.id)
        .execute(db.pool())
        .await
        .is_err()
    );
    terms.send_email = false;
    assert!(matches!(
        db.create_customer_invitation(&owner, &terms).await,
        Err(WalletError::Conflict(_))
    ));

    let mut unavailable = customer("unavailable", "unavailable@example.com");
    unavailable.send_email = true;
    let absent = db
        .create_customer_invitation(&owner, &unavailable)
        .await
        .unwrap();
    assert!(
        db.begin_customer_invitation_mail(
            &owner,
            absent.id,
            absent.token.as_deref().unwrap(),
            false
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(
        db.customer_invitation_mail_state(&owner, absent.id)
            .await
            .unwrap(),
        oxsum_core::InvitationDeliveryState::Unavailable
    );
    assert!(
        db.begin_customer_invitation_mail(
            &owner,
            absent.id,
            absent.token.as_deref().unwrap(),
            true
        )
        .await
        .unwrap()
        .is_none()
    );

    let copy = db
        .create_customer_invitation(&owner, &customer("copy-only", "copy@example.com"))
        .await
        .unwrap();
    assert!(
        db.begin_customer_invitation_mail(&owner, copy.id, copy.token.as_deref().unwrap(), true)
            .await
            .unwrap()
            .is_none()
    );
    let mut revoked = customer("revoked-mail", "revoked-mail@example.com");
    revoked.send_email = true;
    let revoked = db
        .create_customer_invitation(&owner, &revoked)
        .await
        .unwrap();
    db.revoke_invitation(&owner, revoked.id, true, &revoke("stop-mail"))
        .await
        .unwrap();
    assert!(
        db.begin_customer_invitation_mail(
            &owner,
            revoked.id,
            revoked.token.as_deref().unwrap(),
            true
        )
        .await
        .unwrap()
        .is_none()
    );
}
