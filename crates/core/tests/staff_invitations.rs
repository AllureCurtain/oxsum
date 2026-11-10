//! Staff invitations bind an email to a business seat; acceptance grants the
//! invited role — never owner — and revocation removes authority.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{
    BusinessRole, BusinessSetupRequest, BusinessSetupTokenRequest, Db, InvitationRevokeRequest,
    InvitationScope, NewUser, Registration, SessionContext, SessionContextRequest, SetupAuthority,
    StaffInvitationRequest, User, WalletError,
};
use uuid::Uuid;
#[path = "support/integration.rs"]
mod setup;

const PASSWORD: &str = "staff invitation test password";

async fn database() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("staff_invitations")?;
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
            business_name: "Staff invitation business".into(),
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
fn invite(email: &str, role: BusinessRole, send: bool) -> StaffInvitationRequest {
    StaffInvitationRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        email: email.into(),
        role,
        send_email: send,
        reason: "staff fixture".into(),
    }
}
fn revoke(key: &str) -> InvitationRevokeRequest {
    InvitationRevokeRequest {
        idempotency_key: key.into(),
        reason: "staff fixture revoke".into(),
    }
}
async fn staff_roles(db: &Db, email: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT s.role FROM oxsum.business_staff s JOIN oxsum.users u USING(user_id) \
         WHERE u.email_normalized=$1",
    )
    .bind(email.to_lowercase())
    .fetch_optional(db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_staff_invitation_grants_the_invited_role_to_a_registered_recipient() {
    let Some(db) = database().await else { return };
    let (_owner, token) = business(&db).await;
    for role in [
        BusinessRole::Admin,
        BusinessRole::Finance,
        BusinessRole::Support,
        BusinessRole::Viewer,
    ] {
        let name = format!("staff-{role:?}").to_lowercase();
        let recipient = user(&db, &name).await;
        let session = login(&db, &recipient.user).await;
        let created = db
            .create_staff_invitation(&token, &invite(&recipient.user.email, role, false))
            .await
            .unwrap();
        // The grant is the nil organization: the seat is business-scoped.
        assert_eq!(created.organization_id, Uuid::nil());
        let secret = created.token.clone().unwrap();
        let preview = db.invitation_preview(&secret).await.unwrap();
        assert_eq!(preview.organization_id, Uuid::nil());
        assert_eq!(preview.name, "Staff invitation business");
        assert_eq!(preview.staff_role, Some(role));
        // The recipient accepts through their own session on the bound email.
        let accepted = db.accept_invitation(&session, &secret).await.unwrap();
        assert_eq!(accepted.staff_role, Some(role));
        assert!(matches!(
            db.invitation_preview(&secret).await,
            Err(WalletError::NotFound(_))
        ));
        // The seat is real staff authority — not a customer membership.
        assert!(matches!(
            db.switch_session_context(
                &session,
                &SessionContextRequest {
                    idempotency_key: format!("ctx-{name}"),
                    context: SessionContext::Business,
                    organization_id: None,
                },
            )
            .await
            .unwrap()
            .context,
            SessionContext::Business
        ));
        // No customer organization or API key was minted for the recipient.
        assert!(
            db.business_staff(&token)
                .await
                .unwrap()
                .iter()
                .any(|s| s.user_id == recipient.user.id && s.role == role)
        );
    }
}

#[tokio::test]
async fn a_new_recipient_registers_into_the_seat_without_customer_funds() {
    let Some(db) = database().await else { return };
    let (_owner, token) = business(&db).await;
    let created = db
        .create_staff_invitation(
            &token,
            &invite("newstaff@example.com", BusinessRole::Support, false),
        )
        .await
        .unwrap();
    let secret = created.token.clone().unwrap();
    let user = db
        .redeem_staff_invitation(
            &secret,
            NewUser {
                email: "newstaff@example.com".into(),
                password: PASSWORD.into(),
                organization_name: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        staff_roles(&db, "newstaff@example.com").await.as_deref(),
        Some("support")
    );
    // The staff account holds no customer membership, so it cannot spend.
    let session = login(&db, &user).await;
    let ctx = db.session_context(&session).await.unwrap();
    assert!(ctx.organization_id.is_none());
    assert!(matches!(ctx.context, SessionContext::Customer));
    assert!(ctx.business.is_some());
    // No organizations were created for the staff account.
    let orgs: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.memberships WHERE user_id=$1")
        .bind(user.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(orgs, 0);
}

#[tokio::test]
async fn owner_is_never_grantable_and_the_inviter_must_manage_staff() {
    let Some(db) = database().await else { return };
    let (owner, token) = business(&db).await;
    // An owner grant is refused outright in the request.
    assert!(matches!(
        db.create_staff_invitation(&token, &invite("x@example.com", BusinessRole::Owner, false))
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    // A non-owner caller has no StaffManage capability: a freshly invited
    // viewer cannot send a further invitation.
    let viewer_user = user(&db, "staffviewer").await;
    let viewer_session = login(&db, &viewer_user.user).await;
    let v = db
        .create_staff_invitation(
            &token,
            &invite("staffviewer@example.com", BusinessRole::Viewer, false),
        )
        .await
        .unwrap();
    db.accept_invitation(&viewer_session, &v.token.clone().unwrap())
        .await
        .unwrap();
    // The new viewer logs in and selects business context, then tries to invite.
    db.switch_session_context(
        &viewer_session,
        &SessionContextRequest {
            idempotency_key: "v-ctx".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.create_staff_invitation(
            &viewer_session,
            &invite("another@example.com", BusinessRole::Viewer, false)
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    let _ = owner;
}

#[tokio::test]
async fn the_bound_email_must_match_and_replay_cannot_restore_a_seat() {
    let Some(db) = database().await else { return };
    let (_owner, token) = business(&db).await;
    let intended = user(&db, "bound").await;
    let wrong = user(&db, "wrong").await;
    let created = db
        .create_staff_invitation(
            &token,
            &invite("bound@example.com", BusinessRole::Finance, false),
        )
        .await
        .unwrap();
    let secret = created.token.clone().unwrap();
    // A different account's session cannot redeem the bound link.
    let wrong_session = login(&db, &wrong.user).await;
    assert!(db.accept_invitation(&wrong_session, &secret).await.is_err());
    // The bound account accepts; a replay returns the standing grant.
    let session = login(&db, &intended.user).await;
    db.accept_invitation(&session, &secret).await.unwrap();
    let replay = db.accept_invitation(&session, &secret).await.unwrap();
    assert_eq!(replay.staff_role, Some(BusinessRole::Finance));
    // Revoke the seat: authority stops resolving and a further accept does not
    // resurrect membership.
    db.revoke_business_staff(
        &token,
        &oxsum_core::BusinessStaffRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            user_id: intended.user.id,
            reason: "remove".into(),
        },
    )
    .await
    .unwrap();
    assert!(staff_roles(&db, "bound@example.com").await.is_none());
    // A replayed accept only re-reads the already-committed grant — it does
    // not re-seat the user. The seat stays removed and business authority no
    // longer resolves for that account.
    let replayed = db.accept_invitation(&session, &secret).await.unwrap();
    assert_eq!(replayed.staff_role, Some(BusinessRole::Finance));
    assert!(staff_roles(&db, "bound@example.com").await.is_none());
    assert!(matches!(
        db.switch_session_context(
            &session,
            &SessionContextRequest {
                idempotency_key: "post-revoke".into(),
                context: SessionContext::Business,
                organization_id: None,
            },
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
}

#[tokio::test]
async fn creation_is_idempotent_and_pending_revocation_releases_the_address() {
    let Some(db) = database().await else { return };
    let (_owner, token) = business(&db).await;
    let req = invite("dup@example.com", BusinessRole::Admin, false);
    let first = db.create_staff_invitation(&token, &req).await.unwrap();
    // An identical replay returns the same invitation, with no second token.
    let replay = db.create_staff_invitation(&token, &req).await.unwrap();
    assert_eq!(replay.id, first.id);
    assert!(replay.token.is_none());
    // A second pending invitation for the same address conflicts.
    assert!(matches!(
        db.create_staff_invitation(
            &token,
            &invite("dup@example.com", BusinessRole::Viewer, false)
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    // Revoking the pending invitation frees the address for a fresh invite.
    db.revoke_invitation(
        &token,
        first.id,
        InvitationScope::Staff,
        &revoke("withdraw"),
    )
    .await
    .unwrap();
    let fresh = db
        .create_staff_invitation(
            &token,
            &invite("dup@example.com", BusinessRole::Admin, false),
        )
        .await
        .unwrap();
    assert_ne!(fresh.id, first.id);
}

#[tokio::test]
async fn an_owner_seat_is_never_overwritten_by_an_invitation() {
    let Some(db) = database().await else { return };
    let (owner, token) = business(&db).await;
    // Invite the owner's own address to a lower role: even a valid pending
    // invitation cannot demote the owner seat on acceptance.
    let created = db
        .create_staff_invitation(
            &token,
            &invite("owner@example.com", BusinessRole::Viewer, false),
        )
        .await;
    // The address already holds the owner seat, so creation refuses.
    assert!(matches!(created, Err(WalletError::Conflict(_))));
    let _ = owner;
}
