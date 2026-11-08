//! Solo business setup, time/configuration regressions and current-role projection.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;

use std::collections::BTreeMap;

use oxsum_core::{
    BusinessReadiness, BusinessRole, BusinessStaffRequest, BusinessStaffRevokeRequest,
    CheckoutRouteRequest, CollectionCurrency, Db, MerchantCreateRequest, MerchantMode,
    MerchantRail, MerchantStatusRequest, NewUser, ProviderAliasInput, ProviderChange,
    ProviderConnection, ProviderPriceInput, ProviderStatusInput, SaleChange, SaleChangeRequest,
    SecretKey, SessionContext, SessionContextRequest, WalletError,
};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

fn state<'a>(readiness: &'a BusinessReadiness, key: &str) -> &'a str {
    readiness
        .steps
        .iter()
        .find(|step| step.key == key)
        .unwrap()
        .state
}

async fn provider(db: &Db, owner: &str, secret: &SecretKey, change: ProviderChange) {
    db.change_business_provider(
        owner,
        &Uuid::new_v4().to_string(),
        "readiness fixture",
        &change,
        secret,
    )
    .await
    .unwrap();
}

async fn sale(db: &Db, owner: &str, change: SaleChange) -> Uuid {
    db.change_sale_terms(
        owner,
        &SaleChangeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "readiness fixture".into(),
            change,
        },
    )
    .await
    .unwrap()
    .resource_id
}

async fn book(db: &Db, owner: &str, model: &str) -> Uuid {
    let book = sale(
        db,
        owner,
        SaleChange::CreateBook {
            name: Uuid::new_v4().to_string(),
        },
    )
    .await;
    sale(db, owner, SaleChange::SaveDraft {
        book_id: book, expected_revision: 0,
        models: BTreeMap::from([(model.into(), serde_json::from_value(json!({
            "price":{"inputPricePerMillion":100,"outputPricePerMillion":200,"maxOutputTokens":32},
            "discountPercent":0
        })).unwrap())]),
    }).await;
    sale(
        db,
        owner,
        SaleChange::PublishDraft {
            book_id: book,
            draft_revision: 1,
        },
    )
    .await;
    book
}

