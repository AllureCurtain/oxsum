//! Admission-time customer, key and channel permissions shared by every model surface.
use std::collections::BTreeSet;

use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::{Db, WalletError};

/// One coherent read of the caller's authority. Changes affect later admissions;
/// an admitted task retains its identity and endpoint for terminal recovery.
/// Sorted sets also make this suitable for a versioned cache-context digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServingAccess {
    /// The customer the credential serves.
    pub customer_id: Uuid,
    /// The consumption key used; `None` for a service-credential turn.
    pub key_id: Option<Uuid>,
    /// The business that owns the customer.
    pub business_id: Uuid,
    key_models: Option<BTreeSet<String>>,
    customer_models: Option<BTreeSet<String>>,
    channels: BTreeSet<Uuid>,
    /// The credential's rolling-minute request cap; `None` is uncapped.
    pub requests_per_minute: Option<i32>,
}

impl ServingAccess {
    /// Whether both the key's and the customer's model scopes allow this public
    /// model name; an absent scope allows everything.
    pub fn allows_model(&self, model: &str) -> bool {
        self.key_models
            .as_ref()
            .is_none_or(|names| names.contains(model))
            && self
                .customer_models
                .as_ref()
                .is_none_or(|names| names.contains(model))
    }

    /// Permission for a public alias is never permission for an excluded target.
    pub fn allows_path(&self, model: &str, upstream: Option<&str>) -> bool {
        self.allows_model(model) && upstream.is_none_or(|name| self.allows_model(name))
    }

    /// Whether the credential may route a request to this channel.
    pub fn allows_channel(&self, channel: Uuid) -> bool {
        self.channels.contains(&channel)
    }

    /// Bounded public-name projection of the same direct/alias authority as the catalog.
    /// Filtering precedes pagination; no provider identity, price or secret is selected.
    pub(crate) async fn public_models_in(
        &self,
        connection: &mut sqlx::PgConnection,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, WalletError> {
        self.public_models_filtered_in(connection, after, limit, None)
            .await
    }

    pub(crate) async fn public_models_filtered_in(
        &self,
        connection: &mut sqlx::PgConnection,
        after: Option<&str>,
        limit: i64,
        mode: Option<&str>,
    ) -> Result<Vec<String>, WalletError> {
        let key_models = self
            .key_models
            .as_ref()
            .map(|names| names.iter().cloned().collect::<Vec<_>>());
        let customer_models = self
            .customer_models
            .as_ref()
            .map(|names| names.iter().cloned().collect::<Vec<_>>());
        Ok(sqlx::query_scalar(
            "WITH latest AS (SELECT DISTINCT ON (p.channel_id,p.model) \
                 p.channel_id,p.model,p.mode FROM oxsum.channel_prices p \
                 JOIN oxsum.channels c USING(channel_id) \
                 WHERE c.business_id=$1 AND c.channel_id=ANY($2) \
                 ORDER BY p.channel_id,p.model,p.version DESC), \
             names AS (SELECT p.channel_id,p.model AS public_model,p.model AS price_model, \
                     NULL::text AS upstream_model FROM latest p \
                 WHERE NOT EXISTS(SELECT 1 FROM oxsum.model_aliases a WHERE a.alias=p.model) \
                 UNION ALL SELECT a.channel_id,a.alias, \
                     CASE WHEN EXISTS(SELECT 1 FROM latest p WHERE p.channel_id=a.channel_id \
                         AND p.model=a.alias) THEN a.alias ELSE a.upstream_model END, \
                     a.upstream_model FROM oxsum.model_aliases a WHERE a.channel_id=ANY($2)) \
             SELECT DISTINCT n.public_model COLLATE \"C\" FROM names n \
             JOIN latest p ON p.channel_id=n.channel_id AND p.model=n.price_model \
             JOIN oxsum.channels c ON c.channel_id=n.channel_id \
             WHERE c.business_id=$1 AND c.channel_id=ANY($2) \
               AND NOT(c.protocol='tasks' AND n.upstream_model IS NOT NULL) \
               AND (p.mode<>'event' OR c.protocol='tasks') \
               AND ($7::text IS NULL OR (p.mode=$7 AND c.protocol<>'tasks')) \
               AND ($3::text[] IS NULL OR (n.public_model=ANY($3) \
                    AND (n.upstream_model IS NULL OR n.upstream_model=ANY($3)))) \
               AND ($4::text[] IS NULL OR (n.public_model=ANY($4) \
                    AND (n.upstream_model IS NULL OR n.upstream_model=ANY($4)))) \
               AND ($5::text IS NULL OR n.public_model COLLATE \"C\">$5 COLLATE \"C\") \
             ORDER BY n.public_model COLLATE \"C\" LIMIT $6",
        )
        .bind(self.business_id)
        .bind(self.channel_ids())
        .bind(key_models)
        .bind(customer_models)
        .bind(after)
        .bind(limit)
        .bind(mode)
        .fetch_all(connection)
        .await?)
    }

    pub(crate) fn channel_ids(&self) -> Vec<Uuid> {
        self.channels.iter().copied().collect()
    }
}

#[cfg(test)]
impl ServingAccess {
    /// A synthetic snapshot for digest tests: the DB-backed constructor needs
    /// a live database, while a cache-key test needs only controlled fields.
    pub(crate) fn sample(
        customer_id: Uuid,
        key_id: Option<Uuid>,
        business_id: Uuid,
        key_models: Option<&[&str]>,
        customer_models: Option<&[&str]>,
        channels: &[Uuid],
        requests_per_minute: Option<i32>,
    ) -> Self {
        Self {
            customer_id,
            key_id,
            business_id,
            key_models: key_models
                .map(|names| names.iter().map(|name| (*name).to_owned()).collect()),
            customer_models: customer_models
                .map(|names| names.iter().map(|name| (*name).to_owned()).collect()),
            channels: channels.iter().copied().collect(),
            requests_per_minute,
        }
    }
}

fn models(value: Option<serde_json::Value>) -> Result<Option<BTreeSet<String>>, WalletError> {
    value
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| WalletError::Misconfigured("invalid stored serving model permissions".into()))
}

