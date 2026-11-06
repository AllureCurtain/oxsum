//! Bridge the durable merchant inbox to the core legacy-deposit adapter.
use oxsum_core::{
    Db, MerchantEventKind, MerchantEventReceipt, MerchantPaymentPurpose, Tenants, WalletError,
};
/// The verified event drives the same deposit settles the legacy webhook did:
/// a success credits, an expiry closes, a refund or dispute settles the standing
/// — each operation is itself idempotent, so replaying a retained event is safe.
pub(crate) async fn apply_legacy_event(
    db: &Db,
    tenants: &Tenants,
    receipt: &MerchantEventReceipt,
) -> Result<(), WalletError> {
    if receipt.state != "accepted" {
        return Ok(());
    }
    let Some(payment_id) = receipt.payment_id else {
        return Ok(());
    };
    let Some(binding) = db.merchant_payment_binding(payment_id).await? else {
        return Ok(());
    };
    if binding.purpose != MerchantPaymentPurpose::LegacyDeposit {
        return Ok(());
    }
    let Some((organization, tenant_id, rail)) = db.legacy_payment_target(payment_id).await? else {
        return Ok(());
    };
    match receipt.kind {
        MerchantEventKind::Succeeded => {
            let wallet = tenants.get(&tenant_id).await?;
            db.apply_legacy_merchant_event(&wallet, receipt, crate::today())
                .await
        }
        MerchantEventKind::Expired => {
            match rail.as_str() {
                "hyperswitch" => db.expire_hyperswitch_deposit(payment_id).await?,
                "stripe" => db.expire_stripe_deposit(payment_id).await?,
                "airwallex" => db.expire_airwallex_deposit(payment_id).await?,
                _ => {}
            }
            db.mark_merchant_event_applied(receipt.id).await
        }
        MerchantEventKind::RefundSucceeded | MerchantEventKind::RefundFailed => {
            // Refunds and disputes are Hyperswitch's only legacy notifications.
            if rail == "hyperswitch"
                && let Some(event) = db.merchant_event_data(receipt.id).await?
                && let Some(refund_id) = event.refund_id.as_deref()
            {
                let status = if receipt.kind == MerchantEventKind::RefundSucceeded {
                    "succeeded"
                } else {
                    "failed"
                };
                db.settle_hyperswitch_refund(payment_id, refund_id, status)
                    .await?;
            }
            db.mark_merchant_event_applied(receipt.id).await
        }
        MerchantEventKind::Dispute => {
            if rail == "hyperswitch" {
                let dispute = db
                    .merchant_event_data(receipt.id)
                    .await?
                    .and_then(|e| serde_json::to_value(&e).ok())
                    .unwrap_or_default();
                db.record_dispute(payment_id, organization, &dispute)
                    .await?;
            }
            db.mark_merchant_event_applied(receipt.id).await
        }
        MerchantEventKind::Failed | MerchantEventKind::Unsupported => {
            db.mark_merchant_event_applied(receipt.id).await
        }
    }
}
pub(crate) async fn recover(db: &Db, tenants: &Tenants) -> Result<(), WalletError> {
    for event in db.recoverable_merchant_events(100).await? {
        let event = db.retry_merchant_event(event.id).await?;
        apply_legacy_event(db, tenants, &event).await?;
    }
    Ok(())
}

/// Preserve the customer checkout shape while pinning merchant terms before
/// sending. Provider idempotency always uses the durable local payment UUID.
pub(crate) async fn checkout(
    state: &crate::AppState,
    organization: uuid::Uuid,
    rail: oxsum_core::MerchantRail,
    key: &str,
    amount: i64,
) -> Result<serde_json::Value, crate::error::ApiError> {
    use crate::error::ApiError;
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    let selected = state
        .db
        .environment_merchant(rail, secret)
        .await?
        .ok_or_else(|| {
            ApiError::ServiceUnavailable(
                "explicit merchant bootstrap is required for legacy checkout".into(),
            )
        })?;
    let checkout = state
        .db
        .begin_merchant_deposit(&selected, organization, key, amount)
        .await?;
    let payment = &checkout.payment;
    let context = state
        .db
        .merchant_signing_context(payment.merchant_id, payment.version_id, secret)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let complete = match rail {
        oxsum_core::MerchantRail::Stripe => checkout.checkout_url.is_some(),
        _ => checkout.client_secret.is_some(),
    };
    let (provider, client_secret, url) = if complete {
        (
            payment
                .provider_payment_id
                .clone()
                .ok_or(ApiError::Internal)?,
            checkout.client_secret,
            checkout.checkout_url,
        )
    } else {
        let public = state.config.public_url().ok_or_else(|| {
            ApiError::ServiceUnavailable("public return URL is not configured".into())
        })?;
        let created = oxsum_integrations::merchant_checkout::create(
            &context,
            payment,
            &state.provider_http,
            public,
        )
        .await
        .map_err(|_| {
            ApiError::ServiceUnavailable(
                "merchant checkout request did not complete; retry the same idempotency key".into(),
            )
        })?;
        state
            .db
            .record_merchant_payment_reference(payment.payment_id, &created.provider_payment_id)
            .await?;
        match rail {
            oxsum_core::MerchantRail::Hyperswitch => {
                state
                    .db
                    .hyperswitch_payment(
                        payment.payment_id,
                        &created.provider_payment_id,
                        created.client_secret.as_deref().ok_or(ApiError::Internal)?,
                    )
                    .await?
            }
            oxsum_core::MerchantRail::Stripe => {
                state
                    .db
                    .stripe_session_id(
                        payment.payment_id,
                        &created.provider_payment_id,
                        created.checkout_url.as_deref().ok_or(ApiError::Internal)?,
                    )
                    .await?
            }
            oxsum_core::MerchantRail::Airwallex => {
                state
                    .db
                    .airwallex_intent(
                        payment.payment_id,
                        &created.provider_payment_id,
                        created.client_secret.as_deref().ok_or(ApiError::Internal)?,
                        created.checkout_url.as_deref().ok_or(ApiError::Internal)?,
                    )
                    .await?
            }
        }
        (
            created.provider_payment_id,
            created.client_secret,
            created.checkout_url,
        )
    };
    Ok(match rail {
        oxsum_core::MerchantRail::Hyperswitch => {
            serde_json::json!({"depositId":payment.payment_id,"paymentId":provider,"clientSecret":client_secret,"publishableKey":context.version.publishable_key,"sdkUrl":context.version.sdk_url})
        }
        oxsum_core::MerchantRail::Stripe => {
            serde_json::json!({"depositId":payment.payment_id,"checkoutUrl":url})
        }
        oxsum_core::MerchantRail::Airwallex => {
            serde_json::json!({"depositId":payment.payment_id,"intentId":provider,"clientSecret":client_secret,"checkoutUrl":url})
        }
    })
}
