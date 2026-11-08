//! Actor-scoped configuration facts for a solo pay-as-you-go business.
//!
//! Reads do not probe providers, send mail, reserve money or certify a release.
//! Restricted facts stay explicit instead of becoming completed checklist items.

use std::collections::BTreeSet;

use serde::Serialize;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    BusinessCapability, BusinessRole, Db, SecretKey, WalletError, business_staff as authority,
    collection_terms, pools,
};

/// One required configuration fact and the current actor's next action.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessReadinessStep {
    /// Stable fact name; translated by the portal.
    pub key: &'static str,
    /// `ready`, `todo` or `restricted`; restricted never counts as ready.
    pub state: &'static str,
    /// Existing business section this actor may manage, or no direct action.
    pub action: Option<&'static str>,
}

/// Current configuration readiness, distinct from external acceptance.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessReadiness {
    /// Database instant used for every time-sensitive fact.
    #[serde(with = "time::serde::rfc3339")]
    pub checked_at: OffsetDateTime,
    /// Every required fact is both readable and ready for this actor.
    pub ready: bool,
    /// Profile, supply, sale terms, collection and admission, in that order.
    pub steps: Vec<BusinessReadinessStep>,
}

fn step(
    key: &'static str,
    fact: Option<bool>,
    can_manage: bool,
    section: &'static str,
) -> BusinessReadinessStep {
    BusinessReadinessStep {
        key,
        state: match fact {
            Some(true) => "ready",
            Some(false) => "todo",
            None => "restricted",
        },
        action: can_manage.then_some(section),
    }
}

/// Configured chat names after alias shadowing and credential health. The first
/// customer starts without a tier or key group; later customer-specific access
/// belongs to the separate customer readiness read and atomic admission.
async fn usable_models_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    secret: Option<&SecretKey>,
) -> Result<BTreeSet<String>, WalletError> {
    let Some(secret) = secret else {
        return Ok(BTreeSet::new());
    };
    let rows = sqlx::query(
        "WITH latest AS (SELECT DISTINCT ON (channel_id,model) * \
             FROM oxsum.channel_prices ORDER BY channel_id,model,version DESC), \
         names AS ( \
             SELECT p.channel_id,p.model AS public_model,p.model AS price_model \
             FROM latest p WHERE NOT EXISTS( \
                 SELECT 1 FROM oxsum.model_aliases a WHERE a.alias=p.model) \
             UNION ALL \
             SELECT a.channel_id,a.alias,CASE WHEN EXISTS( \
                 SELECT 1 FROM latest p WHERE p.channel_id=a.channel_id AND p.model=a.alias) \
                 THEN a.alias ELSE a.upstream_model END FROM oxsum.model_aliases a) \
         SELECT c.channel_id,n.public_model FROM names n \
         JOIN latest p ON p.channel_id=n.channel_id AND p.model=n.price_model \
         JOIN oxsum.channels c ON c.channel_id=n.channel_id \
         WHERE c.business_id=$1 AND p.mode='chat' AND p.weight>0 \
             AND c.protocol IN ('openai','anthropic','gemini') \
         ORDER BY c.channel_id,n.public_model",
    )
    .bind(business)
    .fetch_all(&mut **tx)
    .await?;
    let mut models = BTreeSet::new();
    let mut checked_channel = None;
    let mut usable = false;
    for row in rows {
        let channel: Uuid = row.try_get("channel_id")?;
        if checked_channel != Some(channel) {
            let credentials = pools::healthy_credentials(tx, channel).await?;
            // A lost encryption key is an unmet configuration fact. Never
            // return credential bytes or cryptographic errors to this read.
            usable = !credentials.is_empty()
                && credentials
                    .iter()
                    .all(|(_, sealed, _, _)| secret.open(sealed).is_ok());
            checked_channel = Some(channel);
        }
        if usable {
            models.insert(row.try_get("public_model")?);
        }
    }
    Ok(models)
}

