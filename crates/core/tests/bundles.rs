//! Bundle commercial authority and paid grant conservation over PostgreSQL.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    BundlePublishRequest, BundleRetireRequest, BundleTerms, BusinessRole, BusinessStaffRequest, Db,
    MerchantCreateRequest, MerchantEventKind, MerchantMode, MerchantRail, MerchantSigningContext,
    MerchantStatusRequest, NewPaymentOrder, NewRefundRequest, NewUser, PaymentOrder,
    PaymentOrderPurpose, RefundDecision, SecretKey, SessionContext, SessionContextRequest,
    VerifiedMerchantEvent, Wallet, WalletError,
};
use uuid::Uuid;

fn terms() -> BundleTerms {
    BundleTerms {
        name: "test bundle".into(),
        display_currency: "USD".into(),
        display_amount_minor: 500,
        purchased_minor: 5_000_000,
        promo_minor: 1_000_000,
        promo_lifetime_days: Some(30),
    }
}
fn request() -> BundlePublishRequest {
    BundlePublishRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "isolated bundle fixture".into(),
        bundle_id: None,
        expected_version: 0,
        terms: terms(),
    }
}
async fn world() -> Option<(Db, String, oxsum_core::Registration)> {
    let url = oxsum_testkit::suite_db_url("bundles")?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = register_customer(&db).await;
    Some((db, owner, customer))
}
async fn register_customer(db: &Db) -> oxsum_core::Registration {
    db.register(NewUser {
        email: format!("{}@example.test", Uuid::new_v4()),
        password: "bundle fixture password".into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
async fn create(db: &Db, ctx: &MerchantSigningContext, org: Uuid, id: Uuid) -> PaymentOrder {
    db.create_payment_order(ctx, org, &new_order(id))
        .await
        .unwrap()
}
fn new_order(id: Uuid) -> NewPaymentOrder {
    NewPaymentOrder {
        expected_collection_revision: None,
        purpose: PaymentOrderPurpose::Bundle,
        display_amount_minor: 500,
        display_currency: "USD".into(),
        purpose_ref: Some(id),
        idempotency_key: Uuid::new_v4().to_string(),
        promo_minor: 0,
    }
}
async fn paid(
    db: &Db,
    ctx: &MerchantSigningContext,
    order: &PaymentOrder,
) -> VerifiedMerchantEvent {
    let provider = format!("pay_{}", order.order_id.simple());
    db.note_order_provider(order.order_id, &provider, &serde_json::json!({}))
        .await
        .unwrap();
    let event = VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: MerchantEventKind::Succeeded,
        payment_id: Some(order.order_id),
        provider_payment_id: Some(provider),
        organization_id: Some(order.organization_id),
        provider_account_id: Some(ctx.account_id.clone()),
        mode: Some(MerchantMode::Test),
        amount_minor: Some(order.display_amount_minor),
        currency: Some(order.display_currency.clone()),
        refund_id: None,
    };
    let receipt = db
        .receive_merchant_event(ctx, &event, &[7; 32])
        .await
        .unwrap();
    assert_eq!(receipt.state, "accepted");
    db.apply_order_event(&receipt).await.unwrap();
    event
}
async fn merchant(db: &Db, owner: &str) -> MerchantSigningContext {
    let secret = SecretKey::from_bytes([42; 32]);
    let created=db.create_merchant(owner,&MerchantCreateRequest{idempotency_key:Uuid::new_v4().to_string(),reason:"fixture".into(),name:Uuid::new_v4().to_string(),rail:MerchantRail::Hyperswitch,mode:MerchantMode::Test,account_id:Uuid::new_v4().to_string(),config:serde_json::from_value(serde_json::json!({"apiUrl":"https://sandbox.hyperswitch.io","apiKey":"fake-bundle-key","webhookSecret":"local-secret","publishableKey":"pk_test_bundle","sdkUrl":"https://sdk.example"})).unwrap()},&secret).await.unwrap();
    db.set_merchant_enabled(
        owner,
        created.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "fixture enable".into(),
            enabled: true,
        },
    )
    .await
    .unwrap();
    db.merchant_signing_context(created.merchant_id, created.version_id, &secret)
        .await
        .unwrap()
        .unwrap()
}
#[test]
fn commercial_values_are_bounded_and_exactly_representable() {
    terms().validate().unwrap();
    let mut t = terms();
    t.purchased_minor = 3;
    t.display_amount_minor = 1;
    assert!(t.validate().is_err());
    t = terms();
    t.promo_lifetime_days = Some(0);
    assert!(t.validate().is_err());
    t = terms();
    t.promo_minor = -1;
    assert!(t.validate().is_err());
    t = terms();
    t.purchased_minor = 9_007_199_254_740_991;
    t.promo_minor = 1;
    assert!(t.validate().is_err());
}
#[tokio::test]
async fn published_bundle_pins_grants_retirement_expiry_and_replay() {
    let Some(url) = oxsum_testkit::suite_db_url("bundles") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: "bundle-customer@example.test".into(),
            password: "isolated bundle password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let customer_token = db
        .login(&customer.user.email, "isolated bundle password")
        .await
        .unwrap()
        .into_session()
        .token;
    assert!(matches!(
        db.publish_bundle(&customer_token, &request()).await,
        Err(WalletError::Forbidden(_))
    ));
    let req = request();
    let published = db.publish_bundle(&owner, &req).await.unwrap();
    assert_eq!(
        db.publish_bundle(&owner, &req).await.unwrap().id,
        published.id
    );
    let id = published.offer.bundle_version_id;
    let mut changed = request();
    changed.idempotency_key = req.idempotency_key.clone();
    changed.terms.promo_minor = 2;
    assert!(matches!(
        db.publish_bundle(&owner, &changed).await,
        Err(WalletError::Conflict(_))
    ));
    let ctx = merchant(&db, &owner).await;
    let new = NewPaymentOrder {
        expected_collection_revision: None,
        purpose: PaymentOrderPurpose::Bundle,
        display_amount_minor: 500,
        display_currency: "USD".into(),
        purpose_ref: Some(id),
        idempotency_key: Uuid::new_v4().to_string(),
        promo_minor: 0,
    };
    let mut tampered = new.clone();
    tampered.promo_minor = 9_000_000;
    assert!(matches!(
        db.create_payment_order(&ctx, customer.organization.id, &tampered)
            .await,
        Err(WalletError::Conflict(_))
    ));
    tampered = new.clone();
    tampered.display_amount_minor = 1;
    assert!(
        db.create_payment_order(&ctx, customer.organization.id, &tampered)
            .await
            .is_err()
    );
    let created = db
        .create_payment_order(&ctx, customer.organization.id, &new)
        .await
        .unwrap();
    assert_eq!(created.amount_minor, 5_000_000);
    assert_eq!(created.promo_minor, 1_000_000);
    assert_eq!(created.rate_mantissa, 1);
    assert_eq!(created.rate_scale, 0);
    // Even an old/direct writer cannot insert a forged promotion alongside a
    // valid publication snapshot. This is an oxsum table, not a ledger edit.
    assert!(sqlx::query("INSERT INTO oxsum.payment_orders(order_id,business_id,merchant_id,merchant_version_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,purpose_ref,promo_minor,idempotency_key,bundle_offer) SELECT $2,business_id,merchant_id,merchant_version_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,purpose_ref,promo_minor+1,$2::text,bundle_offer FROM oxsum.payment_orders WHERE order_id=$1")
        .bind(created.order_id).bind(Uuid::new_v4()).execute(db.pool()).await.is_err());
    let frozen = serde_json::to_value(&created).unwrap();
    let retire = BundleRetireRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "no new sales".into(),
    };
    let retired = db.retire_bundle(&owner, id, &retire).await.unwrap();
    assert!(retired.offer.retired_at.is_some());
    assert!(db.bundle_catalog().await.unwrap().is_empty());
    assert_eq!(
        serde_json::to_value(
            db.create_payment_order(&ctx, customer.organization.id, &new)
                .await
                .unwrap()
        )
        .unwrap(),
        frozen
    );
    tampered = new.clone();
    tampered.idempotency_key = Uuid::new_v4().to_string();
    assert!(matches!(
        db.create_payment_order(&ctx, customer.organization.id, &tampered)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let wallet = Wallet::open(db.pool().clone(), &customer.organization.tenant_id)
        .await
        .unwrap();
    let on = time::OffsetDateTime::now_utc().date();
    let event = VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: MerchantEventKind::Succeeded,
        payment_id: Some(created.order_id),
        provider_payment_id: Some("pay_bundle".into()),
        organization_id: Some(customer.organization.id),
        provider_account_id: Some(ctx.account_id.clone()),
        mode: Some(MerchantMode::Test),
        amount_minor: Some(500),
        currency: Some("USD".into()),
        refund_id: None,
    };
    db.note_order_provider(created.order_id, "pay_bundle", &serde_json::json!({}))
        .await
        .unwrap();
    let e = db
        .receive_merchant_event(&ctx, &event, &[7; 32])
        .await
        .unwrap();
    assert_eq!(e.state, "accepted");
    db.apply_order_event(&e).await.unwrap();
    let (a, b) = tokio::join!(
        db.fulfill_payment_order(created.order_id, &wallet, on),
        db.fulfill_payment_order(created.order_id, &wallet, on)
    );
    assert_eq!(a.unwrap().fulfillment_id, b.unwrap().fulfillment_id);
    assert_eq!(wallet.available().await.unwrap(), 6_000_000);
    assert_eq!(
        db.order_refundable_minor(customer.organization.id, created.order_id)
            .await
            .unwrap(),
        Some(5_000_000)
    );
    let order = db
        .payment_order(customer.organization.id, created.order_id)
        .await
        .unwrap()
        .unwrap();
    let expires: time::OffsetDateTime =
        sqlx::query_scalar("SELECT expires_at FROM oxsum.promo_lots WHERE source_ref=$1")
            .bind(created.order_id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(expires, order.paid_at.unwrap() + time::Duration::days(30));
    assert!(
        sqlx::query("UPDATE oxsum.bundle_offers SET terms='{}' WHERE bundle_version_id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE oxsum.payment_orders SET bundle_offer=NULL WHERE order_id=$1")
            .bind(created.order_id)
            .execute(db.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn finance_versions_and_current_authority_control_publication_replays() {
    let Some((db, owner, _)) = world().await else {
        return;
    };
    for role in [
        BusinessRole::Finance,
        BusinessRole::Admin,
        BusinessRole::Viewer,
    ] {
        let person = register_customer(&db).await;
        let token = db
            .login(&person.user.email, "bundle fixture password")
            .await
            .unwrap()
            .into_session()
            .token;
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                user_id: person.user.id,
                role,
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "bundle test role".into(),
            },
        )
        .await
        .unwrap();
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
        assert!(db.business_bundle_offers(&token).await.is_ok());
        let first = request();
        if role != BusinessRole::Finance {
            assert!(matches!(
                db.publish_bundle(&token, &first).await,
                Err(WalletError::Forbidden(_))
            ));
            continue;
        }
        let a = db.publish_bundle(&token, &first).await.unwrap();
        let second = BundlePublishRequest {
            bundle_id: Some(a.offer.bundle_id),
            expected_version: 1,
            ..request()
        };
        let b = db.publish_bundle(&token, &second).await.unwrap();
        assert_eq!(b.offer.version, 2);
        assert_ne!(a.offer.bundle_version_id, b.offer.bundle_version_id);
        let stale = BundlePublishRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            ..second
        };
        assert!(matches!(
            db.publish_bundle(&token, &stale).await,
            Err(WalletError::Conflict(_))
        ));
        let retire = BundleRetireRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "replace price".into(),
        };
        let retired = db
            .retire_bundle(&token, a.offer.bundle_version_id, &retire)
            .await
            .unwrap();
        assert_eq!(
            db.retire_bundle(&token, a.offer.bundle_version_id, &retire)
                .await
                .unwrap()
                .id,
            retired.id
        );
        assert!(
            db.publish_bundle(&token, &first)
                .await
                .unwrap()
                .offer
                .retired_at
                .is_none(),
            "replay preserves the original receipt"
        );
        assert_eq!(db.bundle_catalog().await.unwrap().len(), 1);
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                user_id: person.user.id,
                role: BusinessRole::Viewer,
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "remove commercial authority".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            db.publish_bundle(&token, &first).await,
            Err(WalletError::Forbidden(_))
        ));
        assert!(matches!(
            db.retire_bundle(&token, a.offer.bundle_version_id, &retire)
                .await,
            Err(WalletError::Forbidden(_))
        ));
    }
}

