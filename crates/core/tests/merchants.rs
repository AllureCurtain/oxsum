//! A16 configuration authority, encrypted history and immutable replay results.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    BusinessRole, BusinessStaffRequest, BusinessStaffRevokeRequest, Db, MerchantCreateRequest,
    MerchantMode, MerchantRail, MerchantStatusRequest, MerchantVersionInput,
    MerchantVersionRequest, NewUser, SecretKey, SessionContext, SessionContextRequest, WalletError,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;
fn secret() -> SecretKey {
    SecretKey::from_bytes([37; 32])
}
async fn fixture() -> Option<(Db, String)> {
    let url = oxsum_testkit::suite_db_url("merchants")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    Some((db, owner))
}
fn input() -> MerchantVersionInput {
    serde_json::from_value(json!({"apiUrl":"https://sandbox.hyperswitch.io","apiKey":"snd_merchant_secret","webhookSecret":"merchant-signing-secret","publishableKey":"pk_snd_merchant","sdkUrl":"https://sdk.example"})).unwrap()
}
fn create() -> MerchantCreateRequest {
    MerchantCreateRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "merchant fixture".into(),
        name: format!("merchant-{}", Uuid::new_v4().simple()),
        rail: MerchantRail::Hyperswitch,
        mode: MerchantMode::Test,
        account_id: Uuid::new_v4().to_string(),
        config: input(),
    }
}
async fn staff(db: &Db, owner: &str, role: BusinessRole) -> (String, Uuid) {
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "merchant fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.set_business_staff(
        owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "merchant fixture staff".into(),
            user_id: registration.user.id,
            role,
        },
    )
    .await
    .unwrap();
    let token = db
        .login(&registration.user.email, "merchant fixture password")
        .await
        .unwrap()
        .into_session()
        .token;
    db.switch_session_context(
        &token,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    (token, registration.user.id)
}
#[tokio::test]
async fn owner_writes_staff_reads_and_current_role_revocation_is_enforced() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let created = db
        .create_merchant(&owner, &create(), &secret())
        .await
        .unwrap();
    for role in [
        BusinessRole::Admin,
        BusinessRole::Finance,
        BusinessRole::Support,
        BusinessRole::Viewer,
    ] {
        let (token, id) = staff(&db, &owner, role).await;
        assert!(matches!(
            db.create_merchant(&token, &create(), &secret()).await,
            Err(WalletError::Forbidden(_))
        ));
        assert!(matches!(
            db.rotate_merchant(
                &token,
                created.merchant_id,
                &MerchantVersionRequest {
                    idempotency_key: Uuid::new_v4().to_string(),
                    reason: "forbidden rotate".into(),
                    config: input()
                },
                &secret()
            )
            .await,
            Err(WalletError::Forbidden(_))
        ));
        assert!(matches!(
            db.set_merchant_enabled(
                &token,
                created.merchant_id,
                &MerchantStatusRequest {
                    idempotency_key: Uuid::new_v4().to_string(),
                    reason: "forbidden status".into(),
                    enabled: true
                }
            )
            .await,
            Err(WalletError::Forbidden(_))
        ));
        let list = db.business_merchants(&token, None, 100).await;
        if role == BusinessRole::Support {
            assert!(matches!(list, Err(WalletError::Forbidden(_))));
        } else {
            assert!(list.is_ok());
            assert_eq!(
                db.business_merchant_versions(&token, created.merchant_id, None, 100)
                    .await
                    .unwrap()
                    .versions
                    .len(),
                1
            );
        }
        db.revoke_business_staff(
            &owner,
            &BusinessStaffRevokeRequest {
                user_id: id,
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "revoked merchant reader".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            db.business_merchant_versions(&token, created.merchant_id, None, 10)
                .await,
            Err(WalletError::Forbidden(_))
        ));
    }
}
#[tokio::test]
async fn rotation_preserves_pinned_context_and_original_replay_without_leaking_secrets() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let request = create();
    let first = db
        .create_merchant(&owner, &request, &secret())
        .await
        .unwrap();
    let mut rotated = input();
    rotated.api_key = "snd_rotated_api_secret".into();
    rotated.webhook_secret = "rotated-signing-secret".into();
    let rotation = MerchantVersionRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "key rotation".into(),
        config: rotated,
    };
    let second = db
        .rotate_merchant(&owner, first.merchant_id, &rotation, &secret())
        .await
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_ne!(first.version_id, second.version_id);
    let enabled = db
        .set_merchant_enabled(
            &owner,
            first.merchant_id,
            &MerchantStatusRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "enable test checkout".into(),
                enabled: true,
            },
        )
        .await
        .unwrap();
    assert!(enabled.enabled);
    assert_eq!(
        db.create_merchant(&owner, &request, &secret())
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        db.rotate_merchant(&owner, first.merchant_id, &rotation, &secret())
            .await
            .unwrap(),
        second
    );
    let old = db
        .merchant_signing_context(first.merchant_id, first.version_id, &secret())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.api_key(), "snd_merchant_secret");
    assert_eq!(old.webhook_secret(), "merchant-signing-secret");
    let current = db
        .merchant_signing_context(first.merchant_id, second.version_id, &secret())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.webhook_secret(), "rotated-signing-secret");
    assert!(
        db.merchant_signing_context(Uuid::new_v4(), first.version_id, &secret())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.merchant_signing_context(
            first.merchant_id,
            first.version_id,
            &SecretKey::from_bytes([38; 32])
        )
        .await
        .is_err()
    );
    db.check_merchant_secrets(&secret()).await.unwrap();
    assert!(
        db.check_merchant_secrets(&SecretKey::from_bytes([38; 32]))
            .await
            .is_err()
    );
    let page = db
        .business_merchant_versions(&owner, first.merchant_id, None, 1)
        .await
        .unwrap();
    assert_eq!(page.versions.len(), 1);
    assert_eq!(page.next_cursor, Some(1));
    let tail = db
        .business_merchant_versions(&owner, first.merchant_id, page.next_cursor, 1)
        .await
        .unwrap();
    assert_eq!(tail.versions[0].revision, 2);
    assert_eq!(tail.next_cursor, None);
    let serialized =
        json!({"list":db.business_merchants(&owner,None,100).await.unwrap(),"history":page})
            .to_string();
    for sensitive in [
        "snd_merchant_secret",
        "merchant-signing-secret",
        "snd_rotated_api_secret",
        "rotated-signing-secret",
        "credentialsSealed",
        "credentials_sealed",
        "apiKey",
        "webhookSecret",
    ] {
        assert!(!serialized.contains(sensitive), "leaked {sensitive}");
    }
    let rows =
        sqlx::query("SELECT credentials_sealed FROM oxsum.merchant_versions WHERE merchant_id=$1")
            .bind(first.merchant_id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    for row in rows {
        let cipher: String = row.get("credentials_sealed");
        assert!(!cipher.contains("secret"));
        assert!(!serialized.contains(&cipher));
    }
    let audit: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT request FROM oxsum.business_mutations WHERE mutation_id=ANY($1)",
    )
    .bind(vec![first.id, second.id, enabled.id])
    .fetch_all(db.pool())
    .await
    .unwrap();
    let audit = json!(audit).to_string();
    assert!(!audit.contains("signing-secret"));
    assert!(!audit.contains("api_secret"));
    assert!(
        sqlx::query(
            "UPDATE oxsum.merchant_versions SET api_url='https://other.example' WHERE version_id=$1"
        )
        .bind(first.version_id)
        .execute(db.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM oxsum.merchant_versions WHERE version_id=$1")
            .bind(first.version_id)
            .execute(db.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE oxsum.merchant_accounts SET account_id='different' WHERE merchant_id=$1"
        )
        .bind(first.merchant_id)
        .execute(db.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query("UPDATE oxsum.merchant_mutation_results SET revision=10 WHERE mutation_id=$1")
            .bind(first.id)
            .execute(db.pool())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn test_live_identity_and_replay_conflicts_are_separate() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let mut request = create();
    let first = db
        .create_merchant(&owner, &request, &secret())
        .await
        .unwrap();
    request.config.webhook_secret = "changed-signing-secret".into();
    assert!(matches!(
        db.create_merchant(&owner, &request, &secret()).await,
        Err(WalletError::Conflict(_))
    ));
    request.idempotency_key = Uuid::new_v4().to_string();
    request.name = format!("merchant-{}", Uuid::new_v4().simple());
    assert!(matches!(
        db.create_merchant(&owner, &request, &secret()).await,
        Err(WalletError::Conflict(_))
    ));
    request.mode = MerchantMode::Live;
    assert!(matches!(
        db.create_merchant(&owner, &request, &secret()).await,
        Err(WalletError::InvalidInput(_))
    ));
    request.config.api_key = "live_merchant_secret".into();
    request.config.publishable_key = Some("pk_live_merchant".into());
    let live = db
        .create_merchant(&owner, &request, &secret())
        .await
        .unwrap();
    assert_ne!(first.merchant_id, live.merchant_id);
    let state = MerchantStatusRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "enable live fixture".into(),
        enabled: true,
    };
    let original = db
        .set_merchant_enabled(&owner, live.merchant_id, &state)
        .await
        .unwrap();
    db.set_merchant_enabled(
        &owner,
        live.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "disable live fixture".into(),
            enabled: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        db.set_merchant_enabled(&owner, live.merchant_id, &state)
            .await
            .unwrap(),
        original
    );
    let changed = MerchantStatusRequest {
        enabled: false,
        ..state
    };
    assert!(matches!(
        db.set_merchant_enabled(&owner, live.merchant_id, &changed)
            .await,
        Err(WalletError::Conflict(_))
    ));
    // Current enabled state never rewrites historical callback identity.
    assert!(
        !db.merchant_signing_context(live.merchant_id, live.version_id, &secret())
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    assert!(
        !db.merchant_signing_context(first.merchant_id, first.version_id, &secret())
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
}
#[tokio::test]
async fn invalid_configuration_is_atomic_and_only_rail_fields_are_accepted() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    for endpoint in [
        "http://public.example",
        "https://user:pass@public.example",
        "https://127.0.0.1",
        "https://example.com?token=secret",
        "https://169.254.169.254",
    ] {
        let mut request = create();
        request.config.api_url = endpoint.into();
        assert!(matches!(
            db.create_merchant(&owner, &request, &secret()).await,
            Err(WalletError::InvalidInput(_))
        ));
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM oxsum.merchant_accounts WHERE name=$1")
                .bind(&request.name)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(count, 0);
    }
    let mut request = create();
    request.config.client_id = Some("not-valid-for-hyperswitch".into());
    assert!(
        db.create_merchant(&owner, &request, &secret())
            .await
            .is_err()
    );
    request.rail = MerchantRail::Airwallex;
    request.config.publishable_key = None;
    request.config.sdk_url = None;
    request.config.checkout_url = Some("https://checkout.airwallex.com".into());
    let airwallex = db
        .create_merchant(&owner, &request, &secret())
        .await
        .unwrap();
    assert_eq!(
        db.merchant_signing_context(airwallex.merchant_id, airwallex.version_id, &secret())
            .await
            .unwrap()
            .unwrap()
            .client_id(),
        Some("not-valid-for-hyperswitch")
    );
    let mut request = create();
    request.rail = MerchantRail::Stripe;
    request.config.publishable_key = None;
    request.config.sdk_url = None;
    request.config.api_key = "sk_test_stripe_merchant".into();
    let stripe = db
        .create_merchant(&owner, &request, &secret())
        .await
        .unwrap();
    assert_eq!(
        db.merchant_signing_context(stripe.merchant_id, stripe.version_id, &secret())
            .await
            .unwrap()
            .unwrap()
            .rail,
        MerchantRail::Stripe
    );
    assert!(matches!(
        db.business_merchant_versions(&owner, Uuid::new_v4(), None, 20)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(db.business_merchants(&owner, None, 0).await.is_err());
}
