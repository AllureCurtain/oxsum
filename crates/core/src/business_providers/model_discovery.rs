//! Durable, owner-authorized model catalog discovery intents and results.

use crate::business_providers::{owned_channel, probe_endpoint};
use crate::{BusinessCapability, Db, TaskProviderEndpoint, WalletError, business_staff, channels};
use serde::Serialize;
use serde_json::json;
use sqlx::{Postgres, Row};
use uuid::Uuid;

const ACTION: &str = "business.provider.model_discovery";

/// Latest recorded state of an explicit model discovery request.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelDiscovery {
    /// The stable discovery receipt ID.
    pub id: Uuid,
    /// The owned channel whose catalog was requested.
    pub channel_id: Uuid,
    /// Pending, listed, or a redacted failure classification.
    pub state: String,
    /// HTTP status of the upstream model list response, if any.
    pub http_status: Option<i32>,
    /// Sorted candidate IDs, not configured models or sale terms.
    pub models: Vec<String>,
    /// Whether another page or omitted catalog entries make the list partial.
    pub partial: bool,
}

/// A committed result, with the owned provider snapshot only for a new intent.
pub struct ProviderModelDiscoveryStart {
    /// Standing result for this request, including interrupted pending intents.
    pub result: ProviderModelDiscovery,
    /// Only the request that created the intent receives credentials to call upstream.
    pub endpoint: Option<TaskProviderEndpoint>,
}

async fn result<'e>(
    executor: impl sqlx::Executor<'e, Database = Postgres>,
    id: Uuid,
) -> Result<ProviderModelDiscovery, WalletError> {
    let row = sqlx::query("SELECT channel_id,state,http_status,models,partial FROM oxsum.provider_model_discoveries WHERE mutation_id=$1")
        .bind(id).fetch_one(executor).await?;
    Ok(ProviderModelDiscovery {
        id,
        channel_id: row.try_get("channel_id")?,
        state: row.try_get("state")?,
        http_status: row.try_get("http_status")?,
        models: row.try_get("models")?,
        partial: row.try_get("partial")?,
    })
}

impl Db {
    /// Commit audit before permitting one request to the pinned provider. A replay
    /// checks current authority but never returns an endpoint or credential.
    pub async fn begin_provider_model_discovery(
        &self,
        token: &str,
        channel: &str,
        key: &str,
        reason: &str,
    ) -> Result<ProviderModelDiscoveryStart, WalletError> {
        business_staff::validate(key, Some(reason))?;
        let mut tx = self.pool().begin().await?;
        channels::lock_configuration(&mut tx).await?;
        let business = business_staff::lock_business(&mut tx).await?;
        let actor = business_staff::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::ProviderSecretsWrite,
        )
        .await?;
        let detail = json!({"channel":channel,"reason":reason});
        if let Some(id) =
            business_staff::replay(&mut tx, business, actor.user_id, key, ACTION, &detail).await?
        {
            return Ok(ProviderModelDiscoveryStart {
                result: result(&mut *tx, id).await?,
                endpoint: None,
            });
        }
        let channel_id = owned_channel(&mut tx, business, channel).await?;
        let endpoint = probe_endpoint(&mut tx, channel_id).await?;
        let receipt =
            business_staff::record(&mut tx, business, actor.user_id, key, ACTION, &detail).await?;
        sqlx::query(
            "INSERT INTO oxsum.provider_model_discoveries(mutation_id,channel_id) VALUES($1,$2)",
        )
        .bind(receipt.id)
        .bind(channel_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ProviderModelDiscoveryStart {
            result: ProviderModelDiscovery {
                id: receipt.id,
                channel_id,
                state: "pending".into(),
                http_status: None,
                models: Vec::new(),
                partial: false,
            },
            endpoint: Some(endpoint),
        })
    }

    /// Retain the first terminal result, with only validated model IDs. Results
    /// have no effect on the serving catalog or customer access.
    pub async fn finish_provider_model_discovery(
        &self,
        id: Uuid,
        state: &str,
        status: Option<u16>,
        models: &[String],
        partial: bool,
    ) -> Result<ProviderModelDiscovery, WalletError> {
        if !matches!(
            state,
            "listed"
                | "unsupported"
                | "authenticationFailed"
                | "httpError"
                | "invalidCatalog"
                | "blocked"
                | "unreachable"
        ) || (state != "listed" && (!models.is_empty() || partial))
            || models.len() > 500
            || models.windows(2).any(|pair| pair[0] >= pair[1])
            || models
                .iter()
                .any(|id| channels::model_name(id).ok() != Some(id.as_str()))
        {
            return Err(WalletError::InvalidInput(
                "invalid model discovery result".into(),
            ));
        }
        sqlx::query("UPDATE oxsum.provider_model_discoveries SET state=$2,http_status=$3,models=$4,partial=$5,completed_at=now() WHERE mutation_id=$1 AND state='pending'")
            .bind(id).bind(state).bind(status.map(i32::from)).bind(models).bind(partial).execute(self.pool()).await?;
        result(self.pool(), id).await
    }
}