#[tokio::test]
async fn retirement_races_new_orders_without_rewriting_any_admitted_offer() {
    let Some((db, owner, customer)) = world().await else {
        return;
    };
    let ctx = merchant(&db, &owner).await;
    for _ in 0..12 {
        let offer = db.publish_bundle(&owner, &request()).await.unwrap().offer;
        let new = new_order(offer.bundle_version_id);
        let retire = BundleRetireRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "racing new purchase".into(),
        };
        let (order, retired) = tokio::join!(
            db.create_payment_order(&ctx, customer.organization.id, &new),
            db.retire_bundle(&owner, offer.bundle_version_id, &retire)
        );
        retired.unwrap();
        match order {
            Ok(order) => {
                assert_eq!(order.amount_minor, 5_000_000);
                assert_eq!(order.promo_minor, 1_000_000);
                assert_eq!(
                    db.create_payment_order(&ctx, customer.organization.id, &new)
                        .await
                        .unwrap()
                        .order_id,
                    order.order_id
                );
                assert!(order.bundle_offer.unwrap().retired_at.is_none());
            }
            Err(WalletError::NotFound(_)) => {}
            other => panic!("unexpected race outcome: {other:?}"),
        }
        assert!(matches!(
            db.create_payment_order(
                &ctx,
                customer.organization.id,
                &new_order(offer.bundle_version_id),
            )
            .await,
            Err(WalletError::NotFound(_))
        ));
    }
}

