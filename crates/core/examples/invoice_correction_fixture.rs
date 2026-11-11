//! Local acceptance fixture: activate a bound priced plan through the real
//! quoted merchant order, signed event, activation and fulfillment path so the
//! posted invoice journal and statement exist for browser correction checks.
//! Development-only; reads `DATABASE_URL` and takes the owner session token,
//! customer organization and customer plan id as arguments.

use oxsum_core::{
    CollectionCurrency, Db, MerchantCreateRequest, MerchantEventKind, MerchantMode, MerchantRail,
    MerchantStatusRequest, MerchantVersionInput, NewPaymentOrder, PaymentOrderPurpose, SecretKey,
    VerifiedMerchantEvent, Wallet,
};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let mut owner = None;
    let mut organization = None;
    let mut plan = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--session-file" => {
                owner = Some(std::fs::read_to_string(
                    arguments.next().ok_or("--session-file needs a path")?,
                )?)
            }
            "--organization" => {
                organization = Some(
                    arguments
                        .next()
                        .ok_or("--organization needs a UUID")?
                        .parse::<Uuid>()?,
                )
            }
            "--customer-plan" => {
                plan = Some(
                    arguments
                        .next()
                        .ok_or("--customer-plan needs a UUID")?
                        .parse::<Uuid>()?,
                )
            }
            _ => {
                return Err(
                    "usage: invoice_correction_fixture --session-file PATH --organization UUID --customer-plan UUID"
                        .into(),
                )
            }
        }
    }
    let owner = owner.ok_or("--session-file is required")?;
    let owner = owner.trim();
    let organization = organization.ok_or("--organization is required")?;
    let plan = plan.ok_or("--customer-plan is required")?;
    let db = Db::connect(&std::env::var("DATABASE_URL")?, 8).await?;
    db.bootstrap_collection_terms(&CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await?;
    let key = SecretKey::from_bytes([71; 32]);
    let merchant = db
        .create_merchant(
            owner,
            &MerchantCreateRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "local invoice-correction acceptance fixture".into(),
                name: format!(
                    "correction-fixture-{}",
                    &Uuid::new_v4().simple().to_string()[..8]
                ),
                rail: MerchantRail::Hyperswitch,
                mode: MerchantMode::Test,
                account_id: Uuid::new_v4().to_string(),
                config: serde_json::from_value::<MerchantVersionInput>(serde_json::json!({
                    "apiUrl": "https://sandbox.hyperswitch.io",
                    "apiKey": "correction-fixture-api-secret",
                    "webhookSecret": "correction-fixture-webhook-secret",
                    "publishableKey": "pk_correction_fixture",
                    "sdkUrl": "https://sdk.example"
                }))?,
            },
            &key,
        )
        .await?;
    db.set_merchant_enabled(
        owner,
        merchant.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "enable fixture".into(),
            enabled: true,
        },
    )
    .await?;
    let context = db
        .merchant_signing_context(merchant.merchant_id, merchant.version_id, &key)
        .await?
        .ok_or("merchant signing context did not land")?;
    let quote = db.plan_payment_quote(organization).await?;
    let order = db
        .create_payment_order(
            &context,
            organization,
            &NewPaymentOrder {
                expected_collection_revision: Some(quote.terms.collection_revision),
                expected_statement_quote: None,
                statement_amount_minor: None,
                expected_plan_quote: Some(quote.fingerprint),
                purpose: PaymentOrderPurpose::PlanPeriod,
                display_amount_minor: quote.terms.display_amount_minor,
                display_currency: quote.terms.display_currency,
                purpose_ref: Some(plan),
                idempotency_key: Uuid::new_v4().to_string(),
                promo_minor: 0,
            },
        )
        .await?;
    let receipt = db
        .receive_merchant_event(
            &context,
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
        .await?;
    db.apply_order_event(&receipt).await?;
    db.activate_plan_period(order.order_id, plan).await?;
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(organization)
            .fetch_one(db.pool())
            .await?;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await?;
    db.fulfill_payment_order(
        order.order_id,
        &wallet,
        time::OffsetDateTime::now_utc().date(),
    )
    .await?;
    let journal: Option<Uuid> = sqlx::query_scalar(
        "SELECT journal_id FROM oxsum.invoice_journals WHERE organization_id=$1 AND state<>'draft' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(organization)
    .fetch_optional(db.pool())
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "orderId": order.order_id,
            "journalId": journal,
            "customerPlanId": plan,
        }))?
    );
    Ok(())
}
