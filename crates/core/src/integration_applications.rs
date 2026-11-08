//! Stable owner-created application namespaces and scoped customer lifecycle.

mod customers;

use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{BusinessCapability, Db, WalletError, business_staff as authority};

pub use customers::{ApplicationCustomer, ApplicationCustomerArchiveRequest};

/// Owner-authorized application identity and lifetime account capacity.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegrationApplicationRequest {
    /// Exact operation key; changed reuse conflicts.
    pub idempotency_key: String,
    /// Stable lowercase key unique within the business.
    pub key: String,
    /// Human-readable name, at most 80 characters.
    pub name: String,
    /// Maximum retained customer identities, including archived accounts.
    pub customer_limit: i32,
    /// Owner-visible reason recorded with the operation.
    pub reason: String,
}

/// An immutable application namespace; no credential material is included.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationApplication {
    /// Stable identity shared by replacement credentials.
    pub application_id: Uuid,
    /// Business that controls issuance.
    pub business_id: Uuid,
    /// Unique business-local application key.
    pub key: String,
    /// Display name chosen by the owner.
    pub name: String,
    /// Lifetime customer identity limit.
    pub customer_limit: i32,
    /// Creation instant.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Cursor-paginated owner read.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationApplicationPage {
    /// Applications in stable identifier order.
    pub applications: Vec<IntegrationApplication>,
    /// Last returned identity when another page exists.
    pub next_cursor: Option<Uuid>,
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<IntegrationApplication, WalletError> {
    Ok(IntegrationApplication {
        application_id: row.try_get("application_id")?,
        business_id: row.try_get("business_id")?,
        key: row.try_get("application_key")?,
        name: row.try_get("name")?,
        customer_limit: row.try_get("customer_limit")?,
        created_at: row.try_get("created_at")?,
    })
}

pub(crate) async fn application_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
) -> Result<IntegrationApplication, WalletError> {
    let row = sqlx::query(
        "SELECT * FROM oxsum.integration_applications WHERE business_id=$1 AND application_id=$2",
    )
    .bind(business)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| WalletError::NotFound("application is not known".into()))?;
    from_row(&row)
}

impl Db {
    /// Create an immutable bounded namespace under current owner authority.
    pub async fn create_integration_application(
        &self,
        token: &str,
        request: &IntegrationApplicationRequest,
    ) -> Result<IntegrationApplication, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let name = request.name.trim();
        if request.key.is_empty()
            || request.key.len() > 80
            || request.key.split('-').any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            })
            || name.is_empty()
            || name.chars().count() > 80
            || name.chars().any(char::is_control)
            || !(1..=10000).contains(&request.customer_limit)
        {
            return Err(WalletError::InvalidInput(
                "invalid application key, name or customer limit".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::IntegrationsManage,
        )
        .await?;
        let detail = serde_json::json!({"key":request.key,"name":name,"customerLimit":request.customer_limit,"reason":request.reason.trim()});
        let action = "business.application.create";
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
            return application_in(&mut tx, business, id).await;
        }
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        sqlx::query("INSERT INTO oxsum.integration_applications(application_id,business_id,application_key,name,customer_limit) VALUES($1,$2,$3,$4,$5)")
            .bind(receipt.id).bind(business).bind(&request.key).bind(name).bind(request.customer_limit).execute(&mut *tx).await.map_err(|error|crate::keys::conflict_or_storage(error,"integration_applications_business_id_application_key_key","application key is already assigned"))?;
        let result = application_in(&mut tx, business, receipt.id).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// List application metadata under current owner authority.
    pub async fn integration_applications(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<IntegrationApplicationPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput("limit must be 1 to 100".into()));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::IntegrationsManage,
        )
        .await?;
        let rows=sqlx::query("SELECT * FROM oxsum.integration_applications WHERE business_id=$1 AND ($2::uuid IS NULL OR application_id>$2) ORDER BY application_id LIMIT $3")
            .bind(business).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut applications = rows.iter().map(from_row).collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if applications.len() > limit as usize {
            applications.truncate(limit as usize);
            applications.last().map(|a| a.application_id)
        } else {
            None
        };
        Ok(IntegrationApplicationPage {
            applications,
            next_cursor,
        })
    }
}
