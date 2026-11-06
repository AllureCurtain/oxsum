//! Business-scoped customer profiles. Identity and ledgers never move.
use crate::{
    BusinessCapability, BusinessMutationResult, Db, WalletError, business_staff as authority,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessCustomer {
    pub organization_id: Uuid,
    pub business_id: Uuid,
    pub external_id: String,
    pub name: String,
    pub contact_email: Option<String>,
    pub state: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub manual_suspended_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub dunning_suspended_at: Option<OffsetDateTime>,
    pub suspended: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessCustomerPage {
    pub customers: Vec<BusinessCustomer>,
    pub next_cursor: Option<Uuid>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessCustomerUpdate {
    pub idempotency_key: String,
    pub name: String,
    pub contact_email: Option<String>,
    pub reason: String,
}
#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum CustomerState {
    Active,
    Archived,
}
impl CustomerState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessCustomerLifecycle {
    pub idempotency_key: String,
    pub state: CustomerState,
    pub reason: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessCustomerSuspension {
    pub idempotency_key: String,
    pub suspended: bool,
    pub reason: String,
}
const COLUMNS: &str = "c.organization_id,c.business_id,c.external_id,c.contact_email,c.state,o.name,o.created_at,o.suspended_at AS manual_suspended_at,o.dunning_suspended_at,(o.suspended_at IS NOT NULL OR o.dunning_suspended_at IS NOT NULL OR c.state='archived') AS suspended";
fn from_row(row: &sqlx::postgres::PgRow) -> Result<BusinessCustomer, WalletError> {
    Ok(BusinessCustomer {
        organization_id: row.try_get("organization_id")?,
        business_id: row.try_get("business_id")?,
        external_id: row.try_get("external_id")?,
        name: row.try_get("name")?,
        contact_email: row.try_get("contact_email")?,
        state: row.try_get("state")?,
        manual_suspended_at: row.try_get("manual_suspended_at")?,
        dunning_suspended_at: row.try_get("dunning_suspended_at")?,
        suspended: row.try_get("suspended")?,
        created_at: row.try_get("created_at")?,
    })
}
async fn customer(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
) -> Result<BusinessCustomer, WalletError> {
    let row=sqlx::query(&format!("SELECT {COLUMNS} FROM oxsum.customer_profiles c JOIN oxsum.organizations o USING (organization_id) WHERE c.business_id=$1 AND c.organization_id=$2"))
        .bind(business).bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::NotFound("customer is not known".into()))?;
    from_row(&row)
}
async fn admission_lock(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<(), WalletError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || $2::text,0))")
        .bind(crate::wallet::ORG_ADMISSION_LOCK)
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
impl Db {
    pub async fn integration_customers(
        &self,
        service: &crate::ActingService,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<BusinessCustomerPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput(
                "limit must be between 1 and 100".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        crate::integration_credentials::require_integration(
            &mut tx,
            service,
            crate::IntegrationScope::CustomersRead,
            None,
        )
        .await?;
        let rows=sqlx::query(&format!("SELECT {COLUMNS} FROM oxsum.customer_profiles c JOIN oxsum.organizations o USING (organization_id) JOIN oxsum.integration_credential_customers a ON a.business_id=c.business_id AND a.organization_id=c.organization_id WHERE c.business_id=$1 AND a.credential_id=$2 AND ($3::uuid IS NULL OR c.organization_id>$3) ORDER BY c.organization_id LIMIT $4"))
            .bind(service.business_id).bind(service.credential_id).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut customers = rows.iter().map(from_row).collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if customers.len() > limit as usize {
            customers.truncate(limit as usize);
            customers.last().map(|c| c.organization_id)
        } else {
            None
        };
        Ok(BusinessCustomerPage {
            customers,
            next_cursor,
        })
    }
    pub async fn integration_customer(
        &self,
        service: &crate::ActingService,
        id: Uuid,
    ) -> Result<BusinessCustomer, WalletError> {
        let mut tx = self.pool().begin().await?;
        crate::integration_credentials::require_integration(
            &mut tx,
            service,
            crate::IntegrationScope::CustomersRead,
            Some(id),
        )
        .await?;
        customer(&mut tx, service.business_id, id).await
    }
    pub async fn business_customers(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<BusinessCustomerPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput(
                "limit must be between 1 and 100".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::CustomersRead)
            .await?;
        let rows=sqlx::query(&format!("SELECT {COLUMNS} FROM oxsum.customer_profiles c JOIN oxsum.organizations o USING (organization_id) WHERE c.business_id=$1 AND ($2::uuid IS NULL OR c.organization_id>$2) ORDER BY c.organization_id LIMIT $3"))
            .bind(business).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut customers = rows.iter().map(from_row).collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if customers.len() > limit as usize {
            customers.truncate(limit as usize);
            customers.last().map(|c| c.organization_id)
        } else {
            None
        };
        Ok(BusinessCustomerPage {
            customers,
            next_cursor,
        })
    }
    pub async fn business_customer(
        &self,
        token: &str,
        id: Uuid,
    ) -> Result<BusinessCustomer, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::CustomersRead)
            .await?;
        customer(&mut tx, business, id).await
    }
    pub async fn update_business_customer(
        &self,
        token: &str,
        id: Uuid,
        request: &BusinessCustomerUpdate,
    ) -> Result<BusinessMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let name = request.name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err(WalletError::InvalidInput(
                "name must contain 1 to 80 characters".into(),
            ));
        }
        let email = request
            .contact_email
            .as_deref()
            .map(crate::users::validate_email)
            .transpose()?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::CustomersWrite,
        )
        .await?;
        customer(&mut tx, business, id).await?;
        let detail = json!({"organizationId":id,"request":request});
        let action = "business.customer.update";
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
            return Ok(BusinessMutationResult { id });
        }
        sqlx::query("UPDATE oxsum.organizations SET name=$2 WHERE organization_id=$1")
            .bind(id)
            .bind(name)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE oxsum.customer_profiles SET contact_email=$2 WHERE business_id=$1 AND organization_id=$3").bind(business).bind(email).bind(id).execute(&mut *tx).await?;
        let result = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn set_customer_lifecycle(
        &self,
        token: &str,
        id: Uuid,
        request: &BusinessCustomerLifecycle,
    ) -> Result<BusinessMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::CustomersWrite,
        )
        .await?;
        admission_lock(&mut tx, id).await?;
        let before = customer(&mut tx, business, id).await?;
        let detail = json!({"organizationId":id,"request":request});
        let action = "business.customer.lifecycle";
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
            return Ok(BusinessMutationResult { id });
        }
        sqlx::query("UPDATE oxsum.customer_profiles SET state=$3,archived_at=CASE WHEN $3='archived' THEN COALESCE(archived_at,now()) ELSE NULL END WHERE business_id=$1 AND organization_id=$2").bind(business).bind(id).bind(request.state.as_str()).execute(&mut *tx).await?;
        if !before.suspended && matches!(request.state, CustomerState::Archived) {
            Self::enqueue_event(
                &mut tx,
                id,
                crate::webhooks::ORG_SUSPENDED,
                json!({"organizationId":id,"reason":"archived"}),
            )
            .await?;
        }
        let result = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn set_customer_suspension(
        &self,
        token: &str,
        id: Uuid,
        request: &BusinessCustomerSuspension,
    ) -> Result<BusinessMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::CustomersSuspend,
        )
        .await?;
        admission_lock(&mut tx, id).await?;
        let before = customer(&mut tx, business, id).await?;
        let detail = json!({"organizationId":id,"request":request});
        let action = "business.customer.suspension";
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
            return Ok(BusinessMutationResult { id });
        }
        sqlx::query("UPDATE oxsum.organizations SET suspended_at=CASE WHEN $2 THEN COALESCE(suspended_at,now()) ELSE NULL END WHERE organization_id=$1").bind(id).bind(request.suspended).execute(&mut *tx).await?;
        if !before.suspended && request.suspended {
            Self::enqueue_event(
                &mut tx,
                id,
                crate::webhooks::ORG_SUSPENDED,
                json!({"organizationId":id,"reason":"manual"}),
            )
            .await?;
        }
        let result = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
}
