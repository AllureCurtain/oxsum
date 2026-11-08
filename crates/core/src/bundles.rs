//! Published bundle versions own purchased credit, promotional credit and expiry.
use crate::{BusinessCapability, Db, OrderRate, WalletError, business_staff as authority};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

const MAX_SAFE: i64 = 9_007_199_254_740_991;
fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}

/// Complete immutable commercial terms of a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleTerms {
    /// Human-readable name of this sold version.
    pub name: String,
    /// Provider currency, currently two minor digits.
    pub display_currency: String,
    /// Provider amount to collect.
    pub display_amount_minor: i64,
    /// Purchased wallet credit in integer credit minor units.
    pub purchased_minor: i64,
    /// Promotional credit, never refundable cash.
    pub promo_minor: i64,
    /// Promo expiry in days after payment; absent means no expiry.
    pub promo_lifetime_days: Option<i32>,
}
impl BundleTerms {
    /// Validate exact quantities and their representable conversion snapshot.
    pub fn validate(&self) -> Result<(), WalletError> {
        if self.name.trim().is_empty()
            || self.name.len() > 200
            || self.name.chars().any(char::is_control)
            || self.display_currency.len() != 3
            || !self
                .display_currency
                .bytes()
                .all(|c| c.is_ascii_uppercase())
            || !(1..=MAX_SAFE).contains(&self.display_amount_minor)
            || !(1..=MAX_SAFE).contains(&self.purchased_minor)
            || !(0..=MAX_SAFE).contains(&self.promo_minor)
            || self
                .purchased_minor
                .checked_add(self.promo_minor)
                .is_none_or(|sum| sum > MAX_SAFE)
            || self
                .promo_lifetime_days
                .is_some_and(|n| !(1..=3650).contains(&n))
        {
            return Err(invalid(
                "invalid bundle name, amount, currency or promotional lifetime",
            ));
        }
        self.rate()?;
        Ok(())
    }
    /// Exact currency-major amount per credit, in the existing order format.
    pub(crate) fn rate(&self) -> Result<OrderRate, WalletError> {
        if self.purchased_minor <= 0 || self.display_amount_minor <= 0 {
            return Err(invalid("bundle amounts must be positive"));
        }
        let mut numerator = i128::from(self.display_amount_minor) * 10_000;
        for scale in 0..=8 {
            if numerator % i128::from(self.purchased_minor) == 0 {
                let mantissa = numerator / i128::from(self.purchased_minor);
                if mantissa > 0 && mantissa <= i128::from(i64::MAX) {
                    return Ok(OrderRate { mantissa, scale });
                }
            }
            numerator = numerator
                .checked_mul(10)
                .ok_or_else(|| invalid("bundle conversion overflows"))?;
        }
        Err(invalid(
            "bundle price ratio cannot be represented exactly with eight decimal places",
        ))
    }
}
/// A published version. Retirement stops new orders, never existing fulfillment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleOffer {
    /// Identity chosen by a customer's order.
    pub bundle_version_id: Uuid,
    /// Stable identity across versions.
    pub bundle_id: Uuid,
    /// Revision within the stable offer.
    pub version: i32,
    /// Exact sold quantities.
    pub terms: BundleTerms,
    /// Publication time, retained in order snapshots.
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
    /// When new sales ended; older order snapshots retain their original value.
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
}
/// Audited publication under current commercial authority.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundlePublishRequest {
    /// Replay identity for unchanged intent.
    pub idempotency_key: String,
    /// Staff reason.
    pub reason: String,
    /// Existing stable offer, or absent for a new one.
    pub bundle_id: Option<Uuid>,
    /// Last observed version; zero for a new offer.
    pub expected_version: i32,
    /// Complete published terms.
    pub terms: BundleTerms,
}
/// Audited retirement intent.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleRetireRequest {
    /// Replay identity for unchanged intent.
    pub idempotency_key: String,
    /// Staff reason.
    pub reason: String,
}
/// Immutable mutation receipt, including original offer state on replay.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleMutation {
    /// Business audit mutation identity.
    pub id: Uuid,
    /// Offer as recorded by this mutation.
    pub offer: BundleOffer,
}
fn offer(row: &sqlx::postgres::PgRow) -> Result<BundleOffer, WalletError> {
    Ok(BundleOffer {
        bundle_version_id: row.try_get("bundle_version_id")?,
        bundle_id: row.try_get("bundle_id")?,
        version: row.try_get("version")?,
        terms: serde_json::from_value(row.try_get("terms")?)
            .map_err(|_| WalletError::Misconfigured("invalid stored bundle terms".into()))?,
        published_at: row.try_get("published_at")?,
        retired_at: row.try_get("retired_at")?,
    })
}
pub(crate) async fn available_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
) -> Result<BundleOffer, WalletError> {
    let row=sqlx::query("SELECT * FROM oxsum.bundle_offers WHERE bundle_version_id=$1 AND business_id=$2 AND retired_at IS NULL")
        .bind(id).bind(business).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::NotFound("bundle version is not on sale".into()))?;
    let result = offer(&row)?;
    result.terms.validate()?;
    Ok(result)
}
async fn replay_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<BundleMutation, WalletError> {
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT result FROM oxsum.bundle_offer_mutations WHERE mutation_id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    serde_json::from_value(value)
        .map_err(|_| WalletError::Misconfigured("invalid bundle mutation receipt".into()))
}
async fn record_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    actor: Uuid,
    key: &str,
    action: &str,
    detail: &serde_json::Value,
    offer: BundleOffer,
) -> Result<BundleMutation, WalletError> {
    let recorded = authority::record(tx, business, actor, key, action, detail).await?;
    let result = BundleMutation {
        id: recorded.id,
        offer,
    };
    sqlx::query("INSERT INTO oxsum.bundle_offer_mutations(mutation_id,result) VALUES($1,$2)")
        .bind(result.id)
        .bind(serde_json::json!(result))
        .execute(&mut **tx)
        .await?;
    Ok(result)
}
impl Db {
    /// Publish the lot and immediately expire it in the same transaction when
    /// recovery happens beyond the sold lifetime. No caller can spend it between.
    pub(crate) async fn grant_bundle_promotion(
        &self,
        wallet: &crate::Wallet,
        order: &crate::PaymentOrder,
        expires_at: Option<OffsetDateTime>,
        on: time::Date,
    ) -> Result<Uuid, WalletError> {
        let reference = order.order_id.to_string();
        let key = format!(
            "promo-lot:{}:bundle_promo:{reference}",
            order.organization_id
        );
        let mut tx = self.pool().begin().await?;
        let (lot, _) = wallet
            .grant_promo_lot_in(
                &mut tx,
                order.organization_id,
                "bundle_promo",
                &reference,
                &key,
                order.promo_minor,
                expires_at,
                on,
            )
            .await?;
        let due: Option<i64> = sqlx::query_scalar(
            "SELECT remaining_minor FROM oxsum.promo_lots WHERE lot_id=$1 \
             AND expires_at <= now() AND closed_at IS NULL FOR UPDATE",
        )
        .bind(lot)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(remaining) = due {
            if remaining > 0 {
                wallet
                    .burn_promo_in(&mut tx, &format!("promo-expire:{lot}"), remaining, on)
                    .await?;
            }
            sqlx::query("UPDATE oxsum.promo_lots SET remaining_minor=0,closed_at=now(),close_reason='expired' WHERE lot_id=$1")
                .bind(lot).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(lot)
    }
    /// Safe customer catalog, only offers currently accepting new purchases.
    pub async fn bundle_catalog(&self) -> Result<Vec<BundleOffer>, WalletError> {
        let business = self.default_business().await?;
        let rows=sqlx::query("SELECT * FROM oxsum.bundle_offers WHERE business_id=$1 AND retired_at IS NULL ORDER BY published_at,bundle_version_id").bind(business).fetch_all(self.pool()).await?;
        rows.iter().map(offer).collect()
    }
    /// Staff reads complete offer history under current overview authority.
    pub async fn business_bundle_offers(
        &self,
        token: &str,
    ) -> Result<Vec<BundleOffer>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::OverviewRead)
            .await?;
        let rows=sqlx::query("SELECT * FROM oxsum.bundle_offers WHERE business_id=$1 ORDER BY published_at,bundle_version_id").bind(business).fetch_all(&mut *tx).await?;
        let result = rows.iter().map(offer).collect();
        tx.commit().await?;
        result
    }
    /// Publish a version atomically with audit, retry receipt and optimistic revision.
    pub async fn publish_bundle(
        &self,
        token: &str,
        request: &BundlePublishRequest,
    ) -> Result<BundleMutation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        request.terms.validate()?;
        if request.expected_version < 0 || request.expected_version == i32::MAX {
            return Err(invalid("invalid expected bundle version"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::CommercialWrite,
        )
        .await?;
        let action = "business.bundle.publish";
        let detail = serde_json::json!(request);
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
            return replay_in(&mut tx, id).await;
        }
        let id = request.bundle_id.unwrap_or_else(Uuid::new_v4);
        let standing: Option<i32> = sqlx::query_scalar(
            "SELECT MAX(version) FROM oxsum.bundle_offers WHERE business_id=$1 AND bundle_id=$2",
        )
        .bind(business)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if request.bundle_id.is_some() && standing.is_none() {
            return Err(WalletError::NotFound("unknown bundle offer".into()));
        }
        if standing.unwrap_or(0) != request.expected_version {
            return Err(WalletError::Conflict(
                "bundle version changed; reload before publishing".into(),
            ));
        }
        let row=sqlx::query("INSERT INTO oxsum.bundle_offers(bundle_version_id,bundle_id,business_id,version,terms) VALUES($1,$2,$3,$4,$5) RETURNING *")
            .bind(Uuid::new_v4()).bind(id).bind(business).bind(request.expected_version+1).bind(serde_json::json!(request.terms)).fetch_one(&mut *tx).await?;
        let result = record_in(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
            offer(&row)?,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Retire one immutable version; existing payment terms remain authoritative.
    pub async fn retire_bundle(
        &self,
        token: &str,
        id: Uuid,
        request: &BundleRetireRequest,
    ) -> Result<BundleMutation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::CommercialWrite,
        )
        .await?;
        let action = "business.bundle.retire";
        let detail = serde_json::json!({"bundleVersionId":id,"request":request});
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
            return replay_in(&mut tx, id).await;
        }
        let row=sqlx::query("UPDATE oxsum.bundle_offers SET retired_at=COALESCE(retired_at,now()) WHERE bundle_version_id=$1 AND business_id=$2 RETURNING *")
            .bind(id).bind(business).fetch_optional(&mut *tx).await?.ok_or_else(||WalletError::NotFound("unknown bundle version".into()))?;
        let result = record_in(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
            offer(&row)?,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
}
