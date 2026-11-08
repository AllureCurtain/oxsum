//! Authoritative business collection conversion, independent of display preferences.
use crate::{BusinessCapability, Db, Display, WalletError, business_staff as authority};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Exact collection currency and currency-major amount paid for one credit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionCurrency {
    /// Uppercase ISO-shaped currency code; current rails use two minor digits.
    pub code: String,
    /// Positive decimal text, with at most eight fractional digits.
    pub rate_per_credit: String,
}
impl CollectionCurrency {
    /// Validate bounded exact arithmetic before saving or quoting a payment.
    pub fn display(&self) -> Result<Display, WalletError> {
        if self.code.len() != 3 || self.rate_per_credit.len() > 30 {
            return Err(WalletError::InvalidInput(
                "invalid collection currency or rate".into(),
            ));
        }
        let display =
            crate::display::display_currency(Some(&self.code), Some(&self.rate_per_credit), 2)?
                .ok_or_else(|| {
                    WalletError::InvalidInput("collection currency and rate are required".into())
                })?;
        if display.rate_mantissa() > i128::from(i64::MAX)
            || self.rate_per_credit.ends_with('.')
            || self.rate_per_credit.trim() != self.rate_per_credit
        {
            return Err(WalletError::InvalidInput(
                "collection rate is outside the exact supported range".into(),
            ));
        }
        Ok(display)
    }
}

/// Current settings plus the immutable history that constrains their currency.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionTerms {
    /// Business owning this policy.
    pub business_id: Uuid,
    /// Zero means no policy has been adopted yet.
    pub revision: i64,
    /// `unconfigured`, `environment` or `owner`.
    pub source: String,
    /// Effective operator policy, absent before explicit adoption.
    pub currency: Option<CollectionCurrency>,
    /// Existing intent/payment currencies; these records are never relabeled.
    pub historical_currencies: Vec<String>,
    /// Configured and compatible with history; not a provider certification.
    pub ready: bool,
    /// Last adopted revision time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}
/// Owner's complete intent, retaining the same key across unchanged retries.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionTermsRequest {
    /// Replay identity, scoped to the current actor and business.
    pub idempotency_key: String,
    /// Required reason recorded with the change.
    pub reason: String,
    /// Revision observed when this draft was opened.
    pub expected_revision: i64,
    /// New currency/rate; currency changes are constrained by history.
    pub currency: CollectionCurrency,
}
/// Immutable result returned on replay even after another revision is saved.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionTermsMutation {
    /// Business audit mutation identifier.
    pub id: Uuid,
    /// Exact settings at the successful mutation.
    pub terms: CollectionTerms,
}

