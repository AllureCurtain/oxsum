//! A06/A08: one customer/key snapshot controls catalogs and credential opening.
use oxsum_core::{
    Db, KeyConstraints, KeyScope, NewUser, Price, PriceBook, Registration, SecretKey, Variant,
    WalletError,
};
use uuid::Uuid;

async fn fixture() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("serving_access")?;
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
async fn customer(db: &Db) -> Registration {
    db.register(NewUser {
        email: format!("{}@example.com", Uuid::new_v4()),
        password: "serving access fixture password".into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
fn price() -> Price {
    PriceBook::from_json("fixture", r#"{"shared":{"inputPricePerMillion":1000000,"outputPricePerMillion":1000000,"maxOutputTokens":30}}"#)
        .unwrap().models().next().unwrap().1.clone()
}

#[tokio::test]
async fn catalog_alias_and_credentials_share_the_customer_key_channel_intersection() {
    let Some(db) = fixture().await else {
        return;
    };
    let a = customer(&db).await;
    let b = customer(&db).await;
    let secret = SecretKey::from_bytes([1; 32]);
    for (name, sealing) in [
        ("allowed", secret.clone()),
        ("excluded", SecretKey::from_bytes([2; 32])),
    ] {
        db.set_channel(
            name,
            "https://provider.example/v1",
            "provider-fixture-secret",
            "openai",
            &sealing,
        )
        .await
        .unwrap();
        db.append_price(name, "shared", price(), 100).await.unwrap();
    }
    db.append_price("allowed", "private", price(), 100)
        .await
        .unwrap();
    db.create_alias("allowed", "public", "shared")
        .await
        .unwrap();
    let allowed = db.channel_id_named("allowed").await.unwrap().unwrap();
    let group = db
        .put_group(a.organization.id, "selected", &[allowed])
        .await
        .unwrap();
    let key = db
        .create_key(
            a.organization.id,
            None,
            None,
            None,
            KeyConstraints {
                model_allowlist: Some(vec!["shared".into(), "public".into(), "private".into()]),
                group_id: Some(Some(group.id)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.set_tier("basic", None, Some(vec!["shared".into(), "public".into()]))
        .await
        .unwrap();
    db.set_organization_tier(a.organization.id, Some("basic"))
        .await
        .unwrap();
    assert!(
        db.put_experiment(
            a.organization.id,
            "hidden-variant",
            &[Variant {
                model: "private".into(),
                weight: 1
            }]
        )
        .await
        .is_err()
    );
    assert!(
        db.put_experiment(
            a.organization.id,
            "allowed-variant",
            &[Variant {
                model: "public".into(),
                weight: 1
            }]
        )
        .await
        .is_ok()
    );
    let snapshot = db
        .serving_access(a.organization.id, Some(key.key.id))
        .await
        .unwrap();
    let catalog = db.catalog_for_access(&snapshot).await.unwrap();
    assert_eq!(
        catalog.iter().map(|r| r.model.as_str()).collect::<Vec<_>>(),
        vec!["public", "shared"]
    );
    assert!(catalog.iter().all(|r| r.channel == "allowed"));
    let (routes, target) = db
        .servings_named_for_access("public", &secret, &snapshot)
        .await
        .unwrap();
    assert_eq!(target.as_deref(), Some("shared"));
    assert_eq!(routes.len(), 1);
    // The other channel's secret cannot open with this key, but an excluded
    // channel never gets opened while resolving an otherwise shared model.
    assert_eq!(
        db.servings_named_for_access("shared", &secret, &snapshot)
            .await
            .unwrap()
            .0
            .len(),
        1
    );
    let session_catalog = db
        .catalog_for_access(&db.serving_access(a.organization.id, None).await.unwrap())
        .await
        .unwrap();
    assert_eq!(session_catalog.len(), 3);
    let other_catalog = db
        .catalog_for_access(&db.serving_access(b.organization.id, None).await.unwrap())
        .await
        .unwrap();
    assert_eq!(other_catalog.len(), 4);
    db.update_key_constraints(
        a.organization.id,
        key.key.id,
        KeyScope::Organization,
        KeyConstraints {
            model_allowlist: Some(vec!["public".into()]),
            group_id: Some(Some(group.id)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let tightened = db
        .serving_access(a.organization.id, Some(key.key.id))
        .await
        .unwrap();
    assert!(db.catalog_for_access(&tightened).await.unwrap().is_empty());
    assert!(
        db.servings_named_for_access("public", &secret, &tightened)
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(
        db.catalog_for_access(&snapshot).await.unwrap().len(),
        2,
        "admitted authority is stable"
    );
    db.put_group(a.organization.id, "selected", &[])
        .await
        .unwrap();
    assert!(
        db.catalog_for_access(
            &db.serving_access(a.organization.id, Some(key.key.id))
                .await
                .unwrap()
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn revoked_foreign_expired_and_missing_keys_fail_closed() {
    let Some(db) = fixture().await else {
        return;
    };
    let a = customer(&db).await;
    let b = customer(&db).await;
    for id in [Uuid::new_v4(), b.api_key.key.id] {
        assert!(matches!(
            db.serving_access(a.organization.id, Some(id)).await,
            Err(WalletError::Unauthenticated)
        ));
    }
    db.revoke_key(a.organization.id, a.api_key.key.id, KeyScope::Organization)
        .await
        .unwrap();
    assert!(matches!(
        db.serving_access(a.organization.id, Some(a.api_key.key.id))
            .await,
        Err(WalletError::Unauthenticated)
    ));
    sqlx::query("UPDATE oxsum.api_keys SET expires_at=now()-interval '1 second' WHERE key_id=$1")
        .bind(b.api_key.key.id)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.serving_access(b.organization.id, Some(b.api_key.key.id))
            .await,
        Err(WalletError::Unauthenticated)
    ));
    assert!(matches!(
        db.serving_access(Uuid::new_v4(), None).await,
        Err(WalletError::Unauthenticated)
    ));
}
