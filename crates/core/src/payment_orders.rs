//! Payment orders and durable fulfillment (issue #606).
//!
//! A payment order separates *paid* from *fulfilled*: the signed merchant
//! callback establishes the first fact inside `merchant_event_inbox`, and a
//! distinct fulfillment step grants the value the order's purpose bought. The
//! order pins its terms — amount, currency, purpose and the server-rated
//! conversion — before any provider call, reusing `merchant_payment_bindings`
//! (purpose `'order'`) as the expected-payment record and `merchant_payment_refs`
//! as the provider-idempotency dedup: a retried create recovers the same local
//! UUID, so a retry never issues a second provider charge.
//!
//! Fulfillment is exhaustive on purpose and one-sided by construction: a
//! `wallet_topup` order books wallet cash, a `bundle` books purchased credit
//! plus a separate promo lot, a `plan_period` activates quota, and an
//! `invoice_repayment` repays a drawn receivable — no purpose may both grant
//! quota and wallet cash.

use crate::{
    BusinessCapability, Db, MerchantEventKind, MerchantEventReceipt, WalletError,
    business_staff as authority,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// The conversion the server rated a display amount at — the `Display` rate's
/// `mantissa / 10^scale` units of currency per credit, kept as the order's own
/// columns so a later re-rate cannot rewrite what the customer agreed to pay.
#[derive(Debug, Clone, Copy)]
pub struct OrderRate {
    /// Rate numerator: currency major units per credit = `mantissa / 10^scale`.
    pub mantissa: i128,
    /// Decimal scale applied to `mantissa`.
    pub scale: u32,
}

/// The one fulfillment arm a payment order may take. The wire spelling is
/// camelCase like every API enum; `as_str`/`parse` hold the snake_case the
/// database stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PaymentOrderPurpose {
    /// Credits wallet cash only — never grants quota.
    WalletTopup,
    /// Posts purchased credit plus a separate promo lot under one fulfillment.
    Bundle,
    /// Activates a plan period's quota — never posts wallet cash.
    PlanPeriod,
    /// Repays a drawn receivable only — never credits wallet or bonus.
    InvoiceRepayment,
}
impl PaymentOrderPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::WalletTopup => "wallet_topup",
            Self::Bundle => "bundle",
            Self::PlanPeriod => "plan_period",
            Self::InvoiceRepayment => "invoice_repayment",
        }
    }
    /// `pub(crate)` so `refunds.rs` reads the same stored spelling rather
    /// than duplicating the parse.
    pub(crate) fn parse(value: &str) -> Result<Self, WalletError> {
        match value {
            "wallet_topup" => Ok(Self::WalletTopup),
            "bundle" => Ok(Self::Bundle),
            "plan_period" => Ok(Self::PlanPeriod),
            "invoice_repayment" => Ok(Self::InvoiceRepayment),
            _ => Err(invalid("invalid stored payment order purpose")),
        }
    }
}

/// A payment order as the customer and the reconciliation report read it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentOrder {
    /// Local order identifier.
    pub order_id: Uuid,
    /// The merchant/version the checkout was offered under. `None` on a
    /// `source: manual` order — a recorded receipt never transited a rail.
    pub merchant_id: Option<Uuid>,
    /// Merchant configuration version in force at creation; `None` for manual orders.
    pub merchant_version_id: Option<Uuid>,
    /// Customer organization that owns the order.
    pub organization_id: Uuid,
    /// What the payment buys: credits, or a subscription period.
    pub purpose: PaymentOrderPurpose,
    /// Ledger terms: integer minor units of one credit, frozen at create.
    pub amount_minor: i64,
    /// Currency of `amount_minor`; currently `"USD"`.
    pub currency: String,
    /// The display amount the customer was quoted, and the pinned rate that
    /// produced `amount_minor` from it.
    pub display_currency: String,
    /// Quoted amount in integer minor units of `display_currency`.
    pub display_amount_minor: i64,
    /// Numerator of the pinned conversion rate, `mantissa / 10^rate_scale`
    /// currency minor units per credit.
    pub rate_mantissa: i128,
    /// Decimal scale of the pinned conversion rate.
    pub rate_scale: u32,
    /// Purpose argument: the plan for `plan_period`, the receivable for
    /// `invoice_repayment`; `None` for single-leg purposes.
    pub purpose_ref: Option<Uuid>,
    /// A `bundle` order's promo lot, in credit-minor; 0 for single-leg purposes.
    pub promo_minor: i64,
    /// Immutable published offer snapshot; absent on legacy/non-bundle orders.
    pub bundle_offer: Option<crate::BundleOffer>,
    /// Immutable customer, sold version, period and fee for a plan payment.
    pub plan_payment: Option<crate::PlanPaymentTerms>,
    /// Provider-side payment identifier once the provider supplied one.
    pub provider_payment_id: Option<String>,
    /// The checkout handles the provider answered, so a replayed create returns
    /// the same checkout. Transient, never part of the frozen terms.
    #[serde(skip_serializing)]
    pub checkout: serde_json::Value,
    /// How the order's payment arrived: `provider` through a rail's signed
    /// webhook, `manual` for a staff-recorded receipt (issue #607).
    pub source: String,
    /// A manual order's audit payload — the receipt `reference`, the operator's
    /// `reason` and the staff `recordedBy`. `{}` on provider orders.
    pub evidence: serde_json::Value,
    /// Lifecycle state: `created`, `provider_pending`, `paid`, `fulfilled`,
    /// `expired`, `failed` or `needs_attention`.
    pub status: String,
    /// When the order was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the signed payment confirmation arrived; `None` until paid.
    #[serde(with = "time::serde::rfc3339::option")]
    pub paid_at: Option<OffsetDateTime>,
}

/// A cursor-paginated page of payment orders.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentOrderPage {
    /// Orders in newest-first order.
    pub orders: Vec<PaymentOrder>,
    /// Order identifier to pass as `after` for the next page; `None` at the end.
    pub next_cursor: Option<Uuid>,
}

/// What a fulfillment granted, as `payment_fulfillments.purpose_payload` records
/// it — the audit of the value the paid order bought, kept beside the grant so
/// it never has to be re-derived.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentFulfillment {
    /// Local fulfillment identifier.
    pub fulfillment_id: Uuid,
    /// Order this fulfillment was granted for.
    pub order_id: Uuid,
    /// Purpose whose single grant arm ran.
    pub purpose: PaymentOrderPurpose,
    /// Audit of what the grant produced, as the purpose arm reported it.
    pub purpose_payload: serde_json::Value,
    /// Outbox row queued for dispatch, when one was written.
    pub outbox_id: Option<Uuid>,
    /// When the fulfillment was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub fulfilled_at: OffsetDateTime,
}

