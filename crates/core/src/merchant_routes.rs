//! Explicit business checkout routing, separate from retained callback identity.

use crate::{
    BusinessCapability, Db, MerchantMode, MerchantRail, MerchantSigningContext, NewPaymentOrder,
    PaymentOrder, SecretKey, WalletError, business_staff as authority,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

/// Customer-visible current choice; optional precondition for a new order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckoutChoice {
    /// Explicit route revision; zero is the compatibility bootstrap.
    pub revision: i64,
    /// Selected merchant, never a credential.
    pub merchant_id: Uuid,
    /// Exact current immutable credential version.
    pub version_id: Uuid,
}

/// Safe route metadata; contains no provider endpoint or account identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutRoute {
    /// Rail selected by the customer.
    pub rail: MerchantRail,
    /// Explicit selection revision; zero before owner adoption.
    pub revision: i64,
    /// `owner`, `environment` or `unconfigured`.
    pub source: String,
    /// Selected merchant, absent for an explicit disabled route.
    pub merchant_id: Option<Uuid>,
    /// Current version, absent for an unconfigured route.
    pub version_id: Option<Uuid>,
    /// Business-chosen public name.
    pub name: Option<String>,
    /// Test/live choice that the customer can inspect before paying.
    pub mode: Option<MerchantMode>,
    /// Whether the selected merchant is currently enabled, not certification.
    pub available: bool,
}
impl CheckoutRoute {
    fn choice(&self) -> Result<CheckoutChoice, WalletError> {
        if !self.available {
            return Err(WalletError::Conflict(
                "the requested checkout route is unavailable".into(),
            ));
        }
        Ok(CheckoutChoice {
            revision: self.revision,
            merchant_id: self.merchant_id.ok_or_else(unavailable)?,
            version_id: self.version_id.ok_or_else(unavailable)?,
        })
    }
}
fn unavailable() -> WalletError {
    WalletError::Conflict("the selected merchant is unavailable".into())
}

/// Audited owner intent; null merchant explicitly disables one rail.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckoutRouteRequest {
    /// Stable key for unchanged retries.
    pub idempotency_key: String,
    /// Required audit reason.
    pub reason: String,
    /// Rail whose selection changes.
    pub rail: MerchantRail,
    /// Last observed selection revision.
    pub expected_revision: i64,
    /// Enabled merchant to select; null disables the route.
    pub merchant_id: Option<Uuid>,
}
/// Immutable audit receipt and route snapshot.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutRouteMutation {
    /// Business mutation identity.
    pub id: Uuid,
    /// Selection snapshot at mutation time.
    pub route: CheckoutRoute,
}

pub(crate) async fn route_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    rail: MerchantRail,
) -> Result<CheckoutRoute, WalletError> {
    let selected = sqlx::query("SELECT revision,merchant_id FROM oxsum.merchant_checkout_routes WHERE business_id=$1 AND rail=$2")
        .bind(business).bind(rail.as_str()).fetch_optional(&mut **tx).await?;
    let (revision, merchant, mut source) = if let Some(row) = selected {
        (
            row.try_get("revision")?,
            row.try_get::<Option<Uuid>, _>("merchant_id")?,
            "owner",
        )
    } else {
        let merchant: Option<Uuid> = sqlx::query_scalar("SELECT e.merchant_id FROM oxsum.merchant_environment_bootstrap e JOIN oxsum.merchant_accounts m USING(merchant_id) WHERE m.business_id=$1 AND e.rail=$2")
            .bind(business).bind(rail.as_str()).fetch_optional(&mut **tx).await?;
        (0, merchant, "environment")
    };
    if merchant.is_none() && revision == 0 {
        source = "unconfigured";
    }
    let row = sqlx::query("SELECT current_version_id,name,mode,enabled FROM oxsum.merchant_accounts WHERE merchant_id=$1 AND business_id=$2 AND rail=$3")
        .bind(merchant).bind(business).bind(rail.as_str()).fetch_optional(&mut **tx).await?;
    let mut result = CheckoutRoute {
        rail,
        revision,
        source: source.into(),
        merchant_id: merchant,
        version_id: None,
        name: None,
        mode: None,
        available: false,
    };
    if let Some(row) = row {
        result.version_id = Some(row.try_get("current_version_id")?);
        result.name = Some(row.try_get("name")?);
        result.mode = Some(MerchantMode::parse(&row.try_get::<String, _>("mode")?)?);
        result.available = row.try_get("enabled")?;
    }
    Ok(result)
}

async fn routes_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
) -> Result<Vec<CheckoutRoute>, WalletError> {
    let mut routes = Vec::new();
    for rail in [
        MerchantRail::Hyperswitch,
        MerchantRail::Stripe,
        MerchantRail::Airwallex,
    ] {
        routes.push(route_in(tx, business, rail).await?);
    }
    Ok(routes)
}

/// Recheck after idempotent-order recovery, under the order's business lock.
pub(crate) async fn guard_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    rail: MerchantRail,
    choice: &CheckoutChoice,
) -> Result<(), WalletError> {
    if route_in(tx, business, rail).await?.choice()? != *choice {
        return Err(WalletError::Conflict(
            "checkout selection changed; review the current payment options".into(),
        ));
    }
    Ok(())
}

