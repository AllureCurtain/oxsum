//! Customer-owned, period-specific payment quotes for sold plan activation.
use crate::{Db, Display, WalletError, business_staff as authority};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}

/// Immutable service and collection terms of one plan payment intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanPaymentTerms {
    /// Customer's assigned plan identity.
    pub customer_plan_id: Uuid,
    /// Exact sold version whose fee and entitlement this order purchases.
    pub plan_version_id: Uuid,
    /// Zero-based period authorized by this payment.
    pub period_index: i32,
    /// Renewal boundary; absent for an initial period starting at activation.
    #[serde(with = "time::serde::rfc3339::option")]
    pub period_start: Option<OffsetDateTime>,
    /// Sold renewal end; absent for an initial period starting at activation.
    #[serde(with = "time::serde::rfc3339::option")]
    pub period_end: Option<OffsetDateTime>,
    /// Exact credit-minor fee, never inverse-rounded from the fiat price.
    pub fee_minor: i64,
    /// Business collection revision the customer saw.
    pub collection_revision: i64,
    /// Currency charged by the provider.
    pub display_currency: String,
    /// Provider minor units rounded up to cover the sold fee.
    pub display_amount_minor: i64,
    /// Exact decimal integer numerator of currency-major units per credit.
    pub rate_mantissa: String,
    /// Decimal scale of the conversion.
    pub rate_scale: u32,
}
impl PlanPaymentTerms {
    /// Stable precondition over every immutable quoted field.
    pub fn fingerprint(&self) -> Result<String, WalletError> {
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("invalid plan payment terms"))?;
        let mut digest = Sha256::new();
        digest.update(b"oxsum/plan-payment-quote/v1\0");
        digest.update(bytes);
        Ok(digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
}

/// Customer-safe quote plus the standing order to resume after a lost response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPaymentQuote {
    /// All sold and collection terms that a new payment must match.
    pub terms: PlanPaymentTerms,
    /// Required unchanged-intent precondition on new plan orders.
    pub fingerprint: String,
    /// Existing live payment for this period, when already created.
    pub existing_order_id: Option<Uuid>,
}

/// A positive fee's exact fiat ceiling, using checked integer arithmetic.
pub(crate) fn fiat_minor(fee: i64, display: &Display) -> Result<i64, WalletError> {
    if fee <= 0 || fee > 9_007_199_254_740_991 {
        return Err(invalid("plan fee is outside the supported exact range"));
    }
    let numerator = i128::from(fee)
        .checked_mul(display.rate_mantissa())
        .and_then(|n| n.checked_mul(100))
        .ok_or_else(|| invalid("plan price overflows"))?;
    let denominator = 10_i128.pow(6 + display.rate_scale());
    let amount = numerator
        .checked_add(denominator - 1)
        .ok_or_else(|| invalid("plan price overflows"))?
        / denominator;
    if !(1..=9_007_199_254_740_991).contains(&amount) {
        return Err(invalid("plan price is outside the supported exact range"));
    }
    Ok(amount as i64)
}

/// Recheck persisted terms before any entitlement or ledger effect. The
/// conversion is a fiat ceiling; it never changes the exact sold credit fee.
pub(crate) fn validate_order(
    order: &crate::PaymentOrder,
    terms: &PlanPaymentTerms,
) -> Result<(), WalletError> {
    let display = crate::collection_terms::CollectionCurrency {
        code: terms.display_currency.clone(),
        rate_per_credit: format_rate(&terms.rate_mantissa, terms.rate_scale)?,
    }
    .display()?;
    if terms.customer_plan_id != order.purpose_ref.unwrap_or(Uuid::nil())
        || terms.fee_minor != order.amount_minor
        || terms.fee_minor <= 0
        || terms.display_currency != order.display_currency
        || terms.display_amount_minor != order.display_amount_minor
        || terms.rate_mantissa != order.rate_mantissa.to_string()
        || terms.rate_scale != order.rate_scale
        || terms.collection_revision <= 0
        || order.promo_minor != 0
        || order.currency != "USD"
        || fiat_minor(terms.fee_minor, &display)? != order.display_amount_minor
        || terms.period_index < 0
        || (terms.period_index == 0 && (terms.period_start.is_some() || terms.period_end.is_some()))
        || (terms.period_index > 0
            && !matches!((terms.period_start,terms.period_end), (Some(start),Some(end)) if end > start))
    {
        return Err(WalletError::Conflict(
            "stored order does not match the sold plan payment".into(),
        ));
    }
    Ok(())
}

fn format_rate(mantissa: &str, scale: u32) -> Result<String, WalletError> {
    if scale > 8
        || mantissa.is_empty()
        || mantissa.len() > 19
        || !mantissa.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(invalid("invalid plan payment rate"));
    }
    let number = mantissa
        .parse::<i64>()
        .map_err(|_| invalid("invalid plan payment rate"))?;
    if number <= 0 {
        return Err(invalid("invalid plan payment rate"));
    }
    let divisor = 10_i64.pow(scale);
    if scale == 0 {
        return Ok(number.to_string());
    }
    Ok(format!(
        "{}.{:0width$}",
        number / divisor,
        number % divisor,
        width = scale as usize
    ))
}