/// The reconciliation report: paid orders still awaiting fulfillment, fulfilled
/// ones, and the `needs_attention` discrepancies a paid-but-mismatched event
/// parked. `paid - fulfilled` should drain to zero; `needs_attention` never
/// auto-resolves.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentReconciliation {
    /// Orders still awaiting a provider response.
    pub created: i64,
    /// Orders with a provider payment pending confirmation.
    pub provider_pending: i64,
    /// Orders paid but not yet fulfilled.
    pub paid: i64,
    /// Orders whose fulfillment completed.
    pub fulfilled: i64,
    /// Orders that expired before payment.
    pub expired: i64,
    /// Orders whose payment attempt failed.
    pub failed: i64,
    /// Paid orders with a mismatch that requires human resolution.
    pub needs_attention: i64,
    /// Paid orders that have not yet fulfilled — the recovery sweep's backlog.
    pub paid_unfulfilled: Vec<Uuid>,
}

/// What an order create carries: the quoted display amount and currency, the
/// purpose, its reference, and the idempotency key. The ledger `amount_minor`
/// is never client input — the server derives it from the display amount at
/// the rate it owns, so a client-supplied rate or ledger figure is refused at
/// the route rather than reaching here.
#[derive(Debug, Clone)]
pub struct NewPaymentOrder {
    /// Required authoritative quote fingerprint for a new plan-period payment.
    pub expected_plan_quote: Option<String>,
    /// Optional displayed quote revision, checked only for a new operation.
    pub expected_collection_revision: Option<i64>,
    /// The fulfillment arm this order will take.
    pub purpose: PaymentOrderPurpose,
    /// The fiat minor amount the provider will charge, in `display_currency`.
    pub display_amount_minor: i64,
    /// ISO 4217 code of the fiat currency the customer is quoted in.
    pub display_currency: String,
    /// The purpose's argument: the plan a `plan_period` activates, the
    /// receivable an `invoice_repayment` settles, or the published bundle version.
    /// `None` for wallet top-up.
    pub purpose_ref: Option<Uuid>,
    /// Caller-supplied key that makes repeated creates return the same order.
    pub idempotency_key: String,
    /// Deprecated assertion: zero or the selected offer's promotional credit.
    /// The offer, never this field, supplies the granted quantity.
    pub promo_minor: i64,
}

/// A staff-recorded manual receipt (issue #607): the fiat amount that arrived
/// out of band, the receipt's reference, and the customer it funds. The ledger
/// amount rates server-side like a provider order's — a staff-supplied credit
/// figure never enters. `purpose` is pinned to `walletTopup`: a receipt books
/// wallet cash or it is not a receipt.
#[derive(Debug, Clone)]
pub struct NewManualOrder {
    /// Optional displayed quote revision, checked only for a new operation.
    pub expected_collection_revision: Option<i64>,
    /// Customer organization the receipt funds.
    pub organization: Uuid,
    /// Order purpose; the service pins this to `walletTopup`.
    pub purpose: PaymentOrderPurpose,
    /// The fiat minor amount the receipt evidences, in `display_currency`.
    pub display_amount_minor: i64,
    /// ISO 4217 code of the fiat currency the receipt is denominated in.
    pub display_currency: String,
    /// Caller-supplied key that makes repeated creates return the same order.
    pub idempotency_key: String,
    /// The receipt's own reference — a bank transaction id, a remittance note —
    /// unique per business among manual orders.
    pub reference: String,
    /// The operator's note: why this receipt funds this customer.
    pub reason: String,
}

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

fn order(row: &PgRow) -> Result<PaymentOrder, WalletError> {
    Ok(PaymentOrder {
        order_id: row.try_get("order_id")?,
        merchant_id: row.try_get("merchant_id")?,
        merchant_version_id: row.try_get("merchant_version_id")?,
        organization_id: row.try_get("organization_id")?,
        purpose: PaymentOrderPurpose::parse(&row.try_get::<String, _>("purpose")?)?,
        amount_minor: row.try_get("amount_minor")?,
        currency: row.try_get("currency")?,
        display_currency: row.try_get("display_currency")?,
        display_amount_minor: row.try_get("display_amount_minor")?,
        rate_mantissa: row.try_get::<i64, _>("rate_mantissa")? as i128,
        rate_scale: row.try_get::<i32, _>("rate_scale")? as u32,
        purpose_ref: row.try_get("purpose_ref")?,
        promo_minor: row.try_get("promo_minor")?,
        bundle_offer: row
            .try_get::<Option<serde_json::Value>, _>("bundle_offer")?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| WalletError::Misconfigured("invalid order bundle snapshot".into()))?,
        plan_payment: row
            .try_get::<Option<serde_json::Value>, _>("plan_payment")?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| WalletError::Misconfigured("invalid stored plan payment intent".into()))?,
        provider_payment_id: row.try_get("provider_payment_id")?,
        checkout: row.try_get("checkout")?,
        source: row.try_get("source")?,
        evidence: row.try_get("evidence")?,
        status: row.try_get("status")?,
        created_at: row.try_get("created_at")?,
        paid_at: row.try_get("paid_at")?,
    })
}