#[tokio::test]
async fn delayed_grant_expires_at_the_sold_instant_and_cannot_credit_another_wallet() {
    let Some((db, owner, customer)) = world().await else {
        return;
    };
    let ctx = merchant(&db, &owner).await;
    let offer = db.publish_bundle(&owner, &request()).await.unwrap().offer;
    let order = create(&db, &ctx, customer.organization.id, offer.bundle_version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &customer.organization.tenant_id)
        .await
        .unwrap();
    // Isolated delayed-worker fixture: the first paid instant is historical,
    // never an update to an already recorded payment time.
    let paid_at: time::OffsetDateTime = sqlx::query_scalar("UPDATE oxsum.payment_orders SET status='paid',paid_at=now()-interval '31 days' WHERE order_id=$1 RETURNING paid_at")
        .bind(order.order_id).fetch_one(db.pool()).await.unwrap();
    let other = register_customer(&db).await;
    let wrong = Wallet::open(db.pool().clone(), &other.organization.tenant_id)
        .await
        .unwrap();
    let on = time::OffsetDateTime::now_utc().date();
    assert!(matches!(
        db.fulfill_payment_order(order.order_id, &wrong, on).await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(wrong.available().await.unwrap(), 0);
    db.fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 5_000_000);
    let lots = db.promo_lots(customer.organization.id, true).await.unwrap();
    assert_eq!(lots.len(), 1);
    assert_eq!(lots[0].expires_at, Some(paid_at + time::Duration::days(30)));
    assert_eq!(lots[0].close_reason.as_deref(), Some("expired"));
    assert_eq!(lots[0].remaining_minor, 0);
    db.fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 5_000_000);
}

