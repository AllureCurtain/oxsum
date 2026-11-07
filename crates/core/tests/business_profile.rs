//! Profile validation, racing editors, atomic audit and current-role replay.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    BusinessOwnershipRequest, BusinessProfileRequest, BusinessRole, BusinessStaffRequest, Db,
    NewUser, PublicBusinessProfile, WalletError,
};
use uuid::Uuid;

fn request(key: &str, revision: i64) -> BusinessProfileRequest {
    BusinessProfileRequest {
        idempotency_key: key.into(),
        reason: "profile test".into(),
        expected_revision: revision,
        branding: PublicBusinessProfile {
            name: key.into(),
            ..PublicBusinessProfile::default()
        },
    }
}

#[test]
fn unsafe_links_and_incomplete_replacements_are_refused() {
    for url in [
        "javascript:alert(1)",
        "data:text/html,test",
        "//example.com/logo",
        "https://user:secret@example.com",
        "https://example.com/\npath",
    ] {
        let value = PublicBusinessProfile {
            help_url: Some(url.into()),
            ..PublicBusinessProfile::default()
        };
        assert!(value.validated().is_err(), "{url}");
    }
    for email in [
        "bad",
        "help@example.com?subject=spoof",
        "help@example.com\r\nBcc:x",
        "help%0a@example.com",
    ] {
        let value = PublicBusinessProfile {
            contact_email: Some(email.into()),
            ..PublicBusinessProfile::default()
        };
        assert!(value.validated().is_err());
    }
    for color in ["red", "#aabbcc;display:none", "#123", "#12☃☃"] {
        let value = PublicBusinessProfile {
            accent_color: Some(color.into()),
            ..PublicBusinessProfile::default()
        };
        assert!(value.validated().is_err());
    }
    assert!(
        serde_json::from_value::<PublicBusinessProfile>(
            serde_json::json!({"name":"missing optional fields"})
        )
        .is_err()
    );
    let value = PublicBusinessProfile {
        name: "  Example business  ".into(),
        help_url: Some("https://example.com/help".into()),
        contact_email: Some("help@example.com".into()),
        accent_color: Some("#123AbC".into()),
        ..PublicBusinessProfile::default()
    }
    .validated()
    .unwrap();
    assert_eq!(value.name, "Example business");
}

#[tokio::test]
async fn racing_revisions_and_audit_failure_cannot_lose_edits_or_replay_old_authority() {
    let Some(url) = oxsum_testkit::suite_db_url("business_profile_core") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let first = request("first-editor", 0);
    let second = request("second-editor", 0);
    let (a, b) = tokio::join!(
        db.set_business_profile(&owner, &first),
        db.set_business_profile(&owner, &second)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(matches!(
        a.as_ref().err().or(b.as_ref().err()),
        Some(WalletError::Conflict(_))
    ));
    let (saved, req) = if a.is_ok() {
        (a.unwrap(), first)
    } else {
        (b.unwrap(), second)
    };
    assert_eq!(saved.profile.revision, 1);
    let audit_sql = "CREATE FUNCTION oxsum.reject_profile_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='business.profile.update' THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_profile_audit BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_profile_audit();";
    sqlx::raw_sql(audit_sql).execute(db.pool()).await.unwrap();
    let fail = request("failed-editor", 1);
    assert!(db.set_business_profile(&owner, &fail).await.is_err());
    assert_eq!(
        db.business_profile(&owner, &PublicBusinessProfile::default())
            .await
            .unwrap()
            .revision,
        1
    );
    sqlx::query("DROP TRIGGER reject_profile_audit ON oxsum.admin_audit")
        .execute(db.pool())
        .await
        .unwrap();
    let next = db.set_business_profile(&owner, &fail).await.unwrap();
    assert_eq!(next.profile.revision, 2);
    let replay = db.set_business_profile(&owner, &req).await.unwrap();
    assert_eq!(serde_json::json!(replay), serde_json::json!(saved));
    let person = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "business profile successor password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.set_business_staff(
        &owner,
        &BusinessStaffRequest {
            idempotency_key: "staff-successor".into(),
            user_id: person.user.id,
            role: BusinessRole::Admin,
            reason: "transfer fixture".into(),
        },
    )
    .await
    .unwrap();
    db.transfer_business_ownership(
        &owner,
        &BusinessOwnershipRequest {
            idempotency_key: "transfer-successor".into(),
            user_id: person.user.id,
            reason: "transfer fixture".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.set_business_profile(&owner, &req).await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(
        db.business_profile(&owner, &PublicBusinessProfile::default())
            .await
            .unwrap()
            .revision,
        2
    );
}
