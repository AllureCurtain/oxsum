//! Business-scoped provider configuration, with atomic authority, replay and audit.
use crate::channels::{self, LAST4, MAX_WEIGHT, credential_text, last_chars};
use crate::{
    BusinessCapability, Channel, Db, ModelAlias, ModelPrice, Price, SecretKey, WalletError,
    business_staff as authority,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderRequest<T> {
    pub idempotency_key: String,
    pub reason: String,
    pub config: T,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderConnection {
    pub name: String,
    pub base_url: String,
    pub protocol: String,
    pub api_keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_policy: Option<crate::ProviderDestinationPolicy>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCredentialInput {
    pub api_key: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderBillKeyInput {
    #[serde(deserialize_with = "required_nullable")]
    pub api_key: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderStatusInput {
    pub status: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCacheInput {
    #[serde(deserialize_with = "required_nullable")]
    pub policy: Option<Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderAliasInput {
    pub alias: String,
    pub upstream_model: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPriceInput {
    pub model: String,
    #[serde(deserialize_with = "explicit_price")]
    pub price: Price,
    pub weight: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderFallbackInput {
    pub fallbacks: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRemoveInput {}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
fn explicit_price<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Price, D::Error> {
    let value = Value::deserialize(deserializer)?;
    for name in [
        "inputPricePerMillion",
        "outputPricePerMillion",
        "maxOutputTokens",
    ] {
        if value.get(name).is_none() {
            return Err(serde::de::Error::custom(format!("price requires {name}")));
        }
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
fn cache_policy(policy: &Value) -> Result<(), WalletError> {
    let Some(map) = policy.as_object() else {
        return Err(WalletError::InvalidInput(
            "cache policy must be an object".into(),
        ));
    };
    if map
        .keys()
        .any(|k| !matches!(k.as_str(), "enabled" | "ttlSeconds" | "excludeSystem"))
        || !map.get("enabled").is_some_and(Value::is_boolean)
        || map.get("excludeSystem").is_some_and(|v| !v.is_boolean())
    {
        return Err(WalletError::InvalidInput(
            "cache policy requires enabled and accepts only enabled, ttlSeconds and excludeSystem"
                .into(),
        ));
    }
    crate::CachePolicy::of(Some(&json!({crate::cache::SETTINGS_KEY:policy})))
        .map_err(|_| WalletError::InvalidInput("invalid cache policy".into()))?;
    Ok(())
}
fn redacted_endpoint(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return "invalid provider endpoint".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_owned()
}
fn redacted_settings(settings: &Value) -> Value {
    let mut result = match settings.get(crate::cache::SETTINGS_KEY) {
        Some(policy) => {
            let mut safe = serde_json::Map::new();
            for field in ["enabled", "ttlSeconds", "excludeSystem"] {
                if let Some(v) = policy.get(field)
                    && ((field == "ttlSeconds" && (v.is_null() || v.as_i64().is_some()))
                        || (field != "ttlSeconds" && v.is_boolean()))
                {
                    safe.insert(field.into(), v.clone());
                }
            }
            json!({crate::cache::SETTINGS_KEY:safe})
        }
        None => json!({}),
    };
    if let Ok(policy) = crate::ProviderDestinationPolicy::from_settings(settings) {
        result[crate::DESTINATION_POLICY_KEY] = json!(policy);
    }
    result
}

/// Typed operations cannot carry arbitrary settings or SQL identifiers.
#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "camelCase")]
pub enum ProviderChange {
    Connection {
        config: ProviderConnection,
    },
    AddCredential {
        channel: String,
        config: ProviderCredentialInput,
    },
    BillKey {
        channel: String,
        config: ProviderBillKeyInput,
    },
    CredentialStatus {
        channel: String,
        credential: Uuid,
        config: ProviderStatusInput,
    },
    RemoveCredential {
        channel: String,
        credential: Uuid,
    },
    Cache {
        channel: String,
        config: ProviderCacheInput,
    },
    Price {
        channel: String,
        config: Box<ProviderPriceInput>,
    },
    Alias {
        channel: String,
        config: ProviderAliasInput,
    },
    RemoveAlias {
        channel: String,
        alias: String,
    },
    Fallbacks {
        model: String,
        config: ProviderFallbackInput,
    },
}
impl ProviderChange {
    fn capability(&self) -> BusinessCapability {
        match self {
            Self::Connection { .. }
            | Self::AddCredential { .. }
            | Self::BillKey { .. }
            | Self::CredentialStatus { .. }
            | Self::RemoveCredential { .. } => BusinessCapability::ProviderSecretsWrite,
            Self::Cache { .. } | Self::Fallbacks { .. } => BusinessCapability::RoutingWrite,
            Self::Price { .. } | Self::Alias { .. } | Self::RemoveAlias { .. } => {
                BusinessCapability::CatalogWrite
            }
        }
    }
    fn action(&self) -> &'static str {
        match self {
            Self::Connection { .. } => "business.provider.connection",
            Self::AddCredential { .. } => "business.provider.credential.add",
            Self::BillKey { .. } => "business.provider.billing_credential",
            Self::CredentialStatus { .. } => "business.provider.credential.status",
            Self::RemoveCredential { .. } => "business.provider.credential.remove",
            Self::Cache { .. } => "business.provider.cache",
            Self::Price { .. } => "business.provider.price",
            Self::Alias { .. } => "business.provider.alias",
            Self::RemoveAlias { .. } => "business.provider.alias.remove",
            Self::Fallbacks { .. } => "business.provider.fallbacks",
        }
    }
    fn channel(&self) -> Option<&str> {
        match self {
            Self::Connection { config } => Some(&config.name),
            Self::AddCredential { channel, .. }
            | Self::BillKey { channel, .. }
            | Self::CredentialStatus { channel, .. }
            | Self::RemoveCredential { channel, .. }
            | Self::Cache { channel, .. }
            | Self::Price { channel, .. }
            | Self::Alias { channel, .. }
            | Self::RemoveAlias { channel, .. } => Some(channel),
            Self::Fallbacks { .. } => None,
        }
    }
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderMutationResult {
    pub id: Uuid,
    pub resource_id: Option<Uuid>,
    pub version: Option<i64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessChannelPage {
    pub channels: Vec<Channel>,
    pub next_cursor: Option<Uuid>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnectionCheck {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub state: String,
    pub http_status: Option<i32>,
}
/// Only a newly committed intent returns coordinates. A replay never causes
/// another network call, including after a process died with a pending check.
pub struct ProviderCheckStart {
    pub result: ProviderConnectionCheck,
    pub endpoint: Option<crate::TaskProviderEndpoint>,
}
fn unknown() -> WalletError {
    WalletError::NotFound("provider resource is not known".into())
}
fn validate_secret(secret: &str) -> Result<&str, WalletError> {
    let secret = credential_text(secret)?;
    if !(8..=8192).contains(&secret.len()) || secret.chars().any(char::is_control) {
        return Err(WalletError::InvalidInput(
            "provider credential must contain 8 to 8192 bytes without control characters".into(),
        ));
    }
    Ok(secret)
}
async fn owned_channel(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    name: &str,
) -> Result<Uuid, WalletError> {
    let name = channels::channel_name(name)?;
    sqlx::query_scalar(
        "SELECT channel_id FROM oxsum.channels WHERE business_id=$1 AND name=$2 FOR UPDATE",
    )
    .bind(business)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(unknown)
}
async fn result(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<ProviderMutationResult, WalletError> {
    let row = sqlx::query(
        "SELECT resource_id,version FROM oxsum.provider_mutation_results WHERE mutation_id=$1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(ProviderMutationResult {
        id,
        resource_id: row.try_get("resource_id")?,
        version: row.try_get("version")?,
    })
}
async fn model_owned(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    model: &str,
) -> Result<bool, WalletError> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.channel_prices p JOIN oxsum.channels c USING(channel_id) WHERE c.business_id=$1 AND p.model=$2 AND p.mode<>'event') OR EXISTS(SELECT 1 FROM oxsum.model_aliases a JOIN oxsum.channels c USING(channel_id) WHERE c.business_id=$1 AND a.alias=$2 AND EXISTS(SELECT 1 FROM oxsum.channel_prices p WHERE p.channel_id=a.channel_id AND p.model=a.upstream_model AND p.mode<>'event'))")
        .bind(business).bind(model).fetch_one(&mut **tx).await?)
}
impl Db {
    pub async fn begin_provider_check(
        &self,
        token: &str,
        channel: &str,
        key: &str,
        reason: &str,
    ) -> Result<ProviderCheckStart, WalletError> {
        authority::validate(key, Some(reason))?;
        let mut tx = self.pool().begin().await?;
        channels::lock_configuration(&mut tx).await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::ProviderSecretsWrite,
        )
        .await?;
        let detail = json!({"channel":channel,"reason":reason});
        let action = "business.provider.connection_check";
        if let Some(id) =
            authority::replay(&mut tx, business, actor.user_id, key, action, &detail).await?
        {
            let row = sqlx::query("SELECT channel_id,state,http_status FROM oxsum.provider_connection_checks WHERE mutation_id=$1")
                .bind(id).fetch_one(&mut *tx).await?;
            return Ok(ProviderCheckStart {
                result: ProviderConnectionCheck {
                    id,
                    channel_id: row.try_get("channel_id")?,
                    state: row.try_get("state")?,
                    http_status: row.try_get("http_status")?,
                },
                endpoint: None,
            });
        }
        let channel_id = owned_channel(&mut tx, business, channel).await?;
        let row = sqlx::query("SELECT base_url,protocol,settings,(SELECT api_key_sealed FROM oxsum.channel_credentials WHERE channel_id=$1 AND status='active' ORDER BY position LIMIT 1) AS sealed FROM oxsum.channels WHERE channel_id=$1")
            .bind(channel_id).fetch_one(&mut *tx).await?;
        let endpoint = crate::TaskProviderEndpoint {
            base_url: row.try_get("base_url")?,
            protocol: row.try_get("protocol")?,
            destination_policy: crate::ProviderDestinationPolicy::from_settings(
                &row.try_get::<Value, _>("settings")?,
            )?,
            api_key_sealed: row.try_get("sealed")?,
        };
        let receipt =
            authority::record(&mut tx, business, actor.user_id, key, action, &detail).await?;
        sqlx::query(
            "INSERT INTO oxsum.provider_connection_checks(mutation_id,channel_id) VALUES($1,$2)",
        )
        .bind(receipt.id)
        .bind(channel_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ProviderCheckStart {
            result: ProviderConnectionCheck {
                id: receipt.id,
                channel_id,
                state: "pending".into(),
                http_status: None,
            },
            endpoint: Some(endpoint),
        })
    }

    /// Trusted completion of the already-authorized single probe. No response
    /// content is persisted; the first terminal classification is immutable.
    pub async fn finish_provider_check(
        &self,
        id: Uuid,
        state: &str,
        status: Option<u16>,
    ) -> Result<ProviderConnectionCheck, WalletError> {
        if !matches!(
            state,
            "reachable" | "authenticationFailed" | "httpError" | "blocked" | "unreachable"
        ) {
            return Err(WalletError::InvalidInput(
                "invalid provider check state".into(),
            ));
        }
        sqlx::query("UPDATE oxsum.provider_connection_checks SET state=$2,http_status=$3,completed_at=now() WHERE mutation_id=$1 AND state='pending'")
            .bind(id).bind(state).bind(status.map(i32::from)).execute(self.pool()).await?;
        let row = sqlx::query("SELECT channel_id,state,http_status FROM oxsum.provider_connection_checks WHERE mutation_id=$1")
            .bind(id).fetch_one(self.pool()).await?;
        Ok(ProviderConnectionCheck {
            id,
            channel_id: row.try_get("channel_id")?,
            state: row.try_get("state")?,
            http_status: row.try_get("http_status")?,
        })
    }

    /// Check current authority in the same transaction as configuration and audit.
    pub async fn change_business_provider(
        &self,
        token: &str,
        idempotency_key: &str,
        reason: &str,
        change: &ProviderChange,
        secret: &SecretKey,
    ) -> Result<ProviderMutationResult, WalletError> {
        authority::validate(idempotency_key, Some(reason))?;
        let mut tx = self.pool().begin().await?;
        channels::lock_configuration(&mut tx).await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor =
            authority::require_capability(&mut tx, business, token, change.capability()).await?;
        let fingerprint = secret.request_fingerprint(
            json!({"change":change,"reason":reason})
                .to_string()
                .as_bytes(),
        )?;
        // Audit exposes only operation/target/reason and a keyed replay digest.
        // Secrets, provider URLs and arbitrary payload fields never enter it.
        let detail = json!({"target":change.channel(),"reason":reason,"fingerprint":fingerprint});
        let action = change.action();
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return result(&mut tx, id).await;
        }
        let (resource_id, version) = match change {
            ProviderChange::Connection { config } => (
                Some(connection(&mut tx, business, config, secret).await?),
                None,
            ),
            ProviderChange::Fallbacks { model, config } => {
                fallbacks(&mut tx, business, model, &config.fallbacks).await?;
                (None, None)
            }
            _ => {
                let channel = change.channel().ok_or_else(unknown)?;
                let id = owned_channel(&mut tx, business, channel).await?;
                apply_channel_change(&mut tx, id, change, secret).await?
            }
        };
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            idempotency_key,
            action,
            &detail,
        )
        .await?;
        sqlx::query("INSERT INTO oxsum.provider_mutation_results(mutation_id,resource_id,version) VALUES($1,$2,$3)")
            .bind(receipt.id).bind(resource_id).bind(version).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(ProviderMutationResult {
            id: receipt.id,
            resource_id,
            version,
        })
    }
    pub async fn business_channels(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<BusinessChannelPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput(
                "limit must be between 1 and 100".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        channels::lock_configuration(&mut tx).await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::CatalogWrite)
            .await?;
        let mut rows = sqlx::query("SELECT channel_id,name,base_url,protocol,bill_key_last4,settings,created_at FROM oxsum.channels WHERE business_id=$1 AND ($2::uuid IS NULL OR channel_id>$2) ORDER BY channel_id LIMIT $3")
            .bind(business).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let next_cursor = if rows.len() > limit as usize {
            rows.truncate(limit as usize);
            rows.last()
                .map(|row| row.try_get("channel_id"))
                .transpose()?
        } else {
            None
        };
        let mut channels = Vec::new();
        for row in rows {
            let id = row.try_get("channel_id")?;
            let settings: Value = row.try_get("settings")?;
            let settings = redacted_settings(&settings);
            channels.push(Channel {
                id,
                name: row.try_get("name")?,
                base_url: redacted_endpoint(&row.try_get::<String, _>("base_url")?),
                protocol: row.try_get("protocol")?,
                bill_key_last4: row.try_get("bill_key_last4")?,
                settings,
                created_at: row.try_get("created_at")?,
                credentials: credentials(&mut tx, id).await?,
                models: channels::prices_in(&mut tx, id, Some(1)).await?,
            });
        }
        Ok(BusinessChannelPage {
            channels,
            next_cursor,
        })
    }
    pub async fn business_channel_prices(
        &self,
        token: &str,
        name: &str,
    ) -> Result<Vec<ModelPrice>, WalletError> {
        let mut tx = self.pool().begin().await?;
        channels::lock_configuration(&mut tx).await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::CatalogWrite)
            .await?;
        let id = owned_channel(&mut tx, business, name).await?;
        channels::prices_in(&mut tx, id, None).await
    }
    pub async fn business_channel_aliases(
        &self,
        token: &str,
        name: &str,
    ) -> Result<Vec<ModelAlias>, WalletError> {
        let mut tx = self.pool().begin().await?;
        channels::lock_configuration(&mut tx).await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::CatalogWrite)
            .await?;
        let id = owned_channel(&mut tx, business, name).await?;
        let rows=sqlx::query("SELECT alias,upstream_model,created_at FROM oxsum.model_aliases WHERE channel_id=$1 ORDER BY alias").bind(id).fetch_all(&mut *tx).await?;
        rows.into_iter()
            .map(|r| {
                Ok(ModelAlias {
                    alias: r.try_get("alias")?,
                    channel: name.to_owned(),
                    upstream_model: r.try_get("upstream_model")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }
    pub async fn business_model_fallbacks(
        &self,
        token: &str,
        model: &str,
    ) -> Result<Vec<String>, WalletError> {
        let mut tx = self.pool().begin().await?;
        channels::lock_configuration(&mut tx).await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::RoutingWrite)
            .await?;
        let model = channels::model_name(model)?;
        if !model_owned(&mut tx, business, model).await? {
            return Err(unknown());
        }
        Ok(sqlx::query_scalar("SELECT fallback_model FROM oxsum.model_fallbacks WHERE business_id=$1 AND model=$2 ORDER BY position").bind(business).bind(model).fetch_all(&mut *tx).await?)
    }
}
async fn connection(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    config: &ProviderConnection,
    secret: &SecretKey,
) -> Result<Uuid, WalletError> {
    let name = channels::channel_name(&config.name)?;
    let base = channels::base_url_of(&config.base_url)?;
    let parsed =
        url::Url::parse(&base).map_err(|_| WalletError::InvalidInput("invalid baseUrl".into()))?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(WalletError::InvalidInput(
            "baseUrl cannot contain credentials, query or fragment".into(),
        ));
    }
    crate::adapters::known_protocol(&config.protocol)?;
    if let Some(keys) = &config.api_keys {
        if keys.is_empty() || keys.len() > 100 {
            return Err(WalletError::InvalidInput(
                "apiKeys must contain 1 to 100 credentials".into(),
            ));
        }
        for key in keys {
            validate_secret(key)?;
        }
    }
    let old=sqlx::query("SELECT channel_id,business_id,base_url,protocol,settings FROM oxsum.channels WHERE name=$1 FOR UPDATE").bind(name).fetch_optional(&mut **tx).await?;
    let unchanged = old.as_ref().is_some_and(|row| {
        row.get::<String, _>("base_url") == base
            && row.get::<String, _>("protocol") == config.protocol
    });
    let policy = match &config.destination_policy {
        Some(policy) => policy.clone(),
        None if unchanged => crate::ProviderDestinationPolicy::from_settings(
            &old.as_ref()
                .ok_or_else(unknown)?
                .try_get::<Value, _>("settings")?,
        )?,
        None => crate::ProviderDestinationPolicy::default(),
    };
    policy.endpoint(&base)?;
    let id = if let Some(old) = old {
        if old.try_get::<Uuid, _>("business_id")? != business {
            return Err(unknown());
        }
        let changed = old.try_get::<String, _>("base_url")? != base
            || old.try_get::<String, _>("protocol")? != config.protocol;
        if changed && config.api_keys.is_none() {
            return Err(WalletError::InvalidInput(
                "a destination or protocol change requires freshly supplied apiKeys".into(),
            ));
        }
        let id: Uuid = old.try_get("channel_id")?;
        sqlx::query("UPDATE oxsum.channels SET base_url=$2,protocol=$3,updated_at=now(),bill_key_sealed=CASE WHEN $4 THEN NULL ELSE bill_key_sealed END,bill_key_last4=CASE WHEN $4 THEN NULL ELSE bill_key_last4 END WHERE channel_id=$1")
            .bind(id).bind(&base).bind(&config.protocol).bind(changed).execute(&mut **tx).await?;
        id
    } else {
        if config.api_keys.is_none() {
            return Err(WalletError::InvalidInput(
                "a new channel requires apiKeys".into(),
            ));
        }
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.channels(channel_id,business_id,name,base_url,protocol) VALUES($1,$2,$3,$4,$5)")
            .bind(id).bind(business).bind(name).bind(&base).bind(&config.protocol).execute(&mut **tx).await?;
        id
    };
    if let Some(keys) = &config.api_keys {
        sqlx::query("DELETE FROM oxsum.channel_credentials WHERE channel_id=$1")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        for (index, key) in keys.iter().enumerate() {
            insert_credential(tx, id, key, secret, index as i32 + 1).await?;
        }
    }
    sqlx::query("UPDATE oxsum.channels SET settings=jsonb_set(settings,'{destinationPolicy}',$2,true) WHERE channel_id=$1")
        .bind(id).bind(json!(policy)).execute(&mut **tx).await?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.channel_credentials WHERE channel_id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    if count == 0 {
        return Err(WalletError::InvalidInput(
            "channel would have no credentials".into(),
        ));
    }
    Ok(id)
}
async fn insert_credential(
    tx: &mut Transaction<'_, Postgres>,
    channel: Uuid,
    key: &str,
    secret: &SecretKey,
    position: i32,
) -> Result<Uuid, WalletError> {
    let key = validate_secret(key)?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.channel_credentials(credential_id,channel_id,api_key_sealed,api_key_last4,position) VALUES($1,$2,$3,$4,$5)")
        .bind(id).bind(channel).bind(secret.seal(key)?).bind(last_chars(key,LAST4)).bind(position).execute(&mut **tx).await?;
    Ok(id)
}
async fn apply_channel_change(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    change: &ProviderChange,
    secret: &SecretKey,
) -> Result<(Option<Uuid>, Option<i64>), WalletError> {
    match change {
        ProviderChange::AddCredential { config, .. } => {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM oxsum.channel_credentials WHERE channel_id=$1",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
            if count >= 100 {
                return Err(WalletError::InvalidInput(
                    "a channel supports at most 100 credentials".into(),
                ));
            }
            let position:i32=sqlx::query_scalar("SELECT COALESCE(max(position),0)+1 FROM oxsum.channel_credentials WHERE channel_id=$1").bind(id).fetch_one(&mut **tx).await?;
            return Ok((
                Some(insert_credential(tx, id, &config.api_key, secret, position).await?),
                None,
            ));
        }
        ProviderChange::BillKey { config, .. } => {
            let key = config.api_key.as_deref().map(validate_secret).transpose()?;
            sqlx::query("UPDATE oxsum.channels SET bill_key_sealed=$2,bill_key_last4=$3,updated_at=now() WHERE channel_id=$1")
                .bind(id).bind(key.map(|k|secret.seal(k)).transpose()?).bind(key.map(|k|last_chars(k,LAST4))).execute(&mut **tx).await?;
        }
        ProviderChange::CredentialStatus {
            credential, config, ..
        } => {
            if !matches!(config.status.as_str(), "active" | "disabled") {
                return Err(WalletError::InvalidInput(
                    "status must be active or disabled".into(),
                ));
            }
            let n=sqlx::query("UPDATE oxsum.channel_credentials SET status=$3 WHERE channel_id=$1 AND credential_id=$2").bind(id).bind(credential).bind(&config.status).execute(&mut **tx).await?.rows_affected();
            if n == 0 {
                return Err(unknown());
            }
            return Ok((Some(*credential), None));
        }
        ProviderChange::RemoveCredential { credential, .. } => {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.channel_credentials WHERE channel_id=$1 AND credential_id=$2)").bind(id).bind(credential).fetch_one(&mut **tx).await?;
            if !exists {
                return Err(unknown());
            }
            let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.channel_credentials WHERE channel_id=$1 AND credential_id<>$2 AND status='active'").bind(id).bind(credential).fetch_one(&mut **tx).await?;
            if remaining == 0 {
                return Err(WalletError::InvalidInput(
                    "last active credential cannot be removed; disable explicitly instead".into(),
                ));
            }
            sqlx::query(
                "DELETE FROM oxsum.channel_credentials WHERE channel_id=$1 AND credential_id=$2",
            )
            .bind(id)
            .bind(credential)
            .execute(&mut **tx)
            .await?;
            return Ok((Some(*credential), None));
        }
        ProviderChange::Cache { config, .. } => {
            if let Some(policy) = &config.policy {
                cache_policy(policy)?;
            }
            sqlx::query("UPDATE oxsum.channels SET settings=CASE WHEN $2::jsonb IS NULL THEN settings-'semanticCache' ELSE jsonb_set(settings,'{semanticCache}',$2,true) END,updated_at=now() WHERE channel_id=$1")
                .bind(id).bind(&config.policy).execute(&mut **tx).await?;
        }
        ProviderChange::Price { config, .. } => {
            let model = channels::model_name(&config.model)?;
            config.price.validate().map_err(WalletError::InvalidInput)?;
            if !(1..=MAX_WEIGHT).contains(&config.weight) {
                return Err(WalletError::InvalidInput(
                    "weight must be between 1 and 1000".into(),
                ));
            }
            let claimed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.model_aliases WHERE alias=$1 AND channel_id<>$2)").bind(model).bind(id).fetch_one(&mut **tx).await?;
            if claimed {
                return Err(WalletError::Conflict(
                    "model name is already an alias on another channel".into(),
                ));
            }
            let version =
                channels::append_price_in(tx, id, model, &config.price, config.weight).await?;
            return Ok((Some(id), Some(version)));
        }
        ProviderChange::Alias { config, .. } => {
            let alias = channels::model_name(&config.alias)?;
            let model = channels::model_name(&config.upstream_model)?;
            let served:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.channel_prices WHERE channel_id=$1 AND model=$2 AND mode<>'event')").bind(id).bind(model).fetch_one(&mut **tx).await?;
            if !served {
                return Err(WalletError::InvalidInput(
                    "alias target must be served on this channel".into(),
                ));
            }
            let claimed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.model_aliases WHERE alias=$1) OR EXISTS(SELECT 1 FROM oxsum.channel_prices WHERE model=$1 AND channel_id<>$2)").bind(alias).bind(id).fetch_one(&mut **tx).await?;
            if claimed {
                return Err(WalletError::Conflict(
                    "alias name is already claimed".into(),
                ));
            }
            sqlx::query(
                "INSERT INTO oxsum.model_aliases(alias,channel_id,upstream_model) VALUES($1,$2,$3)",
            )
            .bind(alias)
            .bind(id)
            .bind(model)
            .execute(&mut **tx)
            .await?;
        }
        ProviderChange::RemoveAlias { alias, .. } => {
            let n = sqlx::query("DELETE FROM oxsum.model_aliases WHERE channel_id=$1 AND alias=$2")
                .bind(id)
                .bind(alias)
                .execute(&mut **tx)
                .await?
                .rows_affected();
            if n == 0 {
                return Err(unknown());
            }
        }
        ProviderChange::Connection { .. } | ProviderChange::Fallbacks { .. } => {
            return Err(WalletError::InvalidInput(
                "invalid channel operation".into(),
            ));
        }
    }
    Ok((Some(id), None))
}
async fn fallbacks(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    model: &str,
    fallbacks: &[String],
) -> Result<(), WalletError> {
    let model = channels::model_name(model)?;
    if !model_owned(tx, business, model).await? {
        return Err(unknown());
    }
    if fallbacks.len() > 16 {
        return Err(WalletError::InvalidInput(
            "at most 16 fallback models are allowed".into(),
        ));
    }
    let mut names = std::collections::HashSet::new();
    for fallback in fallbacks {
        let name = channels::model_name(fallback)?;
        if name == model || !names.insert(name) || !model_owned(tx, business, name).await? {
            return Err(WalletError::InvalidInput(
                "fallbacks must be distinct owned models, excluding the requested model".into(),
            ));
        }
    }
    let foreign: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM oxsum.model_fallbacks WHERE model=$1 AND business_id<>$2)",
    )
    .bind(model)
    .bind(business)
    .fetch_one(&mut **tx)
    .await?;
    if foreign {
        return Err(unknown());
    }
    sqlx::query("DELETE FROM oxsum.model_fallbacks WHERE model=$1 AND business_id=$2")
        .bind(model)
        .bind(business)
        .execute(&mut **tx)
        .await?;
    for (position, fallback) in fallbacks.iter().enumerate() {
        sqlx::query("INSERT INTO oxsum.model_fallbacks(business_id,model,position,fallback_model) VALUES($1,$2,$3,$4)").bind(business).bind(model).bind(position as i32).bind(fallback.trim()).execute(&mut **tx).await?;
    }
    Ok(())
}
async fn credentials(
    tx: &mut Transaction<'_, Postgres>,
    channel: Uuid,
) -> Result<Vec<crate::ChannelCredential>, WalletError> {
    let rows=sqlx::query("SELECT credential_id,api_key_last4,position,status,cooldown_until,last_used_at,created_at FROM oxsum.channel_credentials WHERE channel_id=$1 ORDER BY position,credential_id").bind(channel).fetch_all(&mut **tx).await?;
    rows.into_iter()
        .map(|r| {
            Ok(crate::ChannelCredential {
                credential_id: r.try_get("credential_id")?,
                api_key_last4: r.try_get("api_key_last4")?,
                position: i64::from(r.try_get::<i32, _>("position")?),
                status: r.try_get("status")?,
                cooldown_until: r.try_get("cooldown_until")?,
                last_used_at: r.try_get("last_used_at")?,
                created_at: r.try_get("created_at")?,
                // Legacy provider error text is not trusted to be free of secrets.
                last_error: None,
            })
        })
        .collect()
}
