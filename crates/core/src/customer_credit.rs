//! Business-authorized monthly credit, distinct from sold-plan receivable caps.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::wallet::ORG_ADMISSION_LOCK;
use crate::{
    BusinessCapability, BusinessMutationResult, Db, Tenants, Wallet, WalletError,
    business_staff as authority, customers,
};

/// Current monthly credit facility and payment terms for one customer.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessCustomerCreditTerms {
    /// Customer billing account.
    pub organization_id: Uuid,
    /// Total ledger facility, distinct from plan credit limits.
    pub credit_limit_minor: i64,
    /// Settled credit draws plus reserved credit.
    pub credit_used_minor: i64,
    /// Days applied to future monthly invoice finalization.
    pub payment_terms_days: i32,
    /// Monthly dunning grace; absent disables automatic monthly dunning.
    pub dunning_grace_days: Option<i32>,
}

/// Exact monthly credit decision, recorded atomically with its ledger effect.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessCustomerCreditTermsRequest {
    /// Stable identity for exact retries, including no-op limit changes.
    pub idempotency_key: String,
    /// Total approved facility in integer credit minor units.
    pub credit_limit_minor: i64,
    /// Future monthly invoice payment terms, between zero and ten years.
    pub payment_terms_days: i32,
    /// Current monthly dunning grace; explicit null disables its automation.
    #[serde(deserialize_with = "required_grace")]
    pub dunning_grace_days: Option<i32>,
    /// Operator's reason retained in the immutable actor audit.
    pub reason: String,
}

fn required_grace<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<i32>, D::Error> {
    Option::<i32>::deserialize(deserializer)
}

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
    id: Uuid,
    capability: BusinessCapability,
) -> Result<(Uuid, Uuid, String, String), WalletError> {
    let business = authority::lock_business(tx).await?;
    let actor = authority::require_capability(tx, business, token, capability).await?;
    let customer = customers::customer(tx, business, id).await?;
    let tenant =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    Ok((business, actor.user_id, tenant, customer.state))
}

impl Db {
    /// Read monthly credit without opening a new ledger or changing any terms.
    pub async fn business_customer_credit_terms(
        &self,
        token: &str,
        id: Uuid,
    ) -> Result<BusinessCustomerCreditTerms, WalletError> {
        let result = self.customer_credit_terms_snapshot(token, id).await?;
        // A repeatable-read snapshot can predate a staff revocation whose
        // Business lock it waited for. Recheck with a fresh READ COMMITTED
        // transaction before returning any data, without a nested checkout.
        let mut current = self.pool().begin().await?;
        authorize(&mut current, token, id, BusinessCapability::ReportsRead).await?;
        Ok(result)
    }

    async fn customer_credit_terms_snapshot(
        &self,
        token: &str,
        id: Uuid,
    ) -> Result<BusinessCustomerCreditTerms, WalletError> {
        // Absence and terms belong to the same snapshot: a concurrent first
        // grant must never pair new terms with a stale zero facility.
        let mut preflight = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *preflight)
            .await?;
        let (_, _, tenant, _) =
            authorize(&mut preflight, token, id, BusinessCapability::ReportsRead).await?;
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("\"ledger_{tenant}\".entries"))
            .fetch_one(&mut *preflight)
            .await?;
        if !exists {
            let row = sqlx::query("SELECT payment_terms_days,dunning_grace_days FROM oxsum.organizations WHERE organization_id=$1")
                .bind(id).fetch_one(&mut *preflight).await?;
            let result = BusinessCustomerCreditTerms {
                organization_id: id,
                credit_limit_minor: 0,
                credit_used_minor: 0,
                payment_terms_days: row.try_get("payment_terms_days")?,
                dunning_grace_days: row.try_get("dunning_grace_days")?,
            };
            preflight.commit().await?;
            return Ok(result);
        }
        preflight.commit().await?;
        // Opening the read-only facade outside the transaction also permits
        // a one-connection pool. Recheck live authority in the final snapshot.
        let wallet = Wallet::open_existing(self.pool().clone(), &tenant).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        authorize(&mut tx, token, id, BusinessCapability::ReportsRead).await?;
        let row = sqlx::query("SELECT payment_terms_days,dunning_grace_days FROM oxsum.organizations WHERE organization_id=$1")
            .bind(id).fetch_one(&mut *tx).await?;
        let (credit_limit_minor, credit_used_minor) = wallet.credit_state_in(&mut tx).await?;
        let result = BusinessCustomerCreditTerms {
            organization_id: id,
            credit_limit_minor,
            credit_used_minor,
            payment_terms_days: row.try_get("payment_terms_days")?,
            dunning_grace_days: row.try_get("dunning_grace_days")?,
        };
        tx.commit().await?;
        Ok(result)
    }

    /// Atomically approve monthly credit, terms and audit under live staff authority.
    pub async fn set_business_customer_credit_terms(
        &self,
        token: &str,
        tenants: &Tenants,
        id: Uuid,
        request: &BusinessCustomerCreditTermsRequest,
    ) -> Result<BusinessMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        if !(0..=9_007_199_254_740_991).contains(&request.credit_limit_minor)
            || !(0..=3650).contains(&request.payment_terms_days)
            || request
                .dunning_grace_days
                .is_some_and(|days| !(0..=3650).contains(&days))
        {
            return Err(WalletError::InvalidInput("credit must be an exact nonnegative safe integer; payment terms and grace must be between 0 and 3650 days".into()));
        }
        let detail = json!({"organizationId":id,"request":request});
        let action = "business.customer.credit_terms";
        let mut preflight = self.pool().begin().await?;
        let (business, actor, tenant, state) = authorize(
            &mut preflight,
            token,
            id,
            BusinessCapability::CommercialWrite,
        )
        .await?;
        if let Some(id) = authority::replay(
            &mut preflight,
            business,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return Ok(BusinessMutationResult { id });
        }
        if state != "active" {
            return Err(WalletError::Conflict(
                "archived customer credit terms cannot change".into(),
            ));
        }
        preflight.commit().await?;
        // Initial schema creation is outside the business transaction; no credit
        // is granted until authority is rechecked and all financial writes commit.
        let wallet = tenants.get(&tenant).await?;
        let mut tx = self.pool().begin().await?;
        let (business, actor, _, _) =
            authorize(&mut tx, token, id, BusinessCapability::CommercialWrite).await?;
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return Ok(BusinessMutationResult { id });
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || $2,0))")
            .bind(ORG_ADMISSION_LOCK)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        let customer = customers::customer(&mut tx, business, id).await?;
        if customer.state != "active" {
            return Err(WalletError::Conflict(
                "archived customer credit terms cannot change".into(),
            ));
        }
        // Record first to derive an unguessable, actor-bound ledger identity.
        // Any limit, ledger or column failure rolls back this receipt and audit.
        let result = authority::record(
            &mut tx,
            business,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        wallet
            .set_credit_limit_in(
                &mut tx,
                &format!("business-credit:{}", result.id),
                request.credit_limit_minor,
                OffsetDateTime::now_utc().date(),
            )
            .await?;
        sqlx::query("UPDATE oxsum.organizations SET payment_terms_days=$2,dunning_grace_days=$3 WHERE organization_id=$1")
            .bind(id).bind(request.payment_terms_days).bind(request.dunning_grace_days).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }
}