async fn historical_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
) -> Result<Vec<String>, WalletError> {
    Ok(sqlx::query_scalar("SELECT currency FROM (SELECT display_currency AS currency FROM oxsum.payment_orders WHERE business_id=$1 UNION SELECT b.currency FROM oxsum.merchant_payment_bindings b JOIN oxsum.merchant_accounts m USING(merchant_id) WHERE m.business_id=$1 UNION SELECT 'USD' FROM oxsum.deposits d JOIN oxsum.customer_profiles c USING(organization_id) WHERE c.business_id=$1 AND d.rail IN ('stripe','hyperswitch','airwallex') AND NOT EXISTS(SELECT 1 FROM oxsum.merchant_payment_bindings b WHERE b.payment_id=d.deposit_id)) currencies ORDER BY currency")
        .bind(business).fetch_all(&mut **tx).await?)
}
pub(crate) async fn terms_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
) -> Result<CollectionTerms, WalletError> {
    let row = sqlx::query("SELECT * FROM oxsum.collection_terms WHERE business_id=$1")
        .bind(business)
        .fetch_optional(&mut **tx)
        .await?;
    let history = historical_in(tx, business).await?;
    let Some(row) = row else {
        return Ok(CollectionTerms {
            business_id: business,
            revision: 0,
            source: "unconfigured".into(),
            currency: None,
            historical_currencies: history,
            ready: false,
            updated_at: None,
        });
    };
    let currency = CollectionCurrency {
        code: row.try_get("currency")?,
        rate_per_credit: row.try_get("rate_per_credit")?,
    };
    let ready = currency.display().is_ok() && history.iter().all(|code| *code == currency.code);
    Ok(CollectionTerms {
        business_id: business,
        revision: row.try_get("revision")?,
        source: row.try_get("source")?,
        currency: Some(currency),
        historical_currencies: history,
        ready,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Call under the business lock before committing a new payment intent.
pub(crate) async fn rate_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    code: &str,
    expected: Option<i64>,
) -> Result<Display, WalletError> {
    let terms = terms_in(tx, business).await?;
    if expected.is_some_and(|revision| revision != terms.revision) {
        return Err(WalletError::Conflict(
            "collection settings changed; review the current quote before creating a new order"
                .into(),
        ));
    }
    if !terms.ready {
        return Err(WalletError::Conflict(
            "collection terms are unconfigured or conflict with retained currency history".into(),
        ));
    }
    let currency = terms
        .currency
        .ok_or_else(|| WalletError::Conflict("collection terms are unconfigured".into()))?;
    if currency.code != code {
        return Err(WalletError::Conflict(
            "requested currency differs from the business collection policy".into(),
        ));
    }
    currency.display()
}

/// Legacy/generic bindings retain their units but cannot contradict an adopted
/// collection policy. Their caller holds the business lock through insertion.
pub(crate) async fn guard_currency_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    code: &str,
) -> Result<(), WalletError> {
    let terms = terms_in(tx, business).await?;
    if terms.currency.as_ref().is_some_and(|c| c.code != code)
        || terms.historical_currencies.iter().any(|c| c != code)
    {
        return Err(WalletError::Conflict(
            "payment currency conflicts with retained collection history".into(),
        ));
    }
    Ok(())
}

impl Db {
    /// Read effective collection terms; no credential material is included.
    pub async fn collection_terms(&self) -> Result<CollectionTerms, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        terms_in(&mut tx, business).await
    }
    /// Read under the current staff session and business context.
    pub async fn business_collection_terms(
        &self,
        token: &str,
    ) -> Result<CollectionTerms, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::OverviewRead)
            .await?;
        terms_in(&mut tx, business).await
    }
    /// Adopt the deployment default once. Existing stored policy wins; conflicting
    /// historical currencies remain visible and block new collection.
    pub async fn bootstrap_collection_terms(
        &self,
        currency: &CollectionCurrency,
    ) -> Result<(), WalletError> {
        currency.display()?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let changed=sqlx::query("INSERT INTO oxsum.collection_terms(business_id,revision,currency,rate_per_credit,source) VALUES($1,1,$2,$3,'environment') ON CONFLICT DO NOTHING")
            .bind(business).bind(&currency.code).bind(&currency.rate_per_credit).execute(&mut *tx).await?.rows_affected();
        if changed == 1 {
            let terms = terms_in(&mut tx, business).await?;
            sqlx::query("INSERT INTO oxsum.collection_term_revisions(business_id,revision,result) VALUES($1,1,$2)")
                .bind(business).bind(serde_json::json!(terms)).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    /// Save current owner's terms and immutable audit under one transaction.
    pub async fn set_collection_terms(
        &self,
        token: &str,
        request: &CollectionTermsRequest,
    ) -> Result<CollectionTermsMutation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        request.currency.display()?;
        if !(0..9_007_199_254_740_991).contains(&request.expected_revision) {
            return Err(WalletError::InvalidInput(
                "expectedRevision is outside the supported range".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::CollectionWrite,
        )
        .await?;
        let action = "business.collection.update";
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
            let result: serde_json::Value = sqlx::query_scalar(
                "SELECT result FROM oxsum.collection_term_revisions WHERE mutation_id=$1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            return serde_json::from_value(result).map_err(|_| {
                WalletError::Misconfigured("invalid collection mutation receipt".into())
            });
        }
        let before = terms_in(&mut tx, business).await?;
        if before.revision != request.expected_revision {
            return Err(WalletError::Conflict(
                "collection settings changed; reload before revising the draft".into(),
            ));
        }
        if before
            .historical_currencies
            .iter()
            .any(|code| *code != request.currency.code)
        {
            return Err(WalletError::Conflict(
                "currency is pinned by payment history and cannot be relabeled".into(),
            ));
        }
        sqlx::query("INSERT INTO oxsum.collection_terms(business_id,revision,currency,rate_per_credit,source) VALUES($1,1,$2,$3,'owner') ON CONFLICT(business_id) DO UPDATE SET revision=oxsum.collection_terms.revision+1,currency=EXCLUDED.currency,rate_per_credit=EXCLUDED.rate_per_credit,source='owner',updated_at=now()")
            .bind(business).bind(&request.currency.code).bind(&request.currency.rate_per_credit).execute(&mut *tx).await?;
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let result = CollectionTermsMutation {
            id: receipt.id,
            terms: terms_in(&mut tx, business).await?,
        };
        sqlx::query("INSERT INTO oxsum.collection_term_revisions(business_id,revision,mutation_id,result) VALUES($1,$2,$3,$4)")
            .bind(business).bind(result.terms.revision).bind(result.id).bind(serde_json::json!(result)).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }
}
