//! Scoped application checkout authority inside the payment order transaction.
use super::checkout_types::{
    ApplicationCheckoutRequest, ApplicationCheckoutSelection, ApplicationCheckoutTerms,
};
use crate::{
    ActingService, CheckoutChoice, Db, IntegrationScope, MerchantSigningContext, NewPaymentOrder,
    PaymentOrder, PaymentOrderPage, PaymentOrderPurpose, SecretKey, WalletError, business_staff,
    integration_credentials,
};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use url::{Host, Url};
use uuid::Uuid;

const MAX_EXACT_INTEGER: i64 = 9_007_199_254_740_991;

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}

pub(crate) async fn authorize_in(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    org: Uuid,
    active: bool,
) -> Result<Uuid, WalletError> {
    integration_credentials::require_integration(
        tx,
        service,
        IntegrationScope::CheckoutWrite,
        Some(org),
    )
    .await?;
    let app = service
        .application_id
        .ok_or_else(|| WalletError::Forbidden("application credential required".into()))?;
    if active {
        let state: String = sqlx::query_scalar(
            "SELECT state FROM oxsum.customer_profiles \
             WHERE organization_id=$1 AND business_id=$2 FOR SHARE",
        )
        .bind(org)
        .bind(service.business_id)
        .fetch_one(&mut **tx)
        .await?;
        if state != "active" {
            return Err(WalletError::Conflict(
                "archived account cannot initiate or resume checkout".into(),
            ));
        }
    }
    Ok(app)
}

fn validate_url(value: &str) -> Result<(), WalletError> {
    if value.len() > 2000
        || value.trim() != value
        || value.contains('\\')
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || !(value.starts_with("https://") || value.starts_with("http://"))
    {
        return Err(invalid("invalid checkout return destination"));
    }
    let url = Url::parse(value).map_err(|_| invalid("invalid checkout return destination"))?;
    let loopback = match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost",
        None => false,
    };
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(invalid(
            "checkout returns require absolute HTTPS or loopback HTTP without credentials/fragments",
        ));
    }
    Ok(())
}

fn validate(request: &ApplicationCheckoutRequest) -> Result<(), WalletError> {
    business_staff::validate(&request.idempotency_key, None)?;
    validate_url(&request.return_url)?;
    validate_url(&request.cancel_url)
}