pub(crate) async fn order_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<PaymentOrder>, WalletError> {
    let row = sqlx::query(&format!(
        "SELECT {ORDER_COLS} FROM oxsum.payment_orders WHERE order_id=$1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(order).transpose()
}

fn fulfillment(row: &PgRow) -> Result<PaymentFulfillment, WalletError> {
    Ok(PaymentFulfillment {
        fulfillment_id: row.try_get("fulfillment_id")?,
        order_id: row.try_get("order_id")?,
        purpose: PaymentOrderPurpose::parse(&row.try_get::<String, _>("purpose")?)?,
        purpose_payload: row.try_get("purpose_payload")?,
        outbox_id: row.try_get("outbox_id")?,
        fulfilled_at: row.try_get("fulfilled_at")?,
    })
}

const FULFILL_COLS: &str = "fulfillment_id,order_id,purpose,purpose_payload,outbox_id,fulfilled_at";

/// The fulfillment read inside the caller's transaction — the row a just-landed
/// fulfillment INSERT wrote, which a pool connection cannot see until commit.
async fn fulfillment_in(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<Option<PaymentFulfillment>, WalletError> {
    let row = sqlx::query(&format!(
        "SELECT {FULFILL_COLS} FROM oxsum.payment_fulfillments WHERE order_id=$1"
    ))
    .bind(order_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(fulfillment).transpose()
}

/// The lifecycle states the status filter accepts — the same vocabulary the
/// `status` column's CHECK enumerates.
const ORDER_STATUSES: &[&str] = &[
    "created",
    "provider_pending",
    "paid",
    "fulfilled",
    "expired",
    "failed",
    "needs_attention",
];

const ORDER_COLS: &str = "order_id,merchant_id,merchant_version_id,organization_id,\
     purpose,amount_minor,currency,display_currency,display_amount_minor,\
     rate_mantissa,rate_scale,purpose_ref,promo_minor,bundle_offer,plan_payment,provider_payment_id,checkout,source,evidence,status,created_at,paid_at";

impl Db {
    /// Creates a payment order: pins the expected payment under the order's own
    /// UUID (the `merchant_payment_bindings` row `purpose='order'`) and records
    /// the immutable terms in one transaction — so the expectation is durable
    /// before the provider is ever called.
    ///
    /// The business's stored collection policy rates `display_amount_minor`
    /// inside the creation transaction. Customer display settings cannot affect it. Replaying the same
    /// `(organization, idempotency_key)` returns the standing order unchanged —
    /// changed terms under a reused key conflict, like every deposit rail.
    ///
    /// `merchant`/`version` name the immutable merchant version the checkout is
    /// offered under; the merchant must be enabled for this customer, the same
    /// gate `pin_merchant_payment` enforces on the binding.
    pub async fn create_payment_order(
        &self,
        context: &crate::MerchantSigningContext,
        organization: Uuid,
        new: &NewPaymentOrder,
    ) -> Result<PaymentOrder, WalletError> {
        self.create_payment_order_checked(context, organization, new, None)
            .await
    }

    pub(crate) async fn create_payment_order_checked(
        &self,
        context: &crate::MerchantSigningContext,
        organization: Uuid,
        new: &NewPaymentOrder,
        route: Option<&crate::CheckoutChoice>,
    ) -> Result<PaymentOrder, WalletError> {
        authority::validate(&new.idempotency_key, None)?;
        if new.display_amount_minor <= 0
            || new.display_currency.len() != 3
            || !new.display_currency.bytes().all(|c| c.is_ascii_uppercase())
            || !identifier(&new.idempotency_key)
        {
            return Err(invalid("invalid payment order terms"));
        }
        if new.promo_minor < 0
            || (new.purpose != PaymentOrderPurpose::Bundle && new.promo_minor != 0)
        {
            return Err(invalid(
                "promotional credit requires a published bundle offer",
            ));
        }
        if (new.purpose == PaymentOrderPurpose::PlanPeriod && new.promo_minor != 0)
            || (new.purpose != PaymentOrderPurpose::PlanPeriod && new.expected_plan_quote.is_some())
        {
            return Err(invalid(
                "plan quote and promotional terms do not match this payment purpose",
            ));
        }
        // Purpose-specific validation: a plan period needs its plan, an invoice
        // repayment needs the receivable it settles.
        match new.purpose {
            PaymentOrderPurpose::Bundle
            | PaymentOrderPurpose::PlanPeriod
            | PaymentOrderPurpose::InvoiceRepayment
                if new.purpose_ref.is_none() =>
            {
                return Err(invalid("this payment order purpose needs a reference"));
            }
            _ => {}
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        // The order's own UUID serializes its create: two racing creates of the
        // same idempotency key lock the same expected-payment identity.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "payment-order:{organization}:{}",
                new.idempotency_key
            ))
            .execute(&mut *tx)
            .await?;
        // A replayed create finds its own order by the idempotency anchor and
        // returns it unchanged; different terms under the key conflict.
        let existing = sqlx::query(
            "SELECT * FROM oxsum.payment_orders WHERE organization_id=$1 AND idempotency_key=$2",
        )
        .bind(organization)
        .bind(&new.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing) = existing {
            let standing = order(&existing)?;
            let original_rail: Option<String> =
                sqlx::query_scalar("SELECT rail FROM oxsum.merchant_accounts WHERE merchant_id=$1")
                    .bind(standing.merchant_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if original_rail.as_deref() != Some(context.rail.as_str()) {
                return Err(WalletError::Conflict(
                    "payment rail changed for the same idempotency key".into(),
                ));
            }
            if standing.purpose != new.purpose
                || standing.display_amount_minor != new.display_amount_minor
                || standing.display_currency != new.display_currency
                || standing.purpose_ref != new.purpose_ref
                || (standing.purpose == PaymentOrderPurpose::PlanPeriod
                    && standing
                        .plan_payment
                        .as_ref()
                        .map(crate::PlanPaymentTerms::fingerprint)
                        .transpose()?
                        != new.expected_plan_quote)
            {
                return Err(WalletError::Conflict(
                    "payment order terms changed for the same idempotency key".into(),
                ));
            }
            // Zero omits the optional bundle assertion. The stored promotion
            // comes from the published offer, so replay compares only an
            // explicit assertion with that original grant.
            if new.promo_minor != 0 && new.promo_minor != standing.promo_minor {
                return Err(WalletError::Conflict(
                    "bundle promotion assertion changed on replay".into(),
                ));
            }
            tx.commit().await?;
            return Ok(standing);
        }
        if let Some(route) = route {
            crate::merchant_routes::guard_in(&mut tx, business, context.rail, route).await?;
        }
        // Gate on the merchant being enabled for this customer, mirroring
        // `pin_merchant_payment`'s check — the binding and the order agree on it.
        let allowed = sqlx::query(
            "SELECT m.merchant_id FROM oxsum.merchant_accounts m \
             JOIN oxsum.customer_profiles c ON c.business_id=m.business_id \
             WHERE m.merchant_id=$1 AND m.current_version_id=$2 AND m.enabled \
               AND c.organization_id=$3 FOR SHARE OF m",
        )
        .bind(context.merchant_id)
        .bind(context.version.id)
        .bind(organization)
        .fetch_optional(&mut *tx)
        .await?;
        if allowed.is_none() {
            return Err(WalletError::Forbidden(
                "merchant is not enabled for this customer's new payments".into(),
            ));
        }
        let bundle = if new.purpose == PaymentOrderPurpose::Bundle {
            let offered = crate::bundles::available_in(
                &mut tx,
                business,
                new.purpose_ref
                    .ok_or_else(|| invalid("bundle version is required"))?,
            )
            .await?;
            if offered.terms.display_currency != new.display_currency
                || offered.terms.display_amount_minor != new.display_amount_minor
                || (new.promo_minor != 0 && new.promo_minor != offered.terms.promo_minor)
            {
                return Err(WalletError::Conflict(
                    "checkout does not match the selected published bundle".into(),
                ));
            }
            Some(offered)
        } else {
            None
        };
        let (amount_minor, promo_minor, rate, plan_payment) = if let Some(bundle) = &bundle {
            (
                bundle.terms.purchased_minor,
                bundle.terms.promo_minor,
                bundle.terms.rate()?,
                None,
            )
        } else {
            let display = crate::collection_terms::rate_in(
                &mut tx,
                business,
                &new.display_currency,
                new.expected_collection_revision,
            )
            .await?;
            let plan_payment = if new.purpose == PaymentOrderPurpose::PlanPeriod {
                let quote = crate::plan_payments::quote_in(
                    &mut tx,
                    business,
                    organization,
                    new.purpose_ref,
                )
                .await?;
                if let Some(id) = quote.existing_order_id {
                    return Err(WalletError::Conflict(format!(
                        "this plan period already has order {id}; resume it"
                    )));
                }
                if new.expected_plan_quote.as_deref() != Some(quote.fingerprint.as_str())
                    || new.display_amount_minor != quote.terms.display_amount_minor
                    || new.display_currency != quote.terms.display_currency
                {
                    return Err(WalletError::Conflict(
                        "plan quote changed or does not match; review the current quote".into(),
                    ));
                }
                Some(quote.terms)
            } else {
                None
            };
            let amount = match &plan_payment {
                Some(terms) => terms.fee_minor,
                None => display
                    .to_credit_minor(new.display_amount_minor)
                    .filter(|amount| *amount > 0)
                    .ok_or_else(|| invalid("collection amount does not rate to a credit figure"))?,
            };
            (
                amount,
                0,
                OrderRate {
                    mantissa: display.rate_mantissa(),
                    scale: display.rate_scale(),
                },
                plan_payment,
            )
        };
        let order_id = Uuid::new_v4();
        // The expected payment: the provider will charge the display (fiat)
        // amount, so the binding pins `display_*` for `classify` to match the
        // signed event against. The order's ledger `amount_minor` is what
        // fulfillment books — a separate fact the provider never sees.
        sqlx::query(
            "INSERT INTO oxsum.merchant_payment_bindings\
             (payment_id,merchant_id,version_id,organization_id,amount_minor,currency,purpose,hyperswitch_body_id) \
             VALUES($1,$2,$3,$4,$5,$6,'order',true)",
        )
        .bind(order_id)
        .bind(context.merchant_id)
        .bind(context.version.id)
        .bind(organization)
        .bind(new.display_amount_minor)
        .bind(&new.display_currency)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO oxsum.payment_orders\
             (order_id,business_id,merchant_id,merchant_version_id,organization_id,\
              purpose,amount_minor,currency,display_currency,display_amount_minor,\
              rate_mantissa,rate_scale,purpose_ref,promo_minor,idempotency_key,status,bundle_offer,plan_payment) \
             VALUES($1,$2,$3,$4,$5,$6,$7,'USD',$8,$9,$10,$11,$12,$13,$14,'created',$15,$16)",
        )
        .bind(order_id)
        .bind(business)
        .bind(context.merchant_id)
        .bind(context.version.id)
        .bind(organization)
        .bind(new.purpose.as_str())
        .bind(amount_minor)
        .bind(&new.display_currency)
        .bind(new.display_amount_minor)
        .bind(rate.mantissa as i64)
        .bind(rate.scale as i32)
        .bind(new.purpose_ref)
        .bind(promo_minor)
        .bind(&new.idempotency_key)
        .bind(bundle.as_ref().map(|offer| serde_json::json!(offer)))
        .bind(plan_payment.as_ref().map(|terms| serde_json::json!(terms)))
        .execute(&mut *tx)
        .await?;
        let created = order_in(&mut tx, order_id)
            .await?
            .ok_or_else(|| invalid("the payment order did not land"))?;
        tx.commit().await?;
        Ok(created)
    }

    /// One order the named organization owns — the customer read. An order of
    /// another organization is a not-found, the same scoping deposits use.
    pub async fn payment_order(
        &self,
        organization: Uuid,
        order_id: Uuid,
    ) -> Result<Option<PaymentOrder>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {ORDER_COLS} FROM oxsum.payment_orders WHERE order_id=$1 AND organization_id=$2"
        ))
        .bind(order_id)
        .bind(organization)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(order).transpose()
    }

    /// An organization's orders, newest first, paged by the cursor the previous
    /// page returned — the last order id, resolved back to its `created_at` so
    /// the keyset stays stable under inserts (issue #607: the page the order
    /// history reads, so order is newest-first, not arbitrary uuid order).
    /// `purpose`/`status` narrow the page when present.
    pub async fn organization_payment_orders(
        &self,
        organization: Uuid,
        after: Option<Uuid>,
        limit: i64,
        purpose: Option<PaymentOrderPurpose>,
        status: Option<&str>,
    ) -> Result<PaymentOrderPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        if let Some(status) = status
            && !ORDER_STATUSES.contains(&status)
        {
            return Err(invalid("invalid payment order status filter"));
        }
        let rows = sqlx::query(&format!(
            "SELECT {ORDER_COLS} FROM oxsum.payment_orders \
             WHERE organization_id=$1 \
               AND ($4::text IS NULL OR purpose=$4) \
               AND ($5::text IS NULL OR status=$5) \
               AND ($2::uuid IS NULL OR (created_at, order_id) < \
                    (SELECT created_at, order_id FROM oxsum.payment_orders WHERE order_id=$2)) \
             ORDER BY created_at DESC, order_id DESC LIMIT $3"
        ))
        .bind(organization)
        .bind(after)
        .bind(limit + 1)
        .bind(purpose.map(|p| p.as_str()))
        .bind(status)
        .fetch_all(self.pool())
        .await?;
        let mut orders = rows.iter().map(order).collect::<Result<Vec<_>, _>>()?;
        let more = orders.len() > limit as usize;
        orders.truncate(limit as usize);
        let next_cursor = if more {
            orders.last().map(|o| o.order_id)
        } else {
            None
        };
        Ok(PaymentOrderPage {
            orders,
            next_cursor,
        })
    }

    /// The operator's cross-organization order read (issue #607): every order
    /// the business's customers hold, newest first, paged by the same cursor
    /// rule as [`Self::organization_payment_orders`]. `organization` narrows to
    /// one customer; `purpose`/`status` filter like the customer read.
    /// `ReportsRead` capability is checked by the caller before this runs.
    pub async fn business_payment_orders(
        &self,
        organization: Option<Uuid>,
        purpose: Option<PaymentOrderPurpose>,
        status: Option<&str>,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<PaymentOrderPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        if let Some(status) = status
            && !ORDER_STATUSES.contains(&status)
        {
            return Err(invalid("invalid payment order status filter"));
        }
        let rows = sqlx::query(&format!(
            "SELECT {ORDER_COLS} FROM oxsum.payment_orders \
             WHERE ($3::uuid IS NULL OR organization_id=$3) \
               AND ($4::text IS NULL OR purpose=$4) \
               AND ($5::text IS NULL OR status=$5) \
               AND ($1::uuid IS NULL OR (created_at, order_id) < \
                    (SELECT created_at, order_id FROM oxsum.payment_orders WHERE order_id=$1)) \
             ORDER BY created_at DESC, order_id DESC LIMIT $2"
        ))
        .bind(after)
        .bind(limit + 1)
        .bind(organization)
        .bind(purpose.map(|p| p.as_str()))
        .bind(status)
        .fetch_all(self.pool())
        .await?;
        let mut orders = rows.iter().map(order).collect::<Result<Vec<_>, _>>()?;
        let more = orders.len() > limit as usize;
        orders.truncate(limit as usize);
        let next_cursor = if more {
            orders.last().map(|o| o.order_id)
        } else {
            None
        };
        Ok(PaymentOrderPage {
            orders,
            next_cursor,
        })
    }

    /// The fulfillment record an order landed, if it has one — `None` while the
    /// order is still `paid` or earlier.
    pub async fn payment_fulfillment(
        &self,
        order_id: Uuid,
    ) -> Result<Option<PaymentFulfillment>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {FULFILL_COLS} FROM oxsum.payment_fulfillments WHERE order_id=$1"
        ))
        .bind(order_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(fulfillment).transpose()
    }

    /// Records a verified out-of-band receipt as a `manual` payment order
    /// (issue #607): the business staff's authorized finance action. The
    /// receipt's fiat amount rates to credits at the organization's own display
    /// rate — the same conversion a provider order would pin — and the order
    /// lands already `paid`, its evidence the receipt itself, so the caller's
    /// next step is [`Self::fulfill_payment_order`]: the identical grant path a
    /// webhook-paid order takes, never a ledger bypass.
    ///
    /// `token` is the staff personal session; `FundsWrite` is required inside
    /// the order's own transaction. `evidence.reference` is unique per business
    /// among manual orders — a receipt already recorded is a `409`, never a
    /// second grant — and the `(organization, idempotencyKey)` replay returns
    /// the standing order exactly like a provider order's create.
    ///
    /// The order carries no merchant and no expected-payment binding: nothing
    /// external ever confirms it, and `apply_order_event` can never reach it.
    pub async fn create_manual_payment_order(
        &self,
        token: &str,
        new: &NewManualOrder,
    ) -> Result<PaymentOrder, WalletError> {
        authority::validate(&new.idempotency_key, Some(&new.reason))?;
        if new.display_amount_minor <= 0
            || new.display_currency.len() != 3
            || !new.display_currency.bytes().all(|c| c.is_ascii_uppercase())
            || !identifier(&new.idempotency_key)
            || new.reference.trim().is_empty()
            || new.reference.chars().count() > 200
            || new.purpose != PaymentOrderPurpose::WalletTopup
        {
            return Err(invalid("invalid manual payment order terms"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        // The order's own UUID serializes its create: two racing receipts of
        // the same idempotency key lock the same anchor.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "payment-order:{}:{}",
                new.organization, new.idempotency_key
            ))
            .execute(&mut *tx)
            .await?;
        // The customer must be a live profile of this business — the same gate
        // the provider create applies.
        let allowed = sqlx::query(
            "SELECT organization_id FROM oxsum.customer_profiles \
             WHERE organization_id=$1 AND business_id=$2",
        )
        .bind(new.organization)
        .bind(business)
        .fetch_optional(&mut *tx)
        .await?;
        if allowed.is_none() {
            return Err(WalletError::NotFound("unknown customer".into()));
        }
        let existing = sqlx::query(
            "SELECT * FROM oxsum.payment_orders WHERE organization_id=$1 AND idempotency_key=$2",
        )
        .bind(new.organization)
        .bind(&new.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing) = existing {
            let standing = order(&existing)?;
            if standing.purpose != new.purpose
                || standing.display_amount_minor != new.display_amount_minor
                || standing.display_currency != new.display_currency
                || standing.evidence["reference"].as_str() != Some(new.reference.as_str())
            {
                return Err(WalletError::Conflict(
                    "payment order terms changed for the same idempotency key".into(),
                ));
            }
            tx.commit().await?;
            return Ok(standing);
        }
        let display = crate::collection_terms::rate_in(
            &mut tx,
            business,
            &new.display_currency,
            new.expected_collection_revision,
        )
        .await?;
        let amount_minor = display
            .to_credit_minor(new.display_amount_minor)
            .filter(|amount| *amount > 0)
            .ok_or_else(|| invalid("collection amount does not rate to a credit figure"))?;
        let rate = OrderRate {
            mantissa: display.rate_mantissa(),
            scale: display.rate_scale(),
        };
        let order_id = Uuid::new_v4();
        // The evidence freezes who recorded what receipt — the audit a later
        // dispute or reconciliation reads, immutable with the terms.
        let evidence = json!({
            "reference": new.reference,
            "reason": new.reason,
            "recordedBy": actor.user_id,
        });
        sqlx::query(
            "INSERT INTO oxsum.payment_orders\
             (order_id,business_id,merchant_id,merchant_version_id,organization_id,\
              purpose,amount_minor,currency,display_currency,display_amount_minor,\
              rate_mantissa,rate_scale,purpose_ref,promo_minor,idempotency_key,\
              source,evidence,status,paid_at) \
             VALUES($1,$2,NULL,NULL,$3,$4,$5,'USD',$6,$7,$8,$9,NULL,0,$10,'manual',$11,'paid',now())",
        )
        .bind(order_id)
        .bind(business)
        .bind(new.organization)
        .bind(new.purpose.as_str())
        .bind(amount_minor)
        .bind(&new.display_currency)
        .bind(new.display_amount_minor)
        .bind(rate.mantissa as i64)
        .bind(rate.scale as i32)
        .bind(&new.idempotency_key)
        .bind(&evidence)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            // The manual-reference unique index surfaces as a constraint error:
            // the same receipt funding a second order is a conflict, never a
            // storage failure the caller reads as retryable.
            if let sqlx::Error::Database(db) = &e
                && db.constraint() == Some("payment_orders_manual_reference")
            {
                return WalletError::Conflict(
                    "this receipt reference already funds a manual order".into(),
                );
            }
            WalletError::Storage(e.into())
        })?;
        let created = order_in(&mut tx, order_id)
            .await?
            .ok_or_else(|| invalid("the payment order did not land"))?;
        tx.commit().await?;
        Ok(created)
    }
}