#[tokio::test]
async fn partial_and_full_refunds_cancel_promotion_without_refunding_or_regranting_it() {
    let Some((db, owner, customer)) = world().await else {
        return;
    };
    let ctx = merchant(&db, &owner).await;
    let offer = db.publish_bundle(&owner, &request()).await.unwrap().offer;
    let order = create(&db, &ctx, customer.organization.id, offer.bundle_version_id).await;
    let mut event = paid(&db, &ctx, &order).await;
    let wallet = Wallet::open(db.pool().clone(), &customer.organization.tenant_id)
        .await
        .unwrap();
    let on = time::OffsetDateTime::now_utc().date();
    let request = NewRefundRequest {
        amount_minor: Some(2_000_000),
        reason: "partial refund".into(),
        request_key: Uuid::new_v4().to_string(),
        requested_by: None,
    };
    assert!(matches!(
        db.request_order_refund(customer.organization.id, order.order_id, &request)
            .await,
        Err(WalletError::Conflict(_))
    ));
    db.fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();
    // A hold pins some promotional credit before the refund arrives.
    wallet
        .hold("bundle-refund-hold", "local request", 700_000, on)
        .await
        .unwrap();
    let bad = NewRefundRequest {
        amount_minor: Some(6_000_000),
        ..request.clone()
    };
    assert!(
        db.request_order_refund(customer.organization.id, order.order_id, &bad)
            .await
            .is_err()
    );
    let refund = db
        .request_order_refund(customer.organization.id, order.order_id, &request)
        .await
        .unwrap();
    db.resolve_order_refund(
        &owner,
        refund.refund_id,
        RefundDecision::Approve,
        Some("approve fixture"),
        "bundle-refund-approve",
    )
    .await
    .unwrap();
    db.note_refund_provider(refund.refund_id, "bundle-refund-part")
        .await
        .unwrap();
    event.event_id = Uuid::new_v4().to_string();
    event.kind = MerchantEventKind::RefundSucceeded;
    event.refund_id = Some("bundle-refund-part".into());
    event.amount_minor = Some(200);
    let receipt = db
        .receive_merchant_event(&ctx, &event, &[8; 32])
        .await
        .unwrap();
    assert_eq!(receipt.state, "accepted");
    db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 3_000_000);
    wallet
        .settle("bundle-refund-hold", "release held promotion", 0, on)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 3_000_000);
    let lots = db.promo_lots(customer.organization.id, true).await.unwrap();
    assert_eq!(lots[0].close_reason.as_deref(), Some("cancelled"));
    assert_eq!(lots[0].remaining_minor, 0);
    let rest = db
        .request_order_refund(
            customer.organization.id,
            order.order_id,
            &NewRefundRequest {
                amount_minor: None,
                request_key: Uuid::new_v4().to_string(),
                ..request
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.amount_minor, 3_000_000);
    assert_eq!(rest.display_amount_minor, 300);
    db.resolve_order_refund(
        &owner,
        rest.refund_id,
        RefundDecision::Approve,
        Some("remaining purchase"),
        "bundle-refund-rest",
    )
    .await
    .unwrap();
    db.note_refund_provider(rest.refund_id, "bundle-refund-rest")
        .await
        .unwrap();
    event.event_id = Uuid::new_v4().to_string();
    event.refund_id = Some("bundle-refund-rest".into());
    event.amount_minor = Some(300);
    let receipt = db
        .receive_merchant_event(&ctx, &event, &[9; 32])
        .await
        .unwrap();
    assert_eq!(receipt.state, "accepted");
    db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    db.fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(
        db.order_refundable_minor(customer.organization.id, order.order_id)
            .await
            .unwrap(),
        Some(0)
    );
}
