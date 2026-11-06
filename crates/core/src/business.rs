//! Seller ownership over existing customer identities. No ledger or money moves
//! during mapping, and no customer membership implies business staff authority.

use serde::Serialize;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Db, WalletError};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Business {
    pub id: Uuid,
    pub name: String,
    pub registration_mode: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub initialized_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessMigration {
    pub business_id: Uuid,
    pub version: String,
    pub customers: i64,
    pub channels: i64,
    pub plans: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct BusinessOwnership {
    pub business: Business,
    pub migration: BusinessMigration,
}

/// Read-only preflight, callable on a baseline database before schema migration.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessMigrationPreview {
    pub already_mapped: bool,
    pub customers: i64,
    pub channels: i64,
    pub plans: i64,
    pub ledger_schemas: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerProfile {
    pub organization_id: Uuid,
    pub business_id: Uuid,
    pub external_id: String,
    pub contact_email: Option<String>,
    pub state: String,
    pub name: String,
    pub tenant_id: String,
}

impl Db {
    /// A stable count preview in a read-only transaction; no schema migration,
    /// ledger opening, owner assignment or external connection is performed.
    pub async fn business_migration_preview(
        &self,
    ) -> Result<BusinessMigrationPreview, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let mut counts = [0_i64; 3];
        for (index, table) in ["organizations", "channels", "subscription_plans"]
            .iter()
            .enumerate()
        {
            let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                .bind(format!("oxsum.{table}"))
                .fetch_one(&mut *tx)
                .await?;
            if exists {
                // Table names are a fixed internal inventory, never caller input.
                counts[index] = sqlx::query_scalar(&format!("SELECT count(*) FROM oxsum.{table}"))
                    .fetch_one(&mut *tx)
                    .await?;
            }
        }
        let already_mapped =
            sqlx::query_scalar("SELECT to_regclass('oxsum.business_migrations') IS NOT NULL")
                .fetch_one(&mut *tx)
                .await?;
        let ledger_schemas = sqlx::query_scalar(
            "SELECT count(*) FROM pg_namespace WHERE nspname ~ '^ledger_[a-z0-9_]{1,40}$'",
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.rollback().await?;
        Ok(BusinessMigrationPreview {
            already_mapped,
            customers: counts[0],
            channels: counts[1],
            plans: counts[2],
            ledger_schemas,
        })
    }

    pub async fn business_ownership(&self) -> Result<BusinessOwnership, WalletError> {
        let row = sqlx::query("SELECT business_id, name, registration_mode, initialized_at, created_at FROM oxsum.businesses WHERE singleton")
            .fetch_one(self.pool()).await?;
        let business = Business {
            id: row.try_get("business_id")?,
            name: row.try_get("name")?,
            registration_mode: row.try_get("registration_mode")?,
            initialized_at: row.try_get("initialized_at")?,
            created_at: row.try_get("created_at")?,
        };
        let row = sqlx::query("SELECT business_id, version, customers, channels, plans, recorded_at FROM oxsum.business_migrations WHERE version = '0072_business_ownership' AND business_id = $1")
            .bind(business.id).fetch_one(self.pool()).await?;
        let migration = BusinessMigration {
            business_id: row.try_get("business_id")?,
            version: row.try_get("version")?,
            customers: row.try_get("customers")?,
            channels: row.try_get("channels")?,
            plans: row.try_get("plans")?,
            recorded_at: row.try_get("recorded_at")?,
        };
        Ok(BusinessOwnership {
            business,
            migration,
        })
    }

    /// Scope ownership in the lookup itself. A foreign business/customer pair
    /// is indistinguishable from an unknown customer.
    pub async fn customer_profile(
        &self,
        business_id: Uuid,
        organization_id: Uuid,
    ) -> Result<CustomerProfile, WalletError> {
        let row = sqlx::query("SELECT c.organization_id, c.business_id, c.external_id, c.contact_email, c.state, o.name, o.tenant_id FROM oxsum.customer_profiles c JOIN oxsum.organizations o USING (organization_id) WHERE c.business_id = $1 AND c.organization_id = $2")
            .bind(business_id).bind(organization_id).fetch_optional(self.pool()).await?
            .ok_or_else(|| WalletError::NotFound("customer is not known".into()))?;
        Ok(CustomerProfile {
            organization_id: row.try_get("organization_id")?,
            business_id: row.try_get("business_id")?,
            external_id: row.try_get("external_id")?,
            contact_email: row.try_get("contact_email")?,
            state: row.try_get("state")?,
            name: row.try_get("name")?,
            tenant_id: row.try_get("tenant_id")?,
        })
    }
}