fn operation_key(app: Uuid, key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"oxsum/application-checkout/v1\0");
    digest.update(key.as_bytes());
    format!(
        "application-checkout/{app}/{}",
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

pub(crate) async fn quote_in(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    org: Uuid,
    selection: &ApplicationCheckoutSelection,
) -> Result<ApplicationCheckoutTerms, WalletError> {
    let app = authorize_in(tx, service, org, true).await?;
    let terms = crate::collection_terms::terms_in(tx, service.business_id).await?;
    if !terms.ready {
        return Err(WalletError::Conflict(
            "business collection is not ready".into(),
        ));
    }
    let currency = terms
        .currency
        .ok_or_else(|| invalid("collection currency missing"))?;
    let route = crate::merchant_routes::route_in(tx, service.business_id, selection.rail).await?;
    if !route.available {
        return Err(WalletError::Conflict("checkout route unavailable".into()));
    }
    let checkout = CheckoutChoice {
        revision: route.revision,
        merchant_id: route
            .merchant_id
            .ok_or_else(|| invalid("merchant missing"))?,
        version_id: route
            .version_id
            .ok_or_else(|| invalid("merchant version missing"))?,
    };
    let (display, purchased, promo) = match selection.purpose {
        PaymentOrderPurpose::WalletTopup if selection.bundle_version_id.is_none() => {
            let amount = selection
                .display_amount_minor
                .filter(|v| (1..=MAX_EXACT_INTEGER).contains(v))
                .ok_or_else(|| invalid("select a positive bounded fiat amount"))?;
            let purchased = currency
                .display()?
                .to_credit_minor(amount)
                .filter(|v| (1..=MAX_EXACT_INTEGER).contains(v))
                .ok_or_else(|| invalid("converted amount outside exact range"))?;
            (amount, purchased, 0)
        }
        PaymentOrderPurpose::Bundle if selection.display_amount_minor.is_none() => {
            let version = selection
                .bundle_version_id
                .ok_or_else(|| invalid("published bundle version required"))?;
            let bundle = crate::bundles::available_in(tx, service.business_id, version).await?;
            if bundle.terms.display_currency != currency.code {
                return Err(WalletError::Conflict(
                    "bundle currency differs from collection".into(),
                ));
            }
            (
                bundle.terms.display_amount_minor,
                bundle.terms.purchased_minor,
                bundle.terms.promo_minor,
            )
        }
        _ => {
            return Err(invalid(
                "this application checkout supports wallet top-up or published bundle only",
            ));
        }
    };
    Ok(ApplicationCheckoutTerms {
        application_id: app,
        organization_id: org,
        selection: selection.clone(),
        checkout,
        merchant_mode: route.mode.ok_or_else(|| invalid("merchant mode missing"))?,
        merchant_name: route.name.ok_or_else(|| invalid("merchant name missing"))?,
        collection_revision: terms.revision,
        display_currency: currency.code,
        display_amount_minor: display,
        purchased_minor: purchased,
        promo_minor: promo,
    })
}

/// Called before payment-order replay; exact request and ownership cannot drift.
pub(crate) async fn guard_in(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    org: Uuid,
    request: &ApplicationCheckoutRequest,
    new: &NewPaymentOrder,
) -> Result<(), WalletError> {
    validate(request)?;
    let app = authorize_in(tx, service, org, true).await?;
    if request.quote.application_id != app
        || request.quote.organization_id != org
        || operation_key(app, &request.idempotency_key) != new.idempotency_key
    {
        return Err(WalletError::NotFound(
            "checkout customer or application differs".into(),
        ));
    }
    let prior = sqlx::query(
        "SELECT a.request,a.application_id FROM oxsum.payment_orders p \
         LEFT JOIN oxsum.application_checkout_orders a USING(order_id) \
         WHERE p.organization_id=$1 AND p.idempotency_key=$2",
    )
    .bind(org)
    .bind(&new.idempotency_key)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(prior) = prior {
        if prior.try_get::<Option<Uuid>, _>("application_id")? != Some(app)
            || prior.try_get::<Option<serde_json::Value>, _>("request")?
                != Some(serde_json::json!(request))
        {
            return Err(WalletError::Conflict(
                "checkout key already names different application terms".into(),
            ));
        }
    } else if quote_in(tx, service, org, &request.quote.selection).await? != request.quote {
        return Err(WalletError::Conflict(
            "checkout quote changed; review current terms".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn record_in(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    order: &PaymentOrder,
    request: &ApplicationCheckoutRequest,
) -> Result<(), WalletError> {
    if order.amount_minor != request.quote.purchased_minor
        || order.promo_minor != request.quote.promo_minor
    {
        return Err(WalletError::Conflict(
            "order grant differs from the accepted application quote".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO oxsum.application_checkout_orders \
         (order_id,application_id,organization_id,credential_id,request) \
         VALUES($1,$2,$3,$4,$5)",
    )
    .bind(order.order_id)
    .bind(service.application_id)
    .bind(order.organization_id)
    .bind(service.credential_id)
    .bind(serde_json::json!(request))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Db {
    /// Read exact application checkout terms without provider I/O or writes.
    pub async fn application_checkout_quote(
        &self,
        service: &ActingService,
        org: Uuid,
        selection: &ApplicationCheckoutSelection,
    ) -> Result<ApplicationCheckoutTerms, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        let quote = quote_in(&mut tx, service, org, selection).await?;
        tx.commit().await?;
        Ok(quote)
    }
    /// Create one durable application checkout using transaction-owned authority.
    pub async fn create_application_checkout(
        &self,
        service: &ActingService,
        org: Uuid,
        request: &ApplicationCheckoutRequest,
        secret: &SecretKey,
    ) -> Result<(PaymentOrder, MerchantSigningContext), WalletError> {
        validate(request)?;
        let app = service
            .application_id
            .ok_or_else(|| WalletError::Forbidden("application credential required".into()))?;
        // A preflight avoids disclosing route/history to an unauthorized caller;
        // guard_in repeats authority under the actual order transaction lock.
        let mut tx = self.pool().begin().await?;
        authorize_in(&mut tx, service, org, true).await?;
        tx.commit().await?;
        let q = &request.quote;
        let new = NewPaymentOrder {
            expected_plan_quote: None,
            expected_collection_revision: Some(q.collection_revision),
            expected_statement_quote: None,
            statement_amount_minor: None,
            purpose: q.selection.purpose,
            display_amount_minor: q.display_amount_minor,
            display_currency: q.display_currency.clone(),
            purpose_ref: q.selection.bundle_version_id,
            idempotency_key: operation_key(app, &request.idempotency_key),
            promo_minor: 0,
        };
        self.create_routed_payment_order_authorized(
            org,
            q.selection.rail,
            Some(&q.checkout),
            secret,
            &new,
            Some((service, request)),
        )
        .await
    }
    /// Read this application's exact order and retained destinations under current authority.
    pub async fn application_checkout_order(
        &self,
        service: &ActingService,
        org: Uuid,
        order: Uuid,
        resume: bool,
    ) -> Result<(PaymentOrder, ApplicationCheckoutRequest), WalletError> {
        let mut tx = self.pool().begin().await?;
        let app = authorize_in(&mut tx, service, org, resume).await?;
        let row = sqlx::query(
            "SELECT p.*,a.request AS application_request \
             FROM oxsum.application_checkout_orders a \
             JOIN oxsum.payment_orders p USING(order_id) \
             WHERE a.application_id=$1 AND a.organization_id=$2 AND a.order_id=$3",
        )
        .bind(app)
        .bind(org)
        .bind(order)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("application checkout not found".into()))?;
        let order = crate::payment_orders::order(&row)?;
        let request = serde_json::from_value(row.try_get("application_request")?)
            .map_err(|_| invalid("retained checkout request invalid"))?;
        tx.commit().await?;
        Ok((order, request))
    }
    /// Retained return destinations for an already-authorized order driver.
    /// This does not authenticate a caller; HTTP routes must resolve the owned
    /// order first. Portal recovery also uses these original application URLs.
    pub async fn application_checkout_destinations(
        &self,
        organization: Uuid,
        order: Uuid,
    ) -> Result<Option<(String, String)>, WalletError> {
        let value: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT request FROM oxsum.application_checkout_orders \
             WHERE organization_id=$1 AND order_id=$2",
        )
        .bind(organization)
        .bind(order)
        .fetch_optional(self.pool())
        .await?;
        let Some(value) = value else { return Ok(None) };
        let request: ApplicationCheckoutRequest = serde_json::from_value(value)
            .map_err(|_| invalid("retained application checkout is invalid"))?;
        validate(&request)?;
        Ok(Some((request.return_url, request.cancel_url)))
    }

    /// Bounded identifier page containing only application-created customer orders.
    pub async fn application_checkout_orders(
        &self,
        service: &ActingService,
        org: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<PaymentOrderPage, WalletError> {
        if !(1..=50).contains(&limit) {
            return Err(invalid("limit must be 1 to 50"));
        }
        let mut tx = self.pool().begin().await?;
        let app = authorize_in(&mut tx, service, org, false).await?;
        let rows = sqlx::query(
            "SELECT p.* FROM oxsum.application_checkout_orders a \
             JOIN oxsum.payment_orders p USING(order_id) \
             WHERE a.application_id=$1 AND a.organization_id=$2 \
             AND ($3::uuid IS NULL OR a.order_id>$3) ORDER BY a.order_id LIMIT $4",
        )
        .bind(app)
        .bind(org)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut orders = rows
            .iter()
            .map(crate::payment_orders::order)
            .collect::<Result<Vec<_>, _>>()?;
        let more = orders.len() > limit as usize;
        orders.truncate(limit as usize);
        let next_cursor = if more {
            orders.last().map(|o| o.order_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(PaymentOrderPage {
            orders,
            next_cursor,
        })
    }
}