/// Read and lock the live assignment. No supplied customer-plan identity may
/// substitute a different organization's assignment.
pub(crate) async fn quote_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    organization: Uuid,
    expected_plan: Option<Uuid>,
) -> Result<PlanPaymentQuote, WalletError> {
    let row = sqlx::query(
        "SELECT cp.*,sp.price_minor,sp.currency,sp.period_kind FROM oxsum.customer_plans cp \
         JOIN oxsum.sold_plans sp USING(plan_version_id) \
         WHERE cp.organization_id=$1 AND sp.business_id=$2 \
           AND cp.state IN ('pending_payment','active','renewal_pending','past_due') \
         FOR UPDATE OF cp",
    )
    .bind(organization)
    .bind(business)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| WalletError::NotFound("no payable customer plan".into()))?;
    let customer_plan_id: Uuid = row.try_get("customer_plan_id")?;
    if expected_plan.is_some_and(|id| id != customer_plan_id) {
        return Err(WalletError::NotFound("no such customer plan".into()));
    }
    let fee: i64 = row.try_get("price_minor")?;
    if fee <= 0 {
        return Err(invalid("a complimentary plan does not take a paid order"));
    }
    if row.try_get::<bool, _>("cancel_at_period_end")? {
        return Err(WalletError::Conflict(
            "this plan is scheduled to cancel".into(),
        ));
    }
    if row
        .try_get::<Option<Uuid>, _>("scheduled_change")?
        .is_some()
    {
        return Err(WalletError::Conflict(
            "the scheduled plan change must reach its boundary before payment".into(),
        ));
    }
    let state: String = row.try_get("state")?;
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let (index, start, end) = if state == "pending_payment" {
        (0, None, None)
    } else {
        let previous = sqlx::query("SELECT period_index,period_end FROM oxsum.plan_periods WHERE customer_plan_id=$1 ORDER BY period_index DESC LIMIT 1")
            .bind(customer_plan_id).fetch_one(&mut **tx).await?;
        let start: OffsetDateTime = previous.try_get("period_end")?;
        if start > now {
            return Err(WalletError::Conflict(
                "manual renewal becomes available at the current period boundary".into(),
            ));
        }
        let index = previous
            .try_get::<i32, _>("period_index")?
            .checked_add(1)
            .ok_or_else(|| invalid("plan period index overflows"))?;
        let kind = crate::PeriodKind::parse(&row.try_get::<String, _>("period_kind")?)
            .ok_or_else(|| invalid("unknown plan period policy"))?;
        let anchor: Option<OffsetDateTime> = row.try_get("activated_at")?;
        let end = crate::plans::period_end_for(kind, anchor.unwrap_or(start).date(), start);
        (index, Some(start), Some(end))
    };
    // Find the standing order before re-quoting a changed collection policy.
    // The existing immutable payment remains the safe recovery target.
    let standing = sqlx::query("SELECT order_id,plan_payment FROM oxsum.payment_orders WHERE purpose='plan_period' AND purpose_ref=$1 AND (plan_payment->>'periodIndex')::integer=$2 AND status NOT IN ('failed','expired') ORDER BY created_at LIMIT 1")
        .bind(customer_plan_id).bind(index).fetch_optional(&mut **tx).await?;
    if let Some(standing) = standing {
        let terms: PlanPaymentTerms = serde_json::from_value(standing.try_get("plan_payment")?)
            .map_err(|_| invalid("invalid stored plan payment"))?;
        return Ok(PlanPaymentQuote {
            fingerprint: terms.fingerprint()?,
            terms,
            existing_order_id: Some(standing.try_get("order_id")?),
        });
    }
    let collection = crate::collection_terms::terms_in(tx, business).await?;
    let code: String = row.try_get("currency")?;
    let display =
        crate::collection_terms::rate_in(tx, business, &code, Some(collection.revision)).await?;
    let terms = PlanPaymentTerms {
        customer_plan_id,
        plan_version_id: row.try_get("plan_version_id")?,
        period_index: index,
        period_start: start,
        period_end: end,
        fee_minor: fee,
        collection_revision: collection.revision,
        display_currency: code,
        display_amount_minor: fiat_minor(fee, &display)?,
        rate_mantissa: display.rate_mantissa().to_string(),
        rate_scale: display.rate_scale(),
    };
    Ok(PlanPaymentQuote {
        fingerprint: terms.fingerprint()?,
        terms,
        existing_order_id: None,
    })
}
impl Db {
    /// Quote the authenticated customer's own next payable plan period.
    pub async fn plan_payment_quote(
        &self,
        organization: Uuid,
    ) -> Result<PlanPaymentQuote, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let quote = quote_in(&mut tx, business, organization, None).await?;
        tx.commit().await?;
        Ok(quote)
    }
}