impl Db {
    /// Read all rail selections under live ReportsRead business authority.
    pub async fn business_checkout_routes(
        &self,
        token: &str,
    ) -> Result<Vec<CheckoutRoute>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        routes_in(&mut tx, business).await
    }

    /// Select or disable one rail; an unchanged key returns its original receipt.
    pub async fn set_checkout_route(
        &self,
        token: &str,
        request: &CheckoutRouteRequest,
    ) -> Result<CheckoutRouteMutation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        if !(0..9_007_199_254_740_991).contains(&request.expected_revision) {
            return Err(WalletError::InvalidInput(
                "invalid checkout route revision".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::MerchantSecretsWrite,
        )
        .await?;
        let detail = json!({"rail":request.rail,"merchantId":request.merchant_id,"expectedRevision":request.expected_revision,"reason":request.reason});
        let action = "business.checkout_route.select";
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            let route = sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT route FROM oxsum.merchant_checkout_route_results WHERE mutation_id=$1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            return Ok(CheckoutRouteMutation {
                id,
                route: serde_json::from_value(route).map_err(|_| unavailable())?,
            });
        }
        let current = route_in(&mut tx, business, request.rail).await?;
        if current.revision != request.expected_revision {
            return Err(WalletError::Conflict(
                "checkout route revision changed".into(),
            ));
        }
        if let Some(merchant) = request.merchant_id {
            let allowed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_accounts WHERE merchant_id=$1 AND business_id=$2 AND rail=$3 AND enabled)")
                .bind(merchant).bind(business).bind(request.rail.as_str()).fetch_one(&mut *tx).await?;
            if !allowed {
                return Err(unavailable());
            }
        }
        sqlx::query("INSERT INTO oxsum.merchant_checkout_routes(business_id,rail,revision,merchant_id) VALUES($1,$2,$3,$4) ON CONFLICT(business_id,rail) DO UPDATE SET revision=EXCLUDED.revision,merchant_id=EXCLUDED.merchant_id,updated_at=now()")
            .bind(business).bind(request.rail.as_str()).bind(current.revision+1).bind(request.merchant_id).execute(&mut *tx).await?;
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let route = route_in(&mut tx, business, request.rail).await?;
        sqlx::query(
            "INSERT INTO oxsum.merchant_checkout_route_results(mutation_id,route) VALUES($1,$2)",
        )
        .bind(receipt.id)
        .bind(json!(route))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(CheckoutRouteMutation {
            id: receipt.id,
            route,
        })
    }

    /// Available safe choices for this customer's own business.
    pub async fn customer_checkout_options(
        &self,
        organization: Uuid,
    ) -> Result<Vec<CheckoutRoute>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.customer_profiles WHERE business_id=$1 AND organization_id=$2)")
            .bind(business).bind(organization).fetch_one(&mut *tx).await?;
        if !owned {
            return Err(WalletError::NotFound("unknown customer".into()));
        }
        Ok(routes_in(&mut tx, business)
            .await?
            .into_iter()
            .filter(|route| route.available)
            .collect())
    }

    /// Pin a selected route or recover an existing order's exact signing version.
    /// No provider I/O occurs until the caller receives this committed order.
    pub async fn create_routed_payment_order(
        &self,
        organization: Uuid,
        rail: MerchantRail,
        expected: Option<&CheckoutChoice>,
        secret: &SecretKey,
        new: &NewPaymentOrder,
    ) -> Result<(PaymentOrder, MerchantSigningContext), WalletError> {
        let existing = sqlx::query("SELECT merchant_id,merchant_version_id FROM oxsum.payment_orders WHERE organization_id=$1 AND idempotency_key=$2")
            .bind(organization).bind(&new.idempotency_key).fetch_optional(self.pool()).await?;
        let (merchant, version, guard) = if let Some(row) = existing {
            let merchant: Option<Uuid> = row.try_get("merchant_id")?;
            let version: Option<Uuid> = row.try_get("merchant_version_id")?;
            (
                merchant.ok_or_else(unavailable)?,
                version.ok_or_else(unavailable)?,
                None,
            )
        } else {
            let choices = self.customer_checkout_options(organization).await?;
            let route = choices
                .into_iter()
                .find(|choice| choice.rail == rail)
                .ok_or_else(unavailable)?;
            let choice = route.choice()?;
            if expected.is_some_and(|value| value != &choice) {
                return Err(WalletError::Conflict(
                    "checkout choice changed; review current options".into(),
                ));
            }
            (choice.merchant_id, choice.version_id, Some(choice))
        };
        let context = self
            .merchant_signing_context(merchant, version, secret)
            .await?
            .ok_or_else(unavailable)?;
        if context.rail != rail {
            return Err(WalletError::Conflict(
                "payment rail changed for the same idempotency key".into(),
            ));
        }
        let order = self
            .create_payment_order_checked(&context, organization, new, guard.as_ref())
            .await?;
        // A concurrent identical create may have committed another retained
        // version before our business lock. Always return the winning version.
        let pinned = self
            .merchant_signing_context(
                order.merchant_id.ok_or_else(unavailable)?,
                order.merchant_version_id.ok_or_else(unavailable)?,
                secret,
            )
            .await?
            .ok_or_else(unavailable)?;
        Ok((order, pinned))
    }
}
