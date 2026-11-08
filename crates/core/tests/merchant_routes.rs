//! Checkout selection pins original merchant identity across route/credential changes.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;

use oxsum_core::{
    CheckoutChoice, CheckoutRouteRequest, CollectionCurrency, Db, MerchantCreateRequest,
    MerchantMode, MerchantRail, MerchantStatusRequest, MerchantVersionRequest, NewPaymentOrder,
    NewUser, PaymentOrderPurpose, SecretKey, WalletError,
};
use serde_json::json;
use uuid::Uuid;

async fn merchant(db: &Db, owner: &str, secret: &SecretKey) -> oxsum_core::MerchantMutationResult {
    let created = db.create_merchant(owner, &MerchantCreateRequest {
        idempotency_key:Uuid::new_v4().to_string(),reason:"checkout fixture".into(),name:format!("merchant-{}",Uuid::new_v4().simple()),
        rail:MerchantRail::Hyperswitch,mode:MerchantMode::Test,account_id:Uuid::new_v4().to_string(),
        config:serde_json::from_value(json!({"apiUrl":"https://api.example.com","apiKey":"snd_fixture_key","webhookSecret":"fixture-signing-secret","publishableKey":"pk_snd_fixture","sdkUrl":"https://sdk.example.com/v1.js"})).unwrap(),
    },secret).await.unwrap();
    db.set_merchant_enabled(
        owner,
        created.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "enable fixture".into(),
            enabled: true,
        },
    )
    .await
    .unwrap()
}
fn request(merchant: Option<Uuid>, revision: i64) -> CheckoutRouteRequest {
    CheckoutRouteRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "choose fixture".into(),
        rail: MerchantRail::Hyperswitch,
        expected_revision: revision,
        merchant_id: merchant,
    }
}

#[tokio::test]
async fn explicit_checkout_selection_retains_replay_and_never_falls_back() {
    let Some(url) = oxsum_testkit::suite_db_url("merchant_routes") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    db.bootstrap_collection_terms(&CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let organization = customer.organization.id;
    let secret = SecretKey::from_bytes([38; 32]);
    let first = merchant(&db, &owner, &secret).await;
    let second = merchant(&db, &owner, &secret).await;
    assert!(
        db.customer_checkout_options(organization)
            .await
            .unwrap()
            .is_empty()
    );
    let select = request(Some(first.merchant_id), 0);
    let selected = db.set_checkout_route(&owner, &select).await.unwrap();
    assert_eq!(selected.route.revision, 1);
    let choice = CheckoutChoice {
        revision: 1,
        merchant_id: first.merchant_id,
        version_id: first.version_id,
    };
    let new = NewPaymentOrder {
        expected_collection_revision: None,
        expected_plan_quote: None,
        purpose: PaymentOrderPurpose::WalletTopup,
        display_amount_minor: 200,
        display_currency: "USD".into(),
        purpose_ref: None,
        idempotency_key: Uuid::new_v4().to_string(),
        promo_minor: 0,
    };
    let (original, context) = db
        .create_routed_payment_order(
            organization,
            MerchantRail::Hyperswitch,
            Some(&choice),
            &secret,
            &new,
        )
        .await
        .unwrap();
    assert_eq!(original.merchant_id, Some(first.merchant_id));
    assert_eq!(
        context.version.sdk_url.as_deref(),
        Some("https://sdk.example.com/v1.js")
    );
    let version:oxsum_core::MerchantVersionInput=serde_json::from_value(json!({"apiUrl":"https://api.example.com","apiKey":"snd_rotated_key","webhookSecret":"new-fixture-signing","publishableKey":"pk_snd_rotated","sdkUrl":"https://sdk.example.com/v2.js"})).unwrap();
    db.rotate_merchant(
        &owner,
        first.merchant_id,
        &MerchantVersionRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotate fixture".into(),
            config: version,
        },
        &secret,
    )
    .await
    .unwrap();
    let fresh = NewPaymentOrder {
        idempotency_key: Uuid::new_v4().to_string(),
        ..new.clone()
    };
    assert!(matches!(
        db.create_routed_payment_order(
            organization,
            MerchantRail::Hyperswitch,
            Some(&choice),
            &secret,
            &fresh
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    db.set_checkout_route(&owner, &request(Some(second.merchant_id), 1))
        .await
        .unwrap();
    db.set_merchant_enabled(
        &owner,
        first.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "disable original".into(),
            enabled: false,
        },
    )
    .await
    .unwrap();
    let (replayed, pinned) = db
        .create_routed_payment_order(
            organization,
            MerchantRail::Hyperswitch,
            Some(&choice),
            &secret,
            &new,
        )
        .await
        .unwrap();
    assert_eq!(replayed.order_id, original.order_id);
    assert_eq!(pinned.version.id, first.version_id);
    assert_eq!(
        pinned.version.sdk_url.as_deref(),
        Some("https://sdk.example.com/v1.js")
    );
    assert!(matches!(
        db.create_routed_payment_order(organization, MerchantRail::Stripe, None, &secret, &new)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let receipt = db.set_checkout_route(&owner, &select).await.unwrap();
    assert_eq!(receipt.id, selected.id);
    assert_eq!(receipt.route.revision, 1);
    assert!(matches!(
        db.set_checkout_route(&owner, &request(Some(second.merchant_id), 0))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let (next, _) = db
        .create_routed_payment_order(
            organization,
            MerchantRail::Hyperswitch,
            None,
            &secret,
            &fresh,
        )
        .await
        .unwrap();
    assert_eq!(next.merchant_id, Some(second.merchant_id));
    db.set_checkout_route(&owner, &request(None, 2))
        .await
        .unwrap();
    assert!(
        db.customer_checkout_options(organization)
            .await
            .unwrap()
            .is_empty()
    );
    let fresh = NewPaymentOrder {
        idempotency_key: Uuid::new_v4().to_string(),
        ..fresh
    };
    assert!(matches!(
        db.create_routed_payment_order(
            organization,
            MerchantRail::Hyperswitch,
            None,
            &secret,
            &fresh
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.customer_checkout_options(Uuid::new_v4()).await,
        Err(WalletError::NotFound(_))
    ));
    // Eight concurrent identical creates after re-enabling the explicit route
    // all return one pinned intent and make no provider calls at this layer.
    db.set_checkout_route(&owner, &request(Some(second.merchant_id), 3))
        .await
        .unwrap();
    let concurrent = NewPaymentOrder {
        idempotency_key: Uuid::new_v4().to_string(),
        ..fresh.clone()
    };
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let db = db.clone();
        let new = concurrent.clone();
        tasks.push(tokio::spawn(async move {
            db.create_routed_payment_order(
                organization,
                MerchantRail::Hyperswitch,
                None,
                &SecretKey::from_bytes([38; 32]),
                &new,
            )
            .await
            .unwrap()
            .0
        }));
    }
    let first_winner = tasks.remove(0).await.unwrap();
    for task in tasks {
        let winner = task.await.unwrap();
        assert_eq!(winner.order_id, first_winner.order_id);
        assert_eq!(winner.merchant_version_id, first_winner.merchant_version_id);
    }
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.payment_orders WHERE organization_id=$1")
            .bind(organization)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rows, 3);
}
