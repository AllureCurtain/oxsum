//! Paid plan fixtures use the same quoted order and verified-event boundary as production.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{
    CollectionCurrency, Db, MerchantCreateRequest, MerchantEventKind, MerchantMode, MerchantRail,
    MerchantStatusRequest, MerchantVersionInput, NewPaymentOrder, PaymentOrder,
    PaymentOrderPurpose, SecretKey, VerifiedMerchantEvent,
};
use uuid::Uuid;

pub async fn merchant_context(db: &Db, owner: &str) -> oxsum_core::MerchantSigningContext {
    db.bootstrap_collection_terms(&CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let key = SecretKey::from_bytes([71; 32]);
    let merchant = db.create_merchant(owner, &MerchantCreateRequest {
        idempotency_key: Uuid::new_v4().to_string(), reason: "local plan payment fixture".into(),
        name: format!("plan-fixture-{}",Uuid::new_v4()), rail: MerchantRail::Hyperswitch,
        mode: MerchantMode::Test, account_id: Uuid::new_v4().to_string(),
        config: serde_json::from_value::<MerchantVersionInput>(serde_json::json!({
            "apiUrl":"https://sandbox.hyperswitch.io", "apiKey":"plan-fixture-api-secret",
            "webhookSecret":"plan-fixture-webhook-secret", "publishableKey":"pk_plan_fixture",
            "sdkUrl":"https://sdk.example"
        })).unwrap(),
    }, &key).await.unwrap();
    db.set_merchant_enabled(
        owner,
        merchant.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "enable fixture".into(),
            enabled: true,
        },
    )
    .await
    .unwrap();
    db.merchant_signing_context(merchant.merchant_id, merchant.version_id, &key)
        .await
        .unwrap()
        .unwrap()
}

/// Create one customer-owned quote-backed order without activating it.
pub async fn created_order(
    db: &Db,
    context: &oxsum_core::MerchantSigningContext,
    organization: Uuid,
    plan: Uuid,
) -> PaymentOrder {
    let quote = db.plan_payment_quote(organization).await.unwrap();
    assert_eq!(quote.terms.customer_plan_id, plan);
    db.create_payment_order(
        context,
        organization,
        &NewPaymentOrder {
            expected_collection_revision: Some(quote.terms.collection_revision),
            expected_plan_quote: Some(quote.fingerprint),
            purpose: PaymentOrderPurpose::PlanPeriod,
            display_amount_minor: quote.terms.display_amount_minor,
            display_currency: quote.terms.display_currency,
            purpose_ref: Some(plan),
            idempotency_key: Uuid::new_v4().to_string(),
            promo_minor: 0,
        },
    )
    .await
    .unwrap()
}

pub async fn mark_paid(
    db: &Db,
    context: &oxsum_core::MerchantSigningContext,
    order: &PaymentOrder,
) {
    let organization = order.organization_id;
    let receipt = db
        .receive_merchant_event(
            context,
            &VerifiedMerchantEvent {
                event_id: Uuid::new_v4().to_string(),
                kind: MerchantEventKind::Succeeded,
                payment_id: Some(order.order_id),
                provider_payment_id: Some(format!("pay_{}", order.order_id)),
                amount_minor: Some(order.display_amount_minor),
                currency: Some(order.display_currency.clone()),
                refund_id: None,
                organization_id: Some(organization),
                provider_account_id: Some(context.account_id.clone()),
                mode: Some(MerchantMode::Test),
            },
            &[71; 32],
        )
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();
}

pub async fn paid_order(db: &Db, owner: &str, organization: Uuid, plan: Uuid) -> PaymentOrder {
    let context = merchant_context(db, owner).await;
    let order = created_order(db, &context, organization, plan).await;
    mark_paid(db, &context, &order).await;
    db.payment_order(organization, order.order_id)
        .await
        .unwrap()
        .unwrap()
}

/// Controlled usage fixture leaves an overage debt after the exact base fee is paid.
pub async fn overage(db: &Db, organization: Uuid, plan: Uuid, amount: i64) {
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=period_start-interval '1 day' WHERE customer_plan_id=$1 AND state='active'")
        .bind(plan).execute(db.pool()).await.unwrap();
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(organization)
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query("INSERT INTO oxsum.usage_records(request_id,tenant_id,model,channel,price_version,kind,entry_id,input_tokens,output_tokens,charged_minor,freeze_minor,settled_at) VALUES($1,$2,'fixture','fixture',1,'usage',$3,1,1,$4,$4,now()-interval '120 seconds')")
        .bind(Uuid::new_v4().to_string()).bind(tenant).bind(Uuid::new_v4()).bind(amount).execute(db.pool()).await.unwrap();
}

/// Fulfill a paid fixture, including its durable receipt/outbox.
pub async fn fulfill(db: &Db, order: &PaymentOrder) {
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(order.organization_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant)
        .await
        .unwrap();
    db.fulfill_payment_order(
        order.order_id,
        &wallet,
        time::OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
}
