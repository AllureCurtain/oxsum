//! T05/A01/A06: credential authority, sparse evidence pagination and stale sessions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    CustomerCapability as C, Db, KeyConstraints, KeyPrincipal, KeyScope, NewUser, Principal,
    SecretKey, Tenants, WalletError,
};
use time::OffsetDateTime;
use uuid::Uuid;

const PASSWORD: &str = "customer authority test password";

async fn fixture() -> Option<(Db, oxsum_core::Registration, oxsum_core::CreatedSession)> {
    let url = oxsum_testkit::suite_db_url("customer_authority")?;
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let reg = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let session = db
        .login(&reg.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session();
    Some((db, reg, session))
}

#[tokio::test]
async fn consumption_keys_cannot_manage_credentials_or_claim_personal_authority() {
    let Some((db, reg, session)) = fixture().await else {
        return;
    };
    let (organization, key) = db.authenticate(&reg.api_key.secret).await.unwrap().unwrap();
    let principal = Principal::Key(KeyPrincipal { organization, key });
    assert!(principal.require_customer(C::ScopedRead).is_ok());
    for capability in [C::Personal, C::AccountManage, C::Retired] {
        assert!(matches!(
            principal.require_customer(capability),
            Err(WalletError::Forbidden(_))
        ));
    }
    let owner = Principal::Session(session.principal.unwrap());
    let sibling = db
        .create_key_as(
            &owner,
            Some("sibling".into()),
            None,
            KeyConstraints::default(),
        )
        .await
        .unwrap();
    let before = db
        .list_keys(reg.organization.id, KeyScope::All)
        .await
        .unwrap();
    assert!(matches!(
        db.create_key_as(&principal, None, None, KeyConstraints::default())
            .await,
        Err(WalletError::Forbidden(_))
    ));
    for id in [reg.api_key.key.id, sibling.key.id] {
        assert!(matches!(
            db.update_key_constraints_as(&principal, id, KeyConstraints::default())
                .await,
            Err(WalletError::Forbidden(_))
        ));
        assert!(matches!(
            db.revoke_key_as(&principal, id).await,
            Err(WalletError::Forbidden(_))
        ));
        assert!(matches!(
            db.create_mandate_as(
                &principal,
                id,
                10,
                None,
                None,
                None,
                OffsetDateTime::now_utc() + time::Duration::hours(1),
                &SecretKey::from_bytes([5; 32])
            )
            .await,
            Err(WalletError::Forbidden(_))
        ));
    }
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(
            db.list_keys(reg.organization.id, KeyScope::All)
                .await
                .unwrap()
        )
        .unwrap()
    );
    let own = db
        .list_keys(reg.organization.id, principal.key_scope())
        .await
        .unwrap();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].id, reg.api_key.key.id);
}

#[tokio::test]
async fn evidence_scope_survives_sparse_pages_and_denies_sibling_and_unattributed_proofs() {
    let Some((db, reg, session)) = fixture().await else {
        return;
    };
    let owner = Principal::Session(session.principal.unwrap());
    let (organization, key) = db.authenticate(&reg.api_key.secret).await.unwrap().unwrap();
    let key_principal = Principal::Key(KeyPrincipal {
        organization,
        key: key.clone(),
    });
    let wallet = Tenants::new(db.pool().clone())
        .get(&reg.organization.tenant_id)
        .await
        .unwrap();
    let day = OffsetDateTime::now_utc().date();
    let funding = wallet.top_up("fund", 10000, day).await.unwrap();
    let own = wallet
        .hold_for_key(&key, None, "own", "private", 5, day, None)
        .await
        .unwrap();
    for n in 0..105 {
        wallet.top_up(&format!("gap-{n}"), 1, day).await.unwrap();
    }
    let latest = wallet
        .hold_for_key(&key, None, "latest", "private", 5, day, None)
        .await
        .unwrap();
    let sibling = db
        .create_key_as(&owner, None, None, KeyConstraints::default())
        .await
        .unwrap();
    let (_, other) = db.authenticate(&sibling.secret).await.unwrap().unwrap();
    let foreign = wallet
        .hold_for_key(&other, None, "other", "other-private", 5, day, None)
        .await
        .unwrap();
    let scope = db.customer_record_scope(&key_principal).await.unwrap();
    let first = wallet.scoped_entries_page(None, 1, &scope).await.unwrap();
    assert_eq!(first.rows[0].id, latest.entry_id.to_string());
    let second = wallet
        .scoped_entries_page(first.next_cursor, 1, &scope)
        .await
        .unwrap();
    assert_eq!(second.rows[0].id, own.entry_id.to_string());
    assert!(
        wallet
            .scoped_entries_page(second.next_cursor, 1, &scope)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        wallet
            .scoped_receipt_proof(own.entry_id, None, &scope)
            .await
            .unwrap()
            .is_some()
    );
    for entry in [foreign.entry_id, funding.entry_id] {
        assert!(
            wallet
                .scoped_receipt_proof(entry, None, &scope)
                .await
                .unwrap()
                .is_none()
        );
    }
    let owner_scope = db.customer_record_scope(&owner).await.unwrap();
    assert!(
        wallet
            .scoped_receipt_proof(foreign.entry_id, None, &owner_scope)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        wallet
            .entitlement_read(
                &reg.organization.id,
                &sibling.key.id,
                key_principal.key_scope(),
                None,
                None,
                &Default::default(),
                None
            )
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn demotion_removal_and_logout_invalidate_a_captured_key_manager() {
    let Some((db, reg, session)) = fixture().await else {
        return;
    };
    let owner = Principal::Session(session.principal.unwrap());
    let id = reg.api_key.key.id;
    sqlx::query(
        "UPDATE oxsum.memberships SET role='member' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(reg.organization.id)
    .bind(reg.user.id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        db.update_key_constraints_as(&owner, id, KeyConstraints::default())
            .await
            .is_err()
    );
    assert!(db.revoke_key_as(&owner, id).await.is_err());
    assert!(
        db.create_mandate_as(
            &owner,
            id,
            10,
            None,
            None,
            None,
            OffsetDateTime::now_utc() + time::Duration::hours(1),
            &SecretKey::from_bytes([5; 32])
        )
        .await
        .is_err()
    );
    let member = Principal::Session(
        db.authenticate_session(&session.token)
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(
        db.create_key_as(&member, None, None, KeyConstraints::default())
            .await
            .is_ok()
    );
    db.logout(&session.token).await.unwrap();
    assert!(
        db.create_key_as(&member, None, None, KeyConstraints::default())
            .await
            .is_err()
    );
    assert!(
        db.authenticate(&reg.api_key.secret)
            .await
            .unwrap()
            .is_some()
    );
    sqlx::query("DELETE FROM oxsum.memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(reg.organization.id)
        .bind(reg.user.id)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db.revoke_key_as(&owner, id).await.is_err());
}
