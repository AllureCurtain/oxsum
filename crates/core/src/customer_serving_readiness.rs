//! Bounded personal key/model selection and sale-priced funding in one read snapshot.

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    BillingMode, CustomerCapability, Db, KeyScope, Principal, Wallet, WalletError, sale_books,
};

const KEY_LIMIT: usize = 8;
const MODEL_LIMIT: usize = 16;
const PROMPT: &str = "Hello";
const OUTPUT: i64 = 32;

/// Exact illustrative request and its conservative sale-priced reservation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerServingSample {
    /// Visible live key metadata; never the secret.
    pub key_id: Uuid,
    /// Permitted public chat model.
    pub model: String,
    /// Single user message used for this preview.
    pub prompt: &'static str,
    /// Requested output ceiling before provider-specific clamps.
    pub max_output_tokens: i64,
    /// Gateway's text byte/overhead input bound.
    pub input_upper_bound: i64,
    /// Undiscounted gateway reservation, including conditional rates and flat fees.
    pub bound_minor: i64,
    /// Recorded quota coverage; absent when the ledger is uninitialized.
    pub quota_minor: Option<i64>,
    /// Wallet remainder; absent when the ledger is uninitialized.
    pub wallet_minor: Option<i64>,
    /// Effective override, assigned or default sale source.
    pub sale_source: String,
    /// Published sale version, absent for a direct customer override.
    pub sale_version: Option<i64>,
    /// Settlement discount; gateway reservations do not subtract it.
    pub discount_percent: i64,
}

/// Snapshot of a bounded search, never a provider or execution certification.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerServingReadiness {
    /// Database snapshot time.
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    /// At least one visible live key exists.
    pub key_ready: bool,
    /// A permitted chat model with effective customer sale terms was found.
    pub model_ready: bool,
    /// The selected sample passed shared funding constraints at this snapshot.
    pub funding_ready: bool,
    /// Refusal/uncertainty class; absent only when funding passes.
    pub reason: Option<&'static str>,
    /// More keys/models exist beyond the documented scan bounds.
    pub scan_limited: bool,
    /// Selected funded sample, otherwise the first priced sample encountered.
    pub sample: Option<CustomerServingSample>,
}

impl Db {
    /// Read customer serving readiness without bootstrap, financial writes or provider I/O.
    /// The principal must come from current personal-session authentication.
    pub async fn customer_serving_readiness(
        &self,
        principal: &Principal,
    ) -> Result<CustomerServingReadiness, WalletError> {
        principal.require_customer(CustomerCapability::Personal)?;
        let organization = principal.organization();
        let owner = match principal.key_scope() {
            KeyScope::Own(user) => Some(user),
            _ => None,
        };
        // Account identities are immutable. Opening the registry reads only;
        // every balance/authority/sale fact below uses the one transaction.
        // Open before borrowing its connection so one-connection pools work.
        let initialized: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!(
                "\"ledger_{}\".entries",
                organization.tenant_id.replace('"', "\"\"")
            ))
            .fetch_one(self.pool())
            .await?;
        let wallet = if initialized {
            Some(Wallet::open_existing(self.pool().clone(), &organization.tenant_id).await?)
        } else {
            None
        };
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let as_of = sqlx::query_scalar("SELECT now()")
            .fetch_one(&mut *tx)
            .await?;
        let restricted: bool = sqlx::query_scalar(
            "SELECT o.suspended_at IS NOT NULL OR o.dunning_suspended_at IS NOT NULL OR c.state='archived' \
             FROM oxsum.organizations o JOIN oxsum.customer_profiles c USING(organization_id) \
             WHERE o.organization_id=$1 AND o.tenant_id=$2",
        ).bind(organization.id).bind(&organization.tenant_id).fetch_one(&mut *tx).await?;
        let keys: Vec<Uuid> = sqlx::query_scalar(
            "SELECT key_id FROM oxsum.api_keys WHERE organization_id=$1 \
             AND ($2::uuid IS NULL OR created_by=$2) AND revoked_at IS NULL \
             AND (expires_at IS NULL OR expires_at>now()) ORDER BY key_id LIMIT $3",
        )
        .bind(organization.id)
        .bind(owner)
        .bind((KEY_LIMIT + 1) as i64)
        .fetch_all(&mut *tx)
        .await?;
        let mut result = CustomerServingReadiness {
            as_of,
            key_ready: !keys.is_empty(),
            model_ready: false,
            funding_ready: false,
            reason: Some(if restricted {
                "restricted"
            } else if keys.is_empty() {
                "noLiveKey"
            } else {
                "noPermittedChatModel"
            }),
            scan_limited: keys.len() > KEY_LIMIT,
            sample: None,
        };
        if restricted || keys.is_empty() {
            return Ok(result);
        }
        for key in keys.into_iter().take(KEY_LIMIT) {
            let access = self
                .serving_access_in(&mut tx, organization.id, Some(key))
                .await?;
            let names = access
                .public_models_filtered_in(&mut tx, None, (MODEL_LIMIT + 1) as i64, Some("chat"))
                .await?;
            result.scan_limited |= names.len() > MODEL_LIMIT;
            for model in names.into_iter().take(MODEL_LIMIT) {
                let Some(sale) = sale_books::read_effective_in(
                    &mut tx,
                    access.business_id,
                    organization.id,
                    &model,
                    as_of,
                )
                .await?
                else {
                    if result.sample.is_none() {
                        result.reason = Some("saleUnavailable");
                    }
                    continue;
                };
                if sale.terms.price.price().mode != BillingMode::Chat {
                    if result.sample.is_none() {
                        result.reason = Some("saleModeMismatch");
                    }
                    continue;
                }
                let bound = sale
                    .terms
                    .price
                    .price()
                    .freeze_minor(&[PROMPT], Some(OUTPUT), None)?;
                let funding = match &wallet {
                    Some(wallet) => Some(
                        wallet
                            .preview_gateway_funding_in(
                                self, &mut tx, principal, key, &model, bound,
                            )
                            .await?,
                    ),
                    None => None,
                };
                let allowed = funding.as_ref().is_some_and(|preview| preview.allowed);
                if result.sample.is_none() || allowed {
                    result.model_ready = true;
                    result.funding_ready = allowed;
                    result.reason = funding
                        .as_ref()
                        .map_or(Some("ledgerUninitialized"), |preview| preview.reason);
                    result.sample = Some(CustomerServingSample {
                        key_id: key,
                        model,
                        prompt: PROMPT,
                        max_output_tokens: OUTPUT,
                        input_upper_bound: crate::input_upper_bound(&[PROMPT]),
                        bound_minor: bound,
                        quota_minor: funding.as_ref().map(|preview| preview.quota_minor),
                        wallet_minor: funding.as_ref().map(|preview| preview.wallet_minor),
                        sale_source: sale.source,
                        sale_version: sale.version,
                        discount_percent: sale.terms.discount_percent,
                    });
                }
                if allowed {
                    tx.commit().await?;
                    return Ok(result);
                }
            }
        }
        tx.commit().await?;
        Ok(result)
    }
}