async fn sale_terms_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    models: &BTreeSet<String>,
) -> Result<bool, WalletError> {
    let rows = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT d.models FROM oxsum.sale_schedules s \
         LEFT JOIN oxsum.sale_schedule_ends e USING(schedule_id) \
         JOIN oxsum.sale_versions v ON v.business_id=s.business_id \
             AND v.book_id=s.book_id AND v.version=s.version \
         JOIN oxsum.sale_drafts d ON d.book_id=v.book_id AND d.revision=v.draft_revision \
         WHERE s.business_id=$1 AND s.kind='default' AND s.effective_from<=now() \
             AND (COALESCE(e.effective_until,s.scheduled_until) IS NULL \
                 OR now()<COALESCE(e.effective_until,s.scheduled_until))",
    )
    .bind(business)
    .fetch_all(&mut **tx)
    .await?;
    // Missing or ambiguous defaults cannot claim that a new customer is sold
    // a usable model. Published drafts and future schedules alone are not ready.
    let [value] = rows.as_slice() else {
        return Ok(false);
    };
    let terms: crate::SaleModelMap = serde_json::from_value(value.clone())
        .map_err(|_| WalletError::Misconfigured("invalid stored sale terms".into()))?;
    Ok(terms.iter().any(|(model, terms)| {
        models.contains(model) && terms.price.price().mode == crate::BillingMode::Chat
    }))
}

async fn collection_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    secret: Option<&SecretKey>,
    public_url_configured: bool,
) -> Result<bool, WalletError> {
    let Some(secret) = secret else {
        return Ok(false);
    };
    if !public_url_configured || !collection_terms::terms_in(tx, business).await?.ready {
        return Ok(false);
    }
    let credentials = sqlx::query_scalar::<_, String>(
        "SELECT v.credentials_sealed FROM oxsum.merchant_checkout_routes r \
         JOIN oxsum.merchant_accounts m ON m.merchant_id=r.merchant_id \
             AND m.business_id=r.business_id AND m.rail=r.rail \
         JOIN oxsum.merchant_versions v ON v.merchant_id=m.merchant_id \
             AND v.version_id=m.current_version_id \
         WHERE r.business_id=$1 AND m.enabled",
    )
    .bind(business)
    .fetch_all(&mut **tx)
    .await?;
    // An environment bootstrap remains compatible checkout behavior, but the
    // onboarding checklist requires the owner to explicitly adopt a route.
    Ok(credentials.iter().any(|sealed| secret.open(sealed).is_ok()))
}

impl Db {
    /// Read five setup facts under current business authority. No network I/O,
    /// ledger writes or secret values leave this method. A role cannot infer a
    /// restricted section's readiness from the overall `ready` flag.
    pub async fn business_readiness(
        &self,
        token: &str,
        secret: Option<&SecretKey>,
        public_url_configured: bool,
    ) -> Result<BusinessReadiness, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::OverviewRead,
        )
        .await?;
        let row = sqlx::query(
            "SELECT initialized_at IS NOT NULL AND length(btrim(name))>0 AS profile, \
             registration_mode IN ('open','invite') AS admission, now() AS checked_at \
             FROM oxsum.businesses WHERE business_id=$1",
        )
        .bind(business)
        .fetch_one(&mut *tx)
        .await?;
        let can_supply = actor.role.allows(BusinessCapability::CatalogWrite);
        let can_sale = matches!(
            actor.role,
            BusinessRole::Owner | BusinessRole::Admin | BusinessRole::Finance
        );
        let models = if can_supply || can_sale {
            usable_models_in(&mut tx, business, secret).await?
        } else {
            BTreeSet::new()
        };
        let sale = if can_sale {
            Some(sale_terms_in(&mut tx, business, &models).await?)
        } else {
            None
        };
        let collection = if actor.role.allows(BusinessCapability::ReportsRead) {
            Some(collection_in(&mut tx, business, secret, public_url_configured).await?)
        } else {
            None
        };
        let steps = vec![
            step(
                "profile",
                Some(row.try_get("profile")?),
                actor.role.allows(BusinessCapability::ProfileWrite),
                "profile",
            ),
            step(
                "supply",
                can_supply.then_some(!models.is_empty()),
                can_supply,
                "channels",
            ),
            step("saleTerms", sale, can_supply, "prices"),
            step(
                "collection",
                collection,
                actor.role.allows(BusinessCapability::MerchantSecretsWrite),
                "integrations",
            ),
            step(
                "admission",
                Some(row.try_get("admission")?),
                actor.role.allows(BusinessCapability::CustomersWrite),
                "customers",
            ),
        ];
        Ok(BusinessReadiness {
            checked_at: row.try_get("checked_at")?,
            ready: steps.iter().all(|step| step.state == "ready"),
            steps,
        })
    }
}
