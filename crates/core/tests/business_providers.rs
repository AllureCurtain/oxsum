//! A01/A07: current authority, transactional configuration, redaction and snapshots.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::*;
use serde_json::json;
use sqlx::{Executor, Row};
use uuid::Uuid;
const PASSWORD: &str = "provider fixture password";
fn secret() -> SecretKey {
    SecretKey::from_bytes([17; 32])
}
async fn fixture() -> Option<(Db, String)> {
    let url = oxsum_testkit::suite_db_url("business_providers")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    Some((db, owner))
}
fn name() -> String {
    format!("p-{}", Uuid::new_v4().simple())
}
fn connection(name: &str, keys: Option<Vec<String>>) -> ProviderChange {
    ProviderChange::Connection {
        config: ProviderConnection {
            destination_policy: None,
            name: name.into(),
            base_url: "https://provider.example/v1".into(),
            protocol: "openai".into(),
            api_keys: keys,
        },
    }
}
fn price(model: &str, rate: i64) -> ProviderPriceInput {
    ProviderPriceInput {
        model: model.into(),
        weight: 100,
        price: serde_json::from_value(
            json!({"inputPricePerMillion":rate,"outputPricePerMillion":rate,"maxOutputTokens":100}),
        )
        .unwrap(),
    }
}
async fn change(db: &Db, token: &str, change: ProviderChange) -> ProviderMutationResult {
    db.change_business_provider(
        token,
        &Uuid::new_v4().to_string(),
        "provider fixture change",
        &change,
        &secret(),
    )
    .await
    .unwrap()
}
async fn staff(db: &Db, owner: &str, role: BusinessRole) -> (String, Uuid) {
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.set_business_staff(
        owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "provider fixture role".into(),
            user_id: user.user.id,
            role,
        },
    )
    .await
    .unwrap();
    let token = db
        .login(&user.user.email, PASSWORD)
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
    (token, user.user.id)
}
#[tokio::test]
async fn current_capabilities_separate_connections_secrets_and_catalog() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let n = name();
    change(
        &db,
        &owner,
        connection(&n, Some(vec!["provider-key-secret".into()])),
    )
    .await;
    let (admin, admin_id) = staff(&db, &owner, BusinessRole::Admin).await;
    assert!(matches!(
        db.change_business_provider(
            &admin,
            "no-secrets",
            "test",
            &connection(&n, None),
            &secret()
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    change(
        &db,
        &admin,
        ProviderChange::Price {
            channel: n.clone(),
            config: Box::new(price("catalog-model", 10)),
        },
    )
    .await;
    change(
        &db,
        &admin,
        ProviderChange::Cache {
            channel: n.clone(),
            config: ProviderCacheInput {
                policy: Some(json!({"enabled":true,"ttlSeconds":30})),
            },
        },
    )
    .await;
    assert!(
        !db.business_channels(&admin, None, 100)
            .await
            .unwrap()
            .channels
            .is_empty()
    );
    for role in [
        BusinessRole::Finance,
        BusinessRole::Support,
        BusinessRole::Viewer,
    ] {
        let (token, _) = staff(&db, &owner, role).await;
        assert!(matches!(
            db.business_channels(&token, None, 100).await,
            Err(WalletError::Forbidden(_))
        ));
        assert!(matches!(
            db.change_business_provider(
                &token,
                "no-catalog",
                "test",
                &ProviderChange::Price {
                    channel: n.clone(),
                    config: Box::new(price("blocked", 1))
                },
                &secret()
            )
            .await,
            Err(WalletError::Forbidden(_))
        ));
    }
    db.revoke_business_staff(
        &owner,
        &BusinessStaffRevokeRequest {
            user_id: admin_id,
            idempotency_key: "revoke-provider-admin".into(),
            reason: "test revocation".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.business_channel_prices(&admin, &n).await,
        Err(WalletError::Forbidden(_))
    ));
    db.switch_session_context(
        &owner,
        &SessionContextRequest {
            idempotency_key: "customer-provider-context".into(),
            context: SessionContext::Customer,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.change_business_provider(
            &owner,
            "wrong-context",
            "test",
            &connection(&n, None),
            &secret()
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
}
#[tokio::test]
async fn encrypted_secret_replay_rotation_and_old_serving_snapshot() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let n = name();
    let key = "provider-secret-never-in-audit";
    let request = connection(&n, Some(vec![key.into()]));
    let receipt = db
        .change_business_provider(&owner, "create", "rotation test", &request, &secret())
        .await
        .unwrap();
    assert_eq!(
        receipt,
        db.change_business_provider(&owner, "create", "rotation test", &request, &secret())
            .await
            .unwrap()
    );
    assert!(matches!(
        db.change_business_provider(
            &owner,
            "create",
            "rotation test",
            &connection(&n, Some(vec!["different-secret".into()])),
            &secret()
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    let row =
        sqlx::query("SELECT api_key_sealed FROM oxsum.channel_credentials WHERE channel_id=$1")
            .bind(receipt.resource_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let sealed: String = row.get("api_key_sealed");
    assert_ne!(sealed, key);
    assert_eq!(secret().open(&sealed).unwrap(), key);
    let model = name();
    let first = change(
        &db,
        &owner,
        ProviderChange::Price {
            channel: n.clone(),
            config: Box::new(price(&model, 10)),
        },
    )
    .await;
    assert_eq!(first.version, Some(1));
    let snapshot = db.servings(&model, &secret()).await.unwrap().pop().unwrap();
    assert_eq!(snapshot.credentials[0].api_key, key);
    assert!(!format!("{snapshot:?}").contains(key));
    change(
        &db,
        &owner,
        ProviderChange::BillKey {
            channel: n.clone(),
            config: ProviderBillKeyInput {
                api_key: Some("billing-secret-only".into()),
            },
        },
    )
    .await;
    let changed = ProviderChange::Connection {
        config: ProviderConnection {
            destination_policy: None,
            name: n.clone(),
            base_url: "https://new-provider.example/v1".into(),
            protocol: "openai".into(),
            api_keys: None,
        },
    };
    assert!(matches!(
        db.change_business_provider(&owner, "no-forward", "test", &changed, &secret())
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    change(
        &db,
        &owner,
        ProviderChange::Connection {
            config: ProviderConnection {
                destination_policy: None,
                name: n.clone(),
                base_url: "https://new-provider.example/v1".into(),
                protocol: "openai".into(),
                api_keys: Some(vec!["new-provider-secret".into()]),
            },
        },
    )
    .await;
    let now = db.servings(&model, &secret()).await.unwrap().pop().unwrap();
    assert_eq!(now.base_url, "https://new-provider.example/v1");
    assert_eq!(snapshot.base_url, "https://provider.example/v1");
    assert_eq!(snapshot.credentials[0].api_key, key);
    change(
        &db,
        &owner,
        ProviderChange::Price {
            channel: n.clone(),
            config: Box::new(price(&model, 20)),
        },
    )
    .await;
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.price.input_price_per_million, 10);
    assert_eq!(
        db.business_channel_prices(&owner, &n).await.unwrap().len(),
        2
    );
    let listed = db.business_channels(&owner, None, 100).await.unwrap();
    let encoded = serde_json::to_string(&listed).unwrap();
    assert!(!encoded.contains(key));
    assert!(!encoded.contains(&sealed));
    assert!(
        listed
            .channels
            .iter()
            .find(|c| c.name == n)
            .unwrap()
            .bill_key_last4
            .is_none()
    );
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT detail::text FROM oxsum.admin_audit WHERE action LIKE 'business.provider.%'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert!(audit.iter().all(|s| !s.contains(key)
        && !s.contains("billing-secret-only")
        && !s.contains("new-provider-secret")));
    assert_eq!(
        receipt,
        db.change_business_provider(&owner, "create", "rotation test", &request, &secret())
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn audit_failure_rolls_back_configuration_pool_price_and_receipt() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let n = name();
    let original = change(
        &db,
        &owner,
        connection(&n, Some(vec!["original-provider-secret".into()])),
    )
    .await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.business_mutations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.pool().execute("CREATE FUNCTION oxsum.reject_provider_audit() RETURNS trigger AS $$ BEGIN IF NEW.action LIKE 'business.provider.%' THEN RAISE EXCEPTION 'injected provider audit failure'; END IF; RETURN NEW; END; $$ LANGUAGE plpgsql; CREATE TRIGGER fail_provider_audit BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_provider_audit()").await.unwrap();
    for operation in [
        connection(&n, Some(vec!["replacement-provider-key".into()])),
        ProviderChange::Price {
            channel: n.clone(),
            config: Box::new(price("fault-model", 10)),
        },
        ProviderChange::AddCredential {
            channel: n.clone(),
            config: ProviderCredentialInput {
                api_key: "extra-provider-key".into(),
            },
        },
        connection(&name(), Some(vec!["new-provider-key".into()])),
    ] {
        assert!(
            db.change_business_provider(
                &owner,
                &Uuid::new_v4().to_string(),
                "audit fault",
                &operation,
                &secret()
            )
            .await
            .is_err()
        );
    }
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.business_mutations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT api_key_sealed FROM oxsum.channel_credentials WHERE channel_id=$1",
    )
    .bind(original.resource_id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(secret().open(&rows[0]).unwrap(), "original-provider-secret");
    assert!(
        db.business_channel_prices(&owner, &n)
            .await
            .unwrap()
            .is_empty()
    );
    db.pool()
        .execute("DROP TRIGGER fail_provider_audit ON oxsum.admin_audit")
        .await
        .unwrap();
    let written = change(
        &db,
        &owner,
        ProviderChange::Price {
            channel: n,
            config: Box::new(price("fault-model", 10)),
        },
    )
    .await;
    assert_eq!(written.version, Some(1));
}
#[tokio::test]
async fn concurrent_replay_and_pool_removal_preserve_one_active_credential() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let n = name();
    let mut calls = Vec::new();
    for _ in 0..6 {
        let db = db.clone();
        let owner = owner.clone();
        let n = n.clone();
        calls.push(tokio::spawn(async move {
            db.change_business_provider(
                &owner,
                "concurrent-create",
                "test",
                &connection(
                    &n,
                    Some(vec![
                        "provider-secret-one".into(),
                        "provider-secret-two".into(),
                    ]),
                ),
                &secret(),
            )
            .await
            .unwrap()
        }));
    }
    let first = calls.remove(0).await.unwrap();
    for call in calls {
        assert_eq!(first, call.await.unwrap());
    }
    let creds = db.channel_credentials(&n).await.unwrap().unwrap();
    assert_eq!(creds.len(), 2);
    let removal = ProviderChange::RemoveCredential {
        channel: n.clone(),
        credential: creds[0].credential_id,
    };
    let encryption = secret();
    let business = db.change_business_provider(&owner, "remove-one", "test", &removal, &encryption);
    let deployment = db.drop_credential(&n, creds[1].credential_id);
    let (a, b) = tokio::join!(business, deployment);
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(db.channel_credentials(&n).await.unwrap().unwrap().len(), 1);
    let other = name();
    change(
        &db,
        &owner,
        connection(&other, Some(vec!["foreign-pool-secret".into()])),
    )
    .await;
    let foreign = db.channel_credentials(&other).await.unwrap().unwrap()[0].credential_id;
    assert!(matches!(
        db.change_business_provider(
            &owner,
            "foreign-key",
            "test",
            &ProviderChange::CredentialStatus {
                channel: n,
                credential: foreign,
                config: ProviderStatusInput {
                    status: "disabled".into()
                }
            },
            &secret()
        )
        .await,
        Err(WalletError::NotFound(_))
    ));
}
#[tokio::test]
async fn aliases_fallbacks_scope_and_unknown_destinations_are_refused() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let n = name();
    let other = name();
    for channel in [&n, &other] {
        change(
            &db,
            &owner,
            connection(channel, Some(vec!["provider-scope-secret".into()])),
        )
        .await;
    }
    let m = name();
    let next = name();
    change(
        &db,
        &owner,
        ProviderChange::Price {
            channel: n.clone(),
            config: Box::new(price(&m, 10)),
        },
    )
    .await;
    change(
        &db,
        &owner,
        ProviderChange::Price {
            channel: other.clone(),
            config: Box::new(price(&next, 10)),
        },
    )
    .await;
    let alias = name();
    change(
        &db,
        &owner,
        ProviderChange::Alias {
            channel: n.clone(),
            config: ProviderAliasInput {
                alias: alias.clone(),
                upstream_model: m.clone(),
            },
        },
    )
    .await;
    assert_eq!(
        db.business_channel_aliases(&owner, &n).await.unwrap()[0].alias,
        alias
    );
    change(
        &db,
        &owner,
        ProviderChange::Fallbacks {
            model: alias.clone(),
            config: ProviderFallbackInput {
                fallbacks: vec![next.clone()],
            },
        },
    )
    .await;
    assert_eq!(
        db.business_model_fallbacks(&owner, &alias).await.unwrap(),
        vec![next]
    );
    assert!(matches!(
        db.change_business_provider(
            &owner,
            "self-fallback",
            "test",
            &ProviderChange::Fallbacks {
                model: alias.clone(),
                config: ProviderFallbackInput {
                    fallbacks: vec![alias.clone()]
                }
            },
            &secret()
        )
        .await,
        Err(WalletError::InvalidInput(_))
    ));
    assert!(matches!(
        db.change_business_provider(
            &owner,
            "wrong-alias",
            "test",
            &ProviderChange::RemoveAlias {
                channel: other,
                alias: alias.clone()
            },
            &secret()
        )
        .await,
        Err(WalletError::NotFound(_))
    ));
    change(
        &db,
        &owner,
        ProviderChange::RemoveAlias {
            channel: n.clone(),
            alias,
        },
    )
    .await;
    assert!(
        db.business_channel_aliases(&owner, &n)
            .await
            .unwrap()
            .is_empty()
    );
    for url in [
        "https://user:private@provider.example/v1",
        "https://provider.example/v1?secret=x",
        "https://provider.example/v1#secret",
        "file:///etc/passwd",
    ] {
        assert!(matches!(
            db.change_business_provider(
                &owner,
                &Uuid::new_v4().to_string(),
                "invalid endpoint",
                &ProviderChange::Connection {
                    config: ProviderConnection {
                        destination_policy: None,
                        name: name(),
                        base_url: url.into(),
                        protocol: "openai".into(),
                        api_keys: Some(vec!["provider-secret".into()])
                    }
                },
                &secret()
            )
            .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    let id = db.channel_id_named(&n).await.unwrap().unwrap();
    assert!(
        sqlx::query("UPDATE oxsum.channels SET business_id=gen_random_uuid() WHERE channel_id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn foreign_configuration_never_appears_or_mutates_through_business_routes() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    // Deliberately create an unsupported second business to exercise the defense
    // below the singleton deployment boundary. Production does not expose this.
    db.pool()
        .execute("ALTER TABLE oxsum.businesses DROP CONSTRAINT businesses_singleton_check")
        .await
        .unwrap();
    let foreign:Uuid=sqlx::query_scalar("INSERT INTO oxsum.businesses(singleton,name) VALUES(false,'foreign fixture') RETURNING business_id").fetch_one(db.pool()).await.unwrap();
    let id = Uuid::new_v4();
    let n = name();
    sqlx::query("INSERT INTO oxsum.channels(channel_id,business_id,name,base_url,protocol) VALUES($1,$2,$3,'https://foreign.example/v1','openai')").bind(id).bind(foreign).bind(&n).execute(db.pool()).await.unwrap();
    let own = name();
    change(
        &db,
        &owner,
        connection(&own, Some(vec!["own-provider-secret".into()])),
    )
    .await;
    let page = db.business_channels(&owner, None, 1).await.unwrap();
    assert_eq!(page.channels.len(), 1);
    assert_eq!(page.channels[0].name, own);
    assert!(page.next_cursor.is_none());
    for op in [
        connection(&n, Some(vec!["attempted-foreign-secret".into()])),
        ProviderChange::Price {
            channel: n.clone(),
            config: Box::new(price("foreign-model", 10)),
        },
        ProviderChange::AddCredential {
            channel: n.clone(),
            config: ProviderCredentialInput {
                api_key: "attempted-foreign-secret".into(),
            },
        },
    ] {
        assert!(matches!(
            db.change_business_provider(
                &owner,
                &Uuid::new_v4().to_string(),
                "foreign denial",
                &op,
                &secret()
            )
            .await,
            Err(WalletError::NotFound(_))
        ));
    }
    assert!(matches!(
        db.business_channel_prices(&owner, &n).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.business_channel_aliases(&owner, &n).await,
        Err(WalletError::NotFound(_))
    ));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.channel_credentials WHERE channel_id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn current_totp_and_owner_transfer_apply_before_replay() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let n = name();
    let request = connection(&n, Some(vec!["owner-transfer-provider-key".into()]));
    db.change_business_provider(&owner, "before-transfer", "test", &request, &secret())
        .await
        .unwrap();
    let (new_owner, target) = staff(&db, &owner, BusinessRole::Admin).await;
    db.transfer_business_ownership(
        &owner,
        &BusinessOwnershipRequest {
            idempotency_key: "provider-owner-transfer".into(),
            user_id: target,
            reason: "provider owner test".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.change_business_provider(&owner, "before-transfer", "test", &request, &secret())
            .await,
        Err(WalletError::Forbidden(_))
    ));
    let seed = totp::generate_secret();
    let encrypted = secret().seal(&totp::base32_encode_pub(&seed)).unwrap();
    db.enroll_totp(target, &encrypted).await.unwrap();
    let code = totp::code(&seed, time::OffsetDateTime::now_utc().unix_timestamp());
    db.confirm_totp(target, &code, &secret()).await.unwrap();
    assert!(matches!(
        db.change_business_provider(&new_owner, "totp-blocked", "test", &request, &secret())
            .await,
        Err(WalletError::Forbidden(_))
    ));
    db.logout(&new_owner).await.unwrap();
    assert!(db.business_channels(&new_owner, None, 1).await.is_err());
}

#[tokio::test]
async fn rotation_never_mixes_endpoint_and_credentials_during_route_resolution() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let n = name();
    let model = name();
    let alias = name();
    let update = |generation| ProviderChange::Connection {
        config: ProviderConnection {
            destination_policy: None,
            name: n.clone(),
            base_url: format!("https://provider-{generation}.example/v1"),
            protocol: "openai".into(),
            api_keys: Some(vec![format!("provider-key-{generation}")]),
        },
    };
    change(&db, &owner, update(0)).await;
    change(
        &db,
        &owner,
        ProviderChange::Price {
            channel: n.clone(),
            config: Box::new(price(&model, 10)),
        },
    )
    .await;
    change(
        &db,
        &owner,
        ProviderChange::Alias {
            channel: n.clone(),
            config: ProviderAliasInput {
                alias: alias.clone(),
                upstream_model: model.clone(),
            },
        },
    )
    .await;
    let write = async {
        for generation in 1..=30 {
            change(&db, &owner, update(generation)).await;
        }
    };
    let read = async {
        for _ in 0..60 {
            for requested in [&model, &alias] {
                let (routes, _) = db.servings_named(requested, &secret()).await.unwrap();
                assert_eq!(routes.len(), 1);
                let route = &routes[0];
                let generation = route
                    .base_url
                    .strip_prefix("https://provider-")
                    .unwrap()
                    .strip_suffix(".example/v1")
                    .unwrap();
                assert_eq!(route.credentials.len(), 1);
                assert_eq!(
                    route.credentials[0].api_key,
                    format!("provider-key-{generation}")
                );
            }
        }
    };
    tokio::join!(write, read);
}

#[tokio::test]
async fn internal_approval_is_owner_only_and_not_carried_to_a_new_endpoint() {
    let Some((db, owner)) = fixture().await else {
        return;
    };
    let channel = name();
    let policy = ProviderDestinationPolicy {
        network: ProviderNetwork::Internal,
        allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
    };
    let mut config = ProviderConnection {
        name: channel.clone(),
        base_url: "http://127.0.0.1:1111/v1".into(),
        protocol: "openai".into(),
        api_keys: Some(vec!["network-provider-secret".into()]),
        destination_policy: Some(policy.clone()),
    };
    let (admin, _) = staff(&db, &owner, BusinessRole::Admin).await;
    assert!(matches!(
        db.change_business_provider(
            &admin,
            "internal-grant",
            "fixture",
            &ProviderChange::Connection {
                config: serde_json::from_value(json!(config)).unwrap()
            },
            &secret()
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    change(
        &db,
        &owner,
        ProviderChange::Connection {
            config: serde_json::from_value(json!(config)).unwrap(),
        },
    )
    .await;
    config.destination_policy = None;
    config.api_keys = None;
    change(
        &db,
        &owner,
        ProviderChange::Connection {
            config: serde_json::from_value(json!(config)).unwrap(),
        },
    )
    .await;
    let settings: serde_json::Value =
        sqlx::query_scalar("SELECT settings FROM oxsum.channels WHERE name=$1")
            .bind(&channel)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        ProviderDestinationPolicy::from_settings(&settings).unwrap(),
        policy
    );
    config.base_url = "http://127.0.0.1:2222/v1".into();
    config.api_keys = Some(vec!["new-destination-secret".into()]);
    assert!(
        db.change_business_provider(
            &owner,
            "change-target",
            "fixture",
            &ProviderChange::Connection {
                config: serde_json::from_value(json!(config)).unwrap()
            },
            &secret()
        )
        .await
        .is_err()
    );
    config.destination_policy = Some(policy);
    change(&db, &owner, ProviderChange::Connection { config }).await;
    db.set_channel(
        &channel,
        "https://public.example/v1",
        "new-public-secret",
        "openai",
        &secret(),
    )
    .await
    .unwrap();
    let settings: serde_json::Value =
        sqlx::query_scalar("SELECT settings FROM oxsum.channels WHERE name=$1")
            .bind(&channel)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        ProviderDestinationPolicy::from_settings(&settings).unwrap(),
        ProviderDestinationPolicy::default()
    );
}