async fn select(db: &Db, owner: &str, revision: i64, merchant: Option<Uuid>) {
    db.set_checkout_route(
        owner,
        &CheckoutRouteRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "readiness fixture".into(),
            rail: MerchantRail::Hyperswitch,
            expected_revision: revision,
            merchant_id: merchant,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn solo_payg_uses_matching_current_configuration_without_staff_or_plans() {
    let Some(url) = oxsum_testkit::suite_db_url("business_readiness") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let secret = SecretKey::from_bytes([73; 32]);
    let first = db
        .business_readiness(&owner, Some(&secret), true)
        .await
        .unwrap();
    assert!(!first.ready);
    assert_eq!(state(&first, "profile"), "ready");
    assert_eq!(state(&first, "admission"), "ready");
    assert_eq!(state(&first, "supply"), "todo");
    assert_eq!(state(&first, "saleTerms"), "todo");
    assert_eq!(state(&first, "collection"), "todo");

    provider(
        &db,
        &owner,
        &secret,
        ProviderChange::Connection {
            config: ProviderConnection {
                name: "first-chat".into(),
                base_url: "https://provider.example.com".into(),
                protocol: "openai".into(),
                api_keys: Some(vec!["fixture-provider-secret".into()]),
                destination_policy: None,
            },
        },
    )
    .await;
    provider(&db, &owner, &secret, ProviderChange::Price { channel:"first-chat".into(),config:Box::new(ProviderPriceInput {
        model:"first-model".into(),price:serde_json::from_value(json!({"inputPricePerMillion":100,"outputPricePerMillion":200,"maxOutputTokens":32})).unwrap(),weight:1,
    })}).await;
    let supply = db
        .business_readiness(&owner, Some(&secret), true)
        .await
        .unwrap();
    assert_eq!(state(&supply, "supply"), "ready");
    assert_eq!(state(&supply, "saleTerms"), "todo");
    let unrelated = book(&db, &owner, "unserved-model").await;
    let schedule = sale(
        &db,
        &owner,
        SaleChange::ScheduleDefault {
            book_id: unrelated,
            version: 1,
            effective_from: None,
            effective_until: None,
            replaces: None,
        },
    )
    .await;
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "saleTerms"
        ),
        "todo"
    );
    let matched = book(&db, &owner, "first-model").await;
    // Publication alone does not activate a sale book.
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "saleTerms"
        ),
        "todo"
    );
    let active = sale(
        &db,
        &owner,
        SaleChange::ScheduleDefault {
            book_id: matched,
            version: 1,
            effective_from: None,
            effective_until: None,
            replaces: Some(schedule),
        },
    )
    .await;
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "saleTerms"
        ),
        "ready"
    );

    db.bootstrap_collection_terms(&CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let merchant=db.create_merchant(&owner,&MerchantCreateRequest {
        idempotency_key:Uuid::new_v4().to_string(),reason:"readiness fixture".into(),name:"first-checkout".into(),
        rail:MerchantRail::Hyperswitch,mode:MerchantMode::Test,account_id:"fixture-account".into(),
        config:serde_json::from_value(json!({"apiUrl":"https://payments.example.com","apiKey":"snd_readiness_fixture","webhookSecret":"fixture-webhook-secret","publishableKey":"pk_snd_readiness","sdkUrl":"https://sdk.example.com/v1.js"})).unwrap(),
    },&secret).await.unwrap();
    db.set_merchant_enabled(
        &owner,
        merchant.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "enable fixture".into(),
            enabled: true,
        },
    )
    .await
    .unwrap();
    // Merely storing/enabling a merchant must not imply route selection.
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "collection"
        ),
        "todo"
    );
    select(&db, &owner, 0, Some(merchant.merchant_id)).await;
    let complete = db
        .business_readiness(&owner, Some(&secret), true)
        .await
        .unwrap();
    assert!(complete.ready, "{complete:?}");
    assert_eq!(db.business_staff(&owner).await.unwrap().len(), 1);
    assert!(
        db.sold_plans(db.default_business().await.unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    let serialized = json!(complete).to_string();
    for forbidden in [
        "fixture-provider-secret",
        "payments.example.com",
        "fixture-webhook-secret",
        "fixture-account",
        "first-model",
        "first-checkout",
    ] {
        assert!(!serialized.contains(forbidden));
    }
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), false)
                .await
                .unwrap(),
            "collection"
        ),
        "todo"
    );
    let no_key = db.business_readiness(&owner, None, true).await.unwrap();
    assert_eq!(state(&no_key, "supply"), "todo");
    assert_eq!(state(&no_key, "collection"), "todo");
    let wrong_key = SecretKey::from_bytes([74; 32]);
    let unreadable = db
        .business_readiness(&owner, Some(&wrong_key), true)
        .await
        .unwrap();
    assert_eq!(state(&unreadable, "supply"), "todo");
    assert_eq!(state(&unreadable, "collection"), "todo");
    select(&db, &owner, 1, None).await;
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "collection"
        ),
        "todo"
    );
    select(&db, &owner, 2, Some(merchant.merchant_id)).await;
    db.set_merchant_enabled(
        &owner,
        merchant.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "disable fixture".into(),
            enabled: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "collection"
        ),
        "todo"
    );
    db.set_merchant_enabled(
        &owner,
        merchant.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "restore fixture".into(),
            enabled: true,
        },
    )
    .await
    .unwrap();

    provider(
        &db,
        &owner,
        &secret,
        ProviderChange::Connection {
            config: ProviderConnection {
                name: "alternate-chat".into(),
                base_url: "https://alternate.example.com".into(),
                protocol: "openai".into(),
                api_keys: Some(vec!["alternate-fixture-secret".into()]),
                destination_policy: None,
            },
        },
    )
    .await;
    provider(&db, &owner, &secret, ProviderChange::Price { channel: "alternate-chat".into(), config: Box::new(ProviderPriceInput {
        model: "first-model".into(), price: serde_json::from_value(json!({"inputPricePerMillion":100,"outputPricePerMillion":200,"maxOutputTokens":32})).unwrap(), weight: 1,
    })}).await;
    let alternate = db
        .business_channels(&owner, None, 10)
        .await
        .unwrap()
        .channels
        .into_iter()
        .find(|channel| channel.name == "alternate-chat")
        .unwrap();
    let wrong_seal = wrong_key.seal("alternate-fixture-secret").unwrap();
    sqlx::query("UPDATE oxsum.channel_credentials SET api_key_sealed=$2 WHERE credential_id=$1")
        .bind(alternate.credentials[0].credential_id)
        .bind(wrong_seal)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "supply"
        ),
        "todo",
        "an unreadable candidate poisons admission before route selection"
    );
    provider(
        &db,
        &owner,
        &secret,
        ProviderChange::CredentialStatus {
            channel: "alternate-chat".into(),
            credential: alternate.credentials[0].credential_id,
            config: ProviderStatusInput {
                status: "disabled".into(),
            },
        },
    )
    .await;
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "supply"
        ),
        "ready"
    );

    let channel = db
        .business_channels(&owner, None, 10)
        .await
        .unwrap()
        .channels
        .into_iter()
        .find(|channel| channel.name == "first-chat")
        .unwrap();
    let credential = channel.credentials[0].credential_id;
    provider(
        &db,
        &owner,
        &secret,
        ProviderChange::CredentialStatus {
            channel: "first-chat".into(),
            credential,
            config: ProviderStatusInput {
                status: "disabled".into(),
            },
        },
    )
    .await;
    let disabled = db
        .business_readiness(&owner, Some(&secret), true)
        .await
        .unwrap();
    assert_eq!(state(&disabled, "supply"), "todo");
    assert_eq!(state(&disabled, "saleTerms"), "todo");
    provider(
        &db,
        &owner,
        &secret,
        ProviderChange::CredentialStatus {
            channel: "first-chat".into(),
            credential,
            config: ProviderStatusInput {
                status: "active".into(),
            },
        },
    )
    .await;
    sqlx::query("UPDATE oxsum.channel_credentials SET cooldown_until=now()+interval '1 hour' WHERE credential_id=$1").bind(credential).execute(db.pool()).await.unwrap();
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "supply"
        ),
        "todo"
    );
    sqlx::query("UPDATE oxsum.channel_credentials SET cooldown_until=NULL WHERE credential_id=$1")
        .bind(credential)
        .execute(db.pool())
        .await
        .unwrap();
    // An alias can be the sold name without carrying a separate provider price.
    provider(
        &db,
        &owner,
        &secret,
        ProviderChange::Alias {
            channel: "first-chat".into(),
            config: ProviderAliasInput {
                alias: "sold-alias".into(),
                upstream_model: "first-model".into(),
            },
        },
    )
    .await;
    let alias_book = book(&db, &owner, "sold-alias").await;
    let active = sale(
        &db,
        &owner,
        SaleChange::ScheduleDefault {
            book_id: alias_book,
            version: 1,
            effective_from: None,
            effective_until: None,
            replaces: Some(active),
        },
    )
    .await;
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "saleTerms"
        ),
        "ready"
    );
    provider(
        &db,
        &owner,
        &secret,
        ProviderChange::RemoveAlias {
            channel: "first-chat".into(),
            alias: "sold-alias".into(),
        },
    )
    .await;
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "saleTerms"
        ),
        "todo"
    );
    // A future replacement cannot conceal the fact that no default is active now.
    sale(
        &db,
        &owner,
        SaleChange::EndSchedule {
            schedule_id: active,
            effective_until: OffsetDateTime::now_utc() + Duration::seconds(1),
        },
    )
    .await;
    sale(
        &db,
        &owner,
        SaleChange::ScheduleDefault {
            book_id: matched,
            version: 1,
            effective_from: Some(OffsetDateTime::now_utc() + Duration::days(1)),
            effective_until: None,
            replaces: None,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(
        state(
            &db.business_readiness(&owner, Some(&secret), true)
                .await
                .unwrap(),
            "saleTerms"
        ),
        "todo"
    );

    for role in [
        BusinessRole::Admin,
        BusinessRole::Finance,
        BusinessRole::Support,
        BusinessRole::Viewer,
    ] {
        let user = db
            .register(NewUser {
                email: format!("{}@example.com", Uuid::new_v4()),
                password: "readiness fixture password".into(),
                organization_name: None,
            })
            .await
            .unwrap();
        let token = db
            .login(&user.user.email, "readiness fixture password")
            .await
            .unwrap()
            .into_session()
            .token;
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                user_id: user.user.id,
                role,
                reason: "readiness role".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            db.business_readiness(&token, Some(&secret), true).await,
            Err(WalletError::Forbidden(_))
        ));
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
        let scoped = db
            .business_readiness(&token, Some(&secret), true)
            .await
            .unwrap();
        assert_eq!(
            state(&scoped, "supply"),
            if role == BusinessRole::Admin {
                "ready"
            } else {
                "restricted"
            }
        );
        assert_eq!(
            state(&scoped, "saleTerms"),
            if matches!(role, BusinessRole::Admin | BusinessRole::Finance) {
                "todo"
            } else {
                "restricted"
            }
        );
        assert_eq!(
            state(&scoped, "collection"),
            if role == BusinessRole::Support {
                "restricted"
            } else {
                "ready"
            }
        );
        assert!(!scoped.ready);
        assert!(
            scoped
                .steps
                .iter()
                .filter(|s| matches!(s.key, "profile" | "collection"))
                .all(|s| s.action.is_none())
        );
        db.revoke_business_staff(
            &owner,
            &BusinessStaffRevokeRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                user_id: user.user.id,
                reason: "revoke readiness role".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            db.business_readiness(&token, Some(&secret), true).await,
            Err(WalletError::Forbidden(_))
        ));
    }
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.business_mutations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.business_readiness(&owner, Some(&secret), true)
        .await
        .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.business_mutations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "readiness must not record fake completion mutations"
    );
}