impl Db {
    /// Applies a classified merchant event to the order it names — the
    /// `paid`/`failed`/`expired`/`needs_attention` marking, deliberately one step
    /// short of granting anything. Fulfillment is [`Self::fulfill_payment_order`],
    /// a separate step that can retry independently.
    ///
    /// `paid` is only ever written by this signed-callback path: a success event
    /// that matched the pinned terms moves `provider_pending`/`created` to
    /// `paid`; a mismatch T14's `classify` rejected never reaches here, and an
    /// amount/currency that disagrees on re-check parks the order in
    /// `needs_attention` instead of crediting. The return page can never mark
    /// an order paid — it holds no signed evidence.
    pub async fn apply_order_event(
        &self,
        receipt: &MerchantEventReceipt,
    ) -> Result<(), WalletError> {
        if receipt.state != "accepted" {
            return Ok(());
        }
        let Some(order_id) = receipt.payment_id else {
            return Ok(());
        };
        let mut tx = self.pool().begin().await?;
        let Some(existing) = order_in(&mut tx, order_id).await? else {
            tx.commit().await?;
            return Ok(());
        };
        // Only `order`-purpose bindings enter this path; the legacy deposit
        // adapter owns the other. A terminal or discrepancy order cannot move.
        if !matches!(
            existing.status.as_str(),
            "created" | "provider_pending" | "paid"
        ) {
            tx.commit().await?;
            return Ok(());
        }
        let event = self
            .merchant_event_data(receipt.id)
            .await?
            .ok_or_else(|| invalid("missing stored merchant event"))?;
        match receipt.kind {
            MerchantEventKind::Succeeded => {
                // Re-check the paid terms against the pinned order, not the
                // binding: `classify` already matched the display amount, but
                // the order's own columns are the discrepancy boundary a paid
                // event is held to. A mismatch is `needs_attention` — never a
                // silent credit.
                if event.amount_minor != Some(existing.display_amount_minor)
                    || event.currency.as_deref() != Some(existing.display_currency.as_str())
                {
                    sqlx::query(
                        "UPDATE oxsum.payment_orders SET status='needs_attention',updated_at=now() \
                         WHERE order_id=$1",
                    )
                    .bind(order_id)
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    return Ok(());
                }
                if let Some(provider) = event.provider_payment_id.as_deref() {
                    sqlx::query(
                        "UPDATE oxsum.payment_orders SET provider_payment_id=$2,updated_at=now() \
                         WHERE order_id=$1 AND provider_payment_id IS NULL",
                    )
                    .bind(order_id)
                    .bind(provider)
                    .execute(&mut *tx)
                    .await?;
                }
                sqlx::query(
                    "UPDATE oxsum.payment_orders SET status='paid',paid_at=COALESCE(paid_at,now()),updated_at=now() \
                     WHERE order_id=$1 AND status IN ('created','provider_pending','paid')",
                )
                .bind(order_id)
                .execute(&mut *tx)
                .await?;
            }
            MerchantEventKind::Failed => {
                sqlx::query(
                    "UPDATE oxsum.payment_orders SET status='failed',updated_at=now() \
                     WHERE order_id=$1 AND status IN ('created','provider_pending')",
                )
                .bind(order_id)
                .execute(&mut *tx)
                .await?;
            }
            MerchantEventKind::Expired => {
                sqlx::query(
                    "UPDATE oxsum.payment_orders SET status='expired',updated_at=now() \
                     WHERE order_id=$1 AND status IN ('created','provider_pending')",
                )
                .bind(order_id)
                .execute(&mut *tx)
                .await?;
            }
            // Refunds, disputes and unsupported kinds carry no order
            // transition: `apply_refund_event` owns the refund/dispute arms,
            // and an unsupported kind is intake only.
            _ => {}
        }
        self.mark_merchant_event_applied(receipt.id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Marks the order `provider_pending` and binds its provider payment id —
    /// written after the PSP accepts the checkout, the order's own anchor for
    /// the synchronous response. The provider reference is bound by the shared
    /// `merchant_payment_refs` dedup, so a retry that re-uses the id cannot
    /// issue a second charge.
    pub async fn note_order_provider(
        &self,
        order_id: Uuid,
        provider_payment_id: &str,
        checkout: &serde_json::Value,
    ) -> Result<(), WalletError> {
        if !identifier(provider_payment_id) {
            return Err(invalid("invalid provider payment reference"));
        }
        // Bind through the shared `merchant_payment_refs` dedup (its own
        // transaction) so one provider payment cannot fund two orders — the
        // same guard the inbox's classify applies. Binding before the status
        // move means a retry that re-uses the id can never issue a second charge.
        self.record_merchant_payment_reference(order_id, provider_payment_id)
            .await?;
        let mut tx = self.pool().begin().await?;
        let Some(existing) = order_in(&mut tx, order_id).await? else {
            return Err(WalletError::NotFound("unknown payment order".into()));
        };
        if existing
            .provider_payment_id
            .as_deref()
            .is_some_and(|p| p != provider_payment_id)
        {
            return Err(WalletError::Conflict(
                "provider payment reference changed".into(),
            ));
        }
        sqlx::query(
            "UPDATE oxsum.payment_orders \
             SET status='provider_pending',provider_payment_id=$2,checkout=$3,updated_at=now() \
             WHERE order_id=$1 AND status='created'",
        )
        .bind(order_id)
        .bind(provider_payment_id)
        .bind(checkout)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Recovery's work list: `paid` orders that can fulfill automatically.
    /// Legacy bundles without trusted terms remain visible in reconciliation
    /// but cannot occupy the bounded worker queue and block unrelated payments.
    /// Paid-unfulfilled is the sweep's backlog; a restart re-scans it and the
    /// fulfillment's own idempotency makes the retry a single grant.
    pub async fn unfulfilled_payment_orders(
        &self,
        limit: i64,
    ) -> Result<Vec<PaymentOrder>, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let rows = sqlx::query(&format!(
            "SELECT {ORDER_COLS} FROM oxsum.payment_orders \
             WHERE status='paid' AND (purpose <> 'bundle' OR bundle_offer IS NOT NULL) \
               AND (purpose <> 'plan_period' OR \
               EXISTS(SELECT 1 FROM oxsum.plan_periods p WHERE p.authorization_kind='payment_order' \
                 AND p.authorization_ref=payment_orders.order_id) OR (plan_payment IS NOT NULL \
               AND NOT EXISTS(SELECT 1 FROM oxsum.refunds r WHERE r.order_id=payment_orders.order_id \
                 AND (r.state IN ('requested','provider_pending','succeeded') OR EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=r.refund_id))))) \
             ORDER BY created_at LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(order).collect()
    }

    /// The paid/fulfilled/needs_attention reconciliation the report reads —
    /// counts per lifecycle state plus the paid-unfulfilled order ids.
    pub async fn payment_reconciliation(
        &self,
        token: &str,
    ) -> Result<PaymentReconciliation, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let mut report = PaymentReconciliation::default();
        let rows = sqlx::query(
            "SELECT status,count(*)::bigint AS n FROM oxsum.payment_orders GROUP BY status",
        )
        .fetch_all(&mut *tx)
        .await?;
        for row in &rows {
            let n: i64 = row.try_get("n")?;
            match row.try_get::<String, _>("status")?.as_str() {
                "created" => report.created = n,
                "provider_pending" => report.provider_pending = n,
                "paid" => report.paid = n,
                "fulfilled" => report.fulfilled = n,
                "expired" => report.expired = n,
                "failed" => report.failed = n,
                "needs_attention" => report.needs_attention = n,
                _ => {}
            }
        }
        report.paid_unfulfilled = sqlx::query_scalar(
            "SELECT order_id FROM oxsum.payment_orders WHERE status='paid' ORDER BY created_at LIMIT 100",
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(report)
    }

    /// Fulfills a `paid` order: runs the one grant its purpose allows, records
    /// the fulfillment and its outbox row, and moves the order to `fulfilled` —
    /// using deterministic effect identities across recovery. The fulfillment
    /// record and its dispatch commit together after all grant legs complete.
    ///
    /// The purpose is re-read off the locked order row and dispatched
    /// exhaustively by [`Self::grant_order`], so no order may both grant quota
    /// and wallet cash. Replaying a fulfilled order returns its standing
    /// record; a `needs_attention` or not-yet-paid order is refused — the
    /// discrepancy is a human's, not a retry's, to resolve.
    ///
    /// `wallet` is the order organization's ledger, resolved by the caller.
    /// The grant itself is idempotent under the order-derived ledger key: a
    /// crash after the grant commits but before the fulfillment row lands
    /// leaves a `paid` order the sweep re-runs, and the grant replays rather
    /// than granting twice.
    pub async fn fulfill_payment_order(
        &self,
        order_id: Uuid,
        wallet: &crate::Wallet,
        on: time::Date,
    ) -> Result<PaymentFulfillment, WalletError> {
        let mut tx = self.pool().begin().await?;
        // Serialize the status read; replayable effects survive releasing this
        // transaction before their independent ledger transactions.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("payment-order-fulfill:{order_id}"))
            .execute(&mut *tx)
            .await?;
        let Some(existing) = order_in(&mut tx, order_id).await? else {
            return Err(WalletError::NotFound("unknown payment order".into()));
        };
        if existing.status == "fulfilled" {
            let standing = fulfillment_in(&mut tx, order_id)
                .await?
                .ok_or_else(|| WalletError::Storage(sqlx::Error::RowNotFound.into()))?;
            tx.commit().await?;
            return Ok(standing);
        }
        if existing.status != "paid" {
            return Err(WalletError::Conflict(format!(
                "a {} payment order cannot fulfill",
                existing.status
            )));
        }
        let tenant: String = sqlx::query_scalar(
            "SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1",
        )
        .bind(existing.organization_id)
        .fetch_one(&mut *tx)
        .await?;
        if tenant != wallet.tenant_id() {
            return Err(WalletError::Forbidden(
                "order and wallet belong to different customers".into(),
            ));
        }
        // Committing releases the advisory lock. Racing grants converge through
        // their deterministic ledger and lot identities; the second transaction
        // serializes the single fulfillment/outbox record.
        tx.commit().await?;
        let payload = self.grant_order(&existing, wallet, on).await?;

        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("payment-order-fulfill:{order_id}"))
            .execute(&mut *tx)
            .await?;
        let current = order_in(&mut tx, order_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("unknown payment order".into()))?;
        if current.status == "fulfilled" {
            let standing = fulfillment_in(&mut tx, order_id)
                .await?
                .ok_or_else(|| WalletError::Storage(sqlx::Error::RowNotFound.into()))?;
            tx.commit().await?;
            return Ok(standing);
        }
        if current.status != "paid" {
            return Err(WalletError::Conflict(format!(
                "a {} payment order cannot fulfill",
                current.status
            )));
        }
        let outbox_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.payment_outbox(outbox_id,order_id,event_type,payload) \
             VALUES($1,$2,$3,$4)",
        )
        .bind(outbox_id)
        .bind(order_id)
        .bind(format!(
            "payment_order.{}_fulfilled",
            current.purpose.as_str()
        ))
        .bind(&payload)
        .execute(&mut *tx)
        .await?;
        let fulfillment_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.payment_fulfillments\
             (fulfillment_id,order_id,purpose,purpose_payload,outbox_id) \
             VALUES($1,$2,$3,$4,$5) ON CONFLICT (order_id) DO NOTHING",
        )
        .bind(fulfillment_id)
        .bind(order_id)
        .bind(current.purpose.as_str())
        .bind(&payload)
        .bind(outbox_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.payment_orders SET status='fulfilled',updated_at=now() \
             WHERE order_id=$1 AND status='paid'",
        )
        .bind(order_id)
        .execute(&mut *tx)
        .await?;
        let standing = fulfillment_in(&mut tx, order_id)
            .await?
            .ok_or_else(|| WalletError::Storage(sqlx::Error::RowNotFound.into()))?;
        tx.commit().await?;
        Ok(standing)
    }

    /// The purpose-scoped grant [`Self::fulfill_payment_order`] runs: the one
    /// arm an order's purpose permits, dispatched exhaustively so no order may
    /// both grant quota and wallet cash.
    ///
    /// - `wallet_topup` books wallet cash via [`crate::Wallet::top_up`] — the
    ///   purchased pool only; it can repay a drawn line but never grants quota.
    /// - `bundle` books the purchased credit the same way, then the separate
    ///   promo lot as a `bundle_promo` `promo_lots` row plus its `debit
    ///   Adjustments / credit Promo` entry — two postings under one
    ///   fulfillment, the lot never cash-backed and never refundable (issue
    ///   #609).
    /// - `plan_period` activates the period via [`Db::activate_plan_period`] —
    ///   the quota grant — and never touches the wallet.
    /// - `invoice_repayment` repays the drawn line via
    ///   [`crate::Wallet::repay_credit`] — `debit Cash / credit CreditLine` —
    ///   so not a minor of it can become spendable wallet balance.
    ///
    /// `wallet` is the order organization's ledger; bundle quantities and expiry
    /// come only from its authoritative snapshot. The returned value is the
    /// `purpose_payload` the fulfillment records — what was granted, as audit.
    pub(crate) async fn grant_order(
        &self,
        order: &PaymentOrder,
        wallet: &crate::Wallet,
        on: time::Date,
    ) -> Result<serde_json::Value, WalletError> {
        // The ledger idempotency keys derive from the order id, so a retried or
        // recovered fulfillment replays the same grant rather than granting
        // twice — the recovery sweep's "single credit" guarantee.
        let key = format!("payment-order:{}", order.order_id);
        match order.purpose {
            PaymentOrderPurpose::WalletTopup => {
                let receipt = wallet.top_up(&key, order.amount_minor, on).await?;
                self.reconcile_statements(order.organization_id, wallet, on)
                    .await?;
                Ok(json!({
                    "arm": "wallet_topup",
                    "walletEntryId": receipt.entry_id.as_uuid(),
                    "creditedMinor": order.amount_minor,
                }))
            }
            PaymentOrderPurpose::Bundle => {
                let offer = order.bundle_offer.as_ref().ok_or_else(|| {
                    WalletError::Conflict(
                        "legacy bundle has no authoritative offer; operator review required".into(),
                    )
                })?;
                offer.terms.validate()?;
                let rate = offer.terms.rate()?;
                if order.purpose_ref != Some(offer.bundle_version_id)
                    || order.amount_minor != offer.terms.purchased_minor
                    || order.promo_minor != offer.terms.promo_minor
                    || order.display_amount_minor != offer.terms.display_amount_minor
                    || order.display_currency != offer.terms.display_currency
                    || order.rate_mantissa != rate.mantissa
                    || order.rate_scale != rate.scale
                {
                    return Err(WalletError::Conflict(
                        "bundle order snapshot does not match its grant".into(),
                    ));
                }
                let paid_at = order
                    .paid_at
                    .ok_or_else(|| invalid("a bundle grant requires confirmed payment"))?;
                let expires_at = offer
                    .terms
                    .promo_lifetime_days
                    .map(|days| paid_at.checked_add(time::Duration::days(i64::from(days))))
                    .map(|value| value.ok_or_else(|| invalid("bundle expiry overflows")))
                    .transpose()?;
                let promo_minor = offer.terms.promo_minor;
                let purchased = wallet.top_up(&key, order.amount_minor, on).await?;
                // The promo leg is a lot, not an anonymous bonus grant: the
                // `bundle_promo` row names the fulfillment that issued it and
                // carries the refund/cancellation semantics a bare adjustment
                // cannot — expiry burns it, a refund cancels it, and neither
                // ever returns it to refundable cash (issue #609). The ledger
                // key derives from the order like the purchased leg's does, so
                // a replayed fulfillment finds the standing lot and posts no
                // second grant.
                let mut promo_lot = None;
                if promo_minor > 0 {
                    // The sold lifetime begins at the immutable paid instant;
                    // retries cannot grant a fresh promotional lifetime.
                    promo_lot = Some(
                        self.grant_bundle_promotion(wallet, order, expires_at, on)
                            .await?,
                    );
                }
                self.reconcile_statements(order.organization_id, wallet, on)
                    .await?;
                Ok(json!({
                    "arm": "bundle",
                    "walletEntryId": purchased.entry_id.as_uuid(),
                    "creditedMinor": order.amount_minor,
                    "promoLotId": promo_lot,
                    "promoMinor": promo_minor,
                }))
            }
            PaymentOrderPurpose::PlanPeriod => {
                let customer_plan = order
                    .purpose_ref
                    .ok_or_else(|| invalid("a plan period order carries no customer plan"))?;
                let plan = self
                    .activate_plan_period(order.order_id, customer_plan)
                    .await?;
                Ok(json!({
                    "arm": "plan_period",
                    "customerPlanId": plan.customer_plan_id,
                    "subscriptionId": plan.subscription_id,
                    "planVersionId": plan.plan_version_id,
                }))
            }
            PaymentOrderPurpose::InvoiceRepayment => {
                let receipt = wallet.repay_credit(&key, order.amount_minor, on).await?;
                self.reconcile_statements(order.organization_id, wallet, on)
                    .await?;
                Ok(json!({
                    "arm": "invoice_repayment",
                    "walletEntryId": receipt.entry_id.as_uuid(),
                    "repaidMinor": order.amount_minor,
                    "receivable": order.purpose_ref,
                }))
            }
        }
    }
}