impl Db {
    /// A missing, expired, revoked or foreign key cannot become unrestricted.
    /// `None` is reserved for an authenticated customer's personal catalog read.
    pub async fn serving_access(
        &self,
        customer_id: Uuid,
        key_id: Option<Uuid>,
    ) -> Result<ServingAccess, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        self.serving_access_in(&mut tx, customer_id, key_id).await
    }

    /// Re-read the route authority on admission's own connection after its key guard.
    pub(crate) async fn serving_access_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        customer_id: Uuid,
        key_id: Option<Uuid>,
    ) -> Result<ServingAccess, WalletError> {
        let customer = sqlx::query(
            "SELECT cp.business_id, t.model_allowlist, t.requests_per_minute \
             FROM oxsum.customer_profiles cp \
             JOIN oxsum.organizations o USING(organization_id) \
             LEFT JOIN oxsum.tier_profiles t ON t.name=o.tier \
             WHERE cp.organization_id=$1",
        )
        .bind(customer_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(WalletError::Unauthenticated)?;
        let business_id: Uuid = customer.try_get("business_id")?;
        let (key_models, group) = match key_id {
            Some(key) => {
                let row = sqlx::query(
                    "SELECT model_allowlist,group_id FROM oxsum.api_keys \
                     WHERE key_id=$1 AND organization_id=$2 AND revoked_at IS NULL \
                     AND (expires_at IS NULL OR expires_at>clock_timestamp())",
                )
                .bind(key)
                .bind(customer_id)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or(WalletError::Unauthenticated)?;
                (
                    row.try_get::<Option<Vec<String>>, _>("model_allowlist")?
                        .map(|names| names.into_iter().collect()),
                    row.try_get::<Option<Uuid>, _>("group_id")?,
                )
            }
            None => (None, None),
        };
        let channels: Vec<Uuid> = sqlx::query_scalar(
            "SELECT c.channel_id FROM oxsum.channels c WHERE c.business_id=$1 \
             AND ($2::uuid IS NULL OR EXISTS(SELECT 1 FROM oxsum.group_channels gc \
                 JOIN oxsum.groups g ON g.id=gc.group_id \
                 WHERE gc.channel_id=c.channel_id AND gc.group_id=$2 AND g.org_id=$3)) \
             ORDER BY c.channel_id",
        )
        .bind(business_id)
        .bind(group)
        .bind(customer_id)
        .fetch_all(&mut **tx)
        .await?;
        Ok(ServingAccess {
            customer_id,
            key_id,
            business_id,
            key_models,
            customer_models: models(customer.try_get("model_allowlist")?)?,
            channels: channels.into_iter().collect(),
            requests_per_minute: customer.try_get("requests_per_minute")?,
        })
    }
}
