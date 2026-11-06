//! Business merchant authority and encrypted immutable signing configurations.
use crate::business_staff as authority;
use crate::{BusinessCapability, Db, ProviderDestinationPolicy, SecretKey, WalletError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MerchantRail {
    Hyperswitch,
    Stripe,
    Airwallex,
}
impl MerchantRail {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hyperswitch => "hyperswitch",
            Self::Stripe => "stripe",
            Self::Airwallex => "airwallex",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, WalletError> {
        match value {
            "hyperswitch" => Ok(Self::Hyperswitch),
            "stripe" => Ok(Self::Stripe),
            "airwallex" => Ok(Self::Airwallex),
            _ => Err(invalid("invalid merchant rail")),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MerchantMode {
    Test,
    Live,
}
impl MerchantMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Live => "live",
        }
    }
    pub(crate) fn parse(value: &str) -> Result<Self, WalletError> {
        match value {
            "test" => Ok(Self::Test),
            "live" => Ok(Self::Live),
            _ => Err(invalid("invalid merchant mode")),
        }
    }
}

/// Write-only credential input. Deliberately has no Debug implementation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MerchantVersionInput {
    pub api_url: String,
    pub api_key: String,
    pub webhook_secret: String,
    pub client_id: Option<String>,
    pub publishable_key: Option<String>,
    pub sdk_url: Option<String>,
    pub checkout_url: Option<String>,
    #[serde(default)]
    pub destination_policy: ProviderDestinationPolicy,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MerchantCreateRequest {
    pub idempotency_key: String,
    pub reason: String,
    pub name: String,
    pub rail: MerchantRail,
    pub mode: MerchantMode,
    pub account_id: String,
    pub config: MerchantVersionInput,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MerchantVersionRequest {
    pub idempotency_key: String,
    pub reason: String,
    pub config: MerchantVersionInput,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MerchantStatusRequest {
    pub idempotency_key: String,
    pub reason: String,
    pub enabled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MerchantMutationResult {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub version_id: Uuid,
    pub revision: i64,
    pub enabled: bool,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MerchantVersion {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub revision: i64,
    pub api_url: String,
    pub publishable_key: Option<String>,
    pub sdk_url: Option<String>,
    pub checkout_url: Option<String>,
    pub destination_policy: ProviderDestinationPolicy,
    pub source: String,
    pub validation_status: &'static str,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Merchant {
    pub id: Uuid,
    pub business_id: Uuid,
    pub name: String,
    pub rail: MerchantRail,
    pub mode: MerchantMode,
    pub account_id: String,
    pub connector: &'static str,
    pub support_status: &'static str,
    pub enabled: bool,
    pub current_version: MerchantVersion,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MerchantPage {
    pub merchants: Vec<Merchant>,
    pub next_cursor: Option<Uuid>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MerchantVersionPage {
    pub versions: Vec<MerchantVersion>,
    pub next_cursor: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct Credentials {
    api_key: String,
    webhook_secret: String,
    client_id: Option<String>,
}
/// Plaintext is opened only for a trusted server-side provider operation. This
/// type deliberately implements neither Serialize nor Debug.
pub struct MerchantSigningContext {
    pub merchant_id: Uuid,
    pub business_id: Uuid,
    pub rail: MerchantRail,
    pub mode: MerchantMode,
    pub account_id: String,
    pub enabled: bool,
    pub version: MerchantVersion,
    credentials: Credentials,
}
impl MerchantSigningContext {
    pub fn api_key(&self) -> &str {
        &self.credentials.api_key
    }
    pub fn webhook_secret(&self) -> &str {
        &self.credentials.webhook_secret
    }
    pub fn client_id(&self) -> Option<&str> {
        self.credentials.client_id.as_deref()
    }
}
fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}
fn unknown() -> WalletError {
    WalletError::NotFound("unknown merchant".into())
}
fn text(value: &str, min: usize, max: usize) -> bool {
    value.len() >= min
        && value.len() <= max
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn public_url(value: &str) -> Result<(), WalletError> {
    ProviderDestinationPolicy::default().endpoint(value)?;
    Ok(())
}

impl MerchantVersionInput {
    fn validate(&self, rail: MerchantRail, mode: MerchantMode) -> Result<(), WalletError> {
        if !text(&self.api_key, 8, 8192) || !text(&self.webhook_secret, 8, 8192) {
            return Err(invalid(
                "merchant secrets must contain 8..8192 characters without surrounding whitespace or controls",
            ));
        }
        self.destination_policy.endpoint(&self.api_url)?;
        if self.client_id.as_deref().is_some_and(|s| !text(s, 1, 200))
            || self
                .publishable_key
                .as_deref()
                .is_some_and(|s| !text(s, 1, 200))
        {
            return Err(invalid("invalid merchant public key or client identifier"));
        }
        match rail {
            MerchantRail::Hyperswitch => {
                if self.client_id.is_some()
                    || self.checkout_url.is_some()
                    || self.publishable_key.is_none()
                    || self.sdk_url.is_none()
                {
                    return Err(invalid(
                        "Hyperswitch requires publishableKey and sdkUrl; clientId and checkoutUrl are not accepted",
                    ));
                }
            }
            MerchantRail::Stripe => {
                if self.client_id.is_some()
                    || self.publishable_key.is_some()
                    || self.sdk_url.is_some()
                    || self.checkout_url.is_some()
                {
                    return Err(invalid(
                        "direct Stripe accepts only API and webhook credentials",
                    ));
                }
            }
            MerchantRail::Airwallex => {
                if self.client_id.is_none()
                    || self.checkout_url.is_none()
                    || self.publishable_key.is_some()
                    || self.sdk_url.is_some()
                {
                    return Err(invalid(
                        "Airwallex requires clientId and checkoutUrl; publishableKey and sdkUrl are not accepted",
                    ));
                }
            }
        }
        for url in [&self.sdk_url, &self.checkout_url].into_iter().flatten() {
            public_url(url)?;
        }
        // Recognizable provider key prefixes cannot contradict declared mode.
        // Shape checks do not assert that a credential is accepted externally.
        for key in [Some(self.api_key.as_str()), self.publishable_key.as_deref()]
            .into_iter()
            .flatten()
        {
            let test = key.starts_with("sk_test_")
                || key.starts_with("pk_test_")
                || key.starts_with("snd_")
                || key.starts_with("pk_snd_");
            let live = key.starts_with("sk_live_")
                || key.starts_with("pk_live_")
                || key.starts_with("live_");
            if (test && mode == MerchantMode::Live) || (live && mode == MerchantMode::Test) {
                return Err(invalid(
                    "credential key prefix conflicts with merchant test/live mode",
                ));
            }
        }
        Ok(())
    }
}
fn version(row: &PgRow) -> Result<MerchantVersion, WalletError> {
    Ok(MerchantVersion {
        id: row.try_get("version_id")?,
        merchant_id: row.try_get("merchant_id")?,
        revision: row.try_get("revision")?,
        api_url: row.try_get("api_url")?,
        publishable_key: row.try_get("publishable_key")?,
        sdk_url: row.try_get("sdk_url")?,
        checkout_url: row.try_get("checkout_url")?,
        destination_policy: serde_json::from_value(row.try_get("destination_policy")?)
            .map_err(|_| invalid("invalid stored merchant destination policy"))?,
        source: row.try_get("source")?,
        validation_status: "locallyValidated",
        created_at: row.try_get("created_at")?,
    })
}
fn merchant(row: &PgRow) -> Result<Merchant, WalletError> {
    let rail = MerchantRail::parse(&row.try_get::<String, _>("rail")?)?;
    Ok(Merchant {
        id: row.try_get("merchant_id")?,
        business_id: row.try_get("business_id")?,
        name: row.try_get("name")?,
        rail,
        mode: MerchantMode::parse(&row.try_get::<String, _>("mode")?)?,
        account_id: row.try_get("account_id")?,
        connector: if rail == MerchantRail::Hyperswitch {
            "stripe"
        } else {
            "direct"
        },
        support_status: if rail == MerchantRail::Hyperswitch {
            "primary"
        } else {
            "compatibility"
        },
        enabled: row.try_get("enabled")?,
        current_version: version(row)?,
    })
}
async fn insert_version(
    tx: &mut Transaction<'_, Postgres>,
    merchant: Uuid,
    id: Uuid,
    revision: i64,
    input: &MerchantVersionInput,
    secret: &SecretKey,
) -> Result<(), WalletError> {
    let sealed = secret.seal(
        &serde_json::to_string(&Credentials {
            api_key: input.api_key.clone(),
            webhook_secret: input.webhook_secret.clone(),
            client_id: input.client_id.clone(),
        })
        .map_err(|_| invalid("invalid merchant credentials"))?,
    )?;
    sqlx::query("INSERT INTO oxsum.merchant_versions(version_id,merchant_id,revision,api_url,publishable_key,sdk_url,checkout_url,destination_policy,credentials_sealed,source) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'owner')")
        .bind(id).bind(merchant).bind(revision).bind(&input.api_url).bind(&input.publishable_key).bind(&input.sdk_url).bind(&input.checkout_url).bind(json!(input.destination_policy)).bind(sealed).execute(&mut **tx).await?;
    Ok(())
}
async fn result(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<MerchantMutationResult, WalletError> {
    let row = sqlx::query("SELECT merchant_id,version_id,revision,enabled FROM oxsum.merchant_mutation_results WHERE mutation_id=$1").bind(id).fetch_one(&mut **tx).await?;
    Ok(MerchantMutationResult {
        id,
        merchant_id: row.try_get("merchant_id")?,
        version_id: row.try_get("version_id")?,
        revision: row.try_get("revision")?,
        enabled: row.try_get("enabled")?,
    })
}
async fn record_result(
    tx: &mut Transaction<'_, Postgres>,
    result: &MerchantMutationResult,
) -> Result<(), WalletError> {
    sqlx::query("INSERT INTO oxsum.merchant_mutation_results(mutation_id,merchant_id,version_id,revision,enabled) VALUES($1,$2,$3,$4,$5)")
        .bind(result.id).bind(result.merchant_id).bind(result.version_id).bind(result.revision).bind(result.enabled).execute(&mut **tx).await?;
    Ok(())
}
fn page_limit(limit: i64) -> Result<(), WalletError> {
    if !(1..=100).contains(&limit) {
        return Err(invalid("limit must be 1..100"));
    }
    Ok(())
}
impl Db {
    /// Owner authority, version, immutable receipt and audit share one commit.
    pub async fn create_merchant(
        &self,
        token: &str,
        request: &MerchantCreateRequest,
        secret: &SecretKey,
    ) -> Result<MerchantMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::MerchantSecretsWrite,
        )
        .await?;
        let fingerprint = secret.request_fingerprint(json!(request).to_string().as_bytes())?;
        let detail = json!({"reason":request.reason,"fingerprint":fingerprint});
        let action = "business.merchant.create";
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
            return result(&mut tx, id).await;
        }
        if !text(&request.name, 1, 80)
            || request.name.split('-').any(|p| {
                p.is_empty()
                    || !p
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
            || !text(&request.account_id, 1, 200)
        {
            return Err(invalid("invalid merchant name or account identifier"));
        }
        request.config.validate(request.rail, request.mode)?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_accounts WHERE business_id=$1 AND (name=$2 OR (rail=$3 AND mode=$4 AND account_id=$5)))")
            .bind(business).bind(&request.name).bind(request.rail.as_str()).bind(request.mode.as_str()).bind(&request.account_id).fetch_one(&mut *tx).await?;
        if exists {
            return Err(WalletError::Conflict(
                "merchant name or account already exists in this business and mode".into(),
            ));
        }
        let merchant_id = Uuid::new_v4();
        let version_id = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.merchant_accounts(merchant_id,business_id,name,rail,mode,account_id,current_version_id) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(merchant_id).bind(business).bind(&request.name).bind(request.rail.as_str()).bind(request.mode.as_str()).bind(&request.account_id).bind(version_id).execute(&mut *tx).await?;
        insert_version(&mut tx, merchant_id, version_id, 1, &request.config, secret).await?;
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let result = MerchantMutationResult {
            id: receipt.id,
            merchant_id,
            version_id,
            revision: 1,
            enabled: false,
        };
        record_result(&mut tx, &result).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn rotate_merchant(
        &self,
        token: &str,
        merchant_id: Uuid,
        request: &MerchantVersionRequest,
        secret: &SecretKey,
    ) -> Result<MerchantMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::MerchantSecretsWrite,
        )
        .await?;
        let fingerprint = secret.request_fingerprint(json!(request).to_string().as_bytes())?;
        let detail =
            json!({"merchantId":merchant_id,"reason":request.reason,"fingerprint":fingerprint});
        let action = "business.merchant.rotate";
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
            return result(&mut tx, id).await;
        }
        let row=sqlx::query("SELECT m.rail,m.mode,m.enabled,v.revision FROM oxsum.merchant_accounts m JOIN oxsum.merchant_versions v ON v.version_id=m.current_version_id WHERE m.business_id=$1 AND m.merchant_id=$2 FOR UPDATE OF m")
            .bind(business).bind(merchant_id).fetch_optional(&mut *tx).await?.ok_or_else(unknown)?;
        request.config.validate(
            MerchantRail::parse(&row.try_get::<String, _>("rail")?)?,
            MerchantMode::parse(&row.try_get::<String, _>("mode")?)?,
        )?;
        let revision: i64 = row
            .try_get::<i64, _>("revision")?
            .checked_add(1)
            .ok_or_else(|| invalid("merchant version exhausted"))?;
        let version_id = Uuid::new_v4();
        insert_version(
            &mut tx,
            merchant_id,
            version_id,
            revision,
            &request.config,
            secret,
        )
        .await?;
        sqlx::query(
            "UPDATE oxsum.merchant_accounts SET current_version_id=$2 WHERE merchant_id=$1",
        )
        .bind(merchant_id)
        .bind(version_id)
        .execute(&mut *tx)
        .await?;
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let result = MerchantMutationResult {
            id: receipt.id,
            merchant_id,
            version_id,
            revision,
            enabled: row.try_get("enabled")?,
        };
        record_result(&mut tx, &result).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn set_merchant_enabled(
        &self,
        token: &str,
        merchant_id: Uuid,
        request: &MerchantStatusRequest,
    ) -> Result<MerchantMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::MerchantSecretsWrite,
        )
        .await?;
        let detail =
            json!({"merchantId":merchant_id,"reason":request.reason,"enabled":request.enabled});
        let action = "business.merchant.status";
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
            return result(&mut tx, id).await;
        }
        let row=sqlx::query("SELECT m.current_version_id,v.revision FROM oxsum.merchant_accounts m JOIN oxsum.merchant_versions v ON v.version_id=m.current_version_id WHERE m.business_id=$1 AND m.merchant_id=$2 FOR UPDATE OF m")
            .bind(business).bind(merchant_id).fetch_optional(&mut *tx).await?.ok_or_else(unknown)?;
        sqlx::query("UPDATE oxsum.merchant_accounts SET enabled=$2 WHERE merchant_id=$1")
            .bind(merchant_id)
            .bind(request.enabled)
            .execute(&mut *tx)
            .await?;
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let result = MerchantMutationResult {
            id: receipt.id,
            merchant_id,
            version_id: row.try_get("current_version_id")?,
            revision: row.try_get("revision")?,
            enabled: request.enabled,
        };
        record_result(&mut tx, &result).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn business_merchants(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<MerchantPage, WalletError> {
        page_limit(limit)?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let rows=sqlx::query("SELECT m.business_id,m.name,m.rail,m.mode,m.account_id,m.enabled,v.* FROM oxsum.merchant_accounts m JOIN oxsum.merchant_versions v ON v.version_id=m.current_version_id WHERE m.business_id=$1 AND ($2::uuid IS NULL OR m.merchant_id>$2) ORDER BY m.merchant_id LIMIT $3")
            .bind(business).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut merchants = rows.iter().map(merchant).collect::<Result<Vec<_>, _>>()?;
        let more = merchants.len() > limit as usize;
        merchants.truncate(limit as usize);
        let next_cursor = if more {
            merchants.last().map(|m| m.id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(MerchantPage {
            merchants,
            next_cursor,
        })
    }
    pub async fn business_merchant_versions(
        &self,
        token: &str,
        merchant_id: Uuid,
        after: Option<i64>,
        limit: i64,
    ) -> Result<MerchantVersionPage, WalletError> {
        page_limit(limit)?;
        if after.is_some_and(|n| n < 0) {
            return Err(invalid("after must be nonnegative"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_accounts WHERE business_id=$1 AND merchant_id=$2)").bind(business).bind(merchant_id).fetch_one(&mut *tx).await?;
        if !exists {
            return Err(unknown());
        }
        let rows=sqlx::query("SELECT * FROM oxsum.merchant_versions WHERE merchant_id=$1 AND revision>$2 ORDER BY revision LIMIT $3").bind(merchant_id).bind(after.unwrap_or(0)).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut versions = rows.iter().map(version).collect::<Result<Vec<_>, _>>()?;
        let more = versions.len() > limit as usize;
        versions.truncate(limit as usize);
        let next_cursor = if more {
            versions.last().map(|v| v.revision)
        } else {
            None
        };
        tx.commit().await?;
        Ok(MerchantVersionPage {
            versions,
            next_cursor,
        })
    }
    /// The caller selects both coordinates before inspecting untrusted payloads.
    /// Disablement affects future checkout selection, not historical signatures.
    pub async fn merchant_signing_context(
        &self,
        merchant_id: Uuid,
        version_id: Uuid,
        secret: &SecretKey,
    ) -> Result<Option<MerchantSigningContext>, WalletError> {
        let row=sqlx::query("SELECT m.business_id,m.name,m.rail,m.mode,m.account_id,m.enabled,v.* FROM oxsum.merchant_accounts m JOIN oxsum.merchant_versions v USING(merchant_id) WHERE m.merchant_id=$1 AND v.version_id=$2")
            .bind(merchant_id).bind(version_id).fetch_optional(self.pool()).await?;
        row.as_ref()
            .map(|row| {
                let credentials = serde_json::from_str(
                    &secret.open(&row.try_get::<String, _>("credentials_sealed")?)?,
                )
                .map_err(|_| invalid("invalid stored merchant credentials"))?;
                Ok(MerchantSigningContext {
                    merchant_id,
                    business_id: row.try_get("business_id")?,
                    rail: MerchantRail::parse(&row.try_get::<String, _>("rail")?)?,
                    mode: MerchantMode::parse(&row.try_get::<String, _>("mode")?)?,
                    account_id: row.try_get("account_id")?,
                    enabled: row.try_get("enabled")?,
                    version: version(row)?,
                    credentials,
                })
            })
            .transpose()
    }
    pub async fn has_merchants(&self) -> Result<bool, WalletError> {
        Ok(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_accounts)")
                .fetch_one(self.pool())
                .await?,
        )
    }
    pub async fn check_merchant_secrets(&self, secret: &SecretKey) -> Result<(), WalletError> {
        // Include historical versions: in-flight callbacks still depend on them.
        let records: Vec<String> =
            sqlx::query_scalar("SELECT credentials_sealed FROM oxsum.merchant_versions")
                .fetch_all(self.pool())
                .await?;
        for record in records {
            let _: Credentials = serde_json::from_str(&secret.open(&record)?)
                .map_err(|_| invalid("invalid stored merchant credentials"))?;
        }
        Ok(())
    }
}

/// Explicit default-business bootstrap from deployment configuration. Raw
/// credentials have no serializer/debug output and never enter audit.
#[derive(Clone)]
pub struct MerchantBootstrap {
    pub rail: MerchantRail,
    pub mode: MerchantMode,
    pub account_id: String,
    pub config: MerchantVersionInput,
}
impl std::fmt::Debug for MerchantBootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MerchantBootstrap([credentials redacted])")
    }
}
impl Db {
    /// Seed only once per environment rail; later environment drift never
    /// replaces owner-managed versions, identity or enablement.
    pub async fn bootstrap_merchant(
        &self,
        input: &MerchantBootstrap,
        secret: &SecretKey,
    ) -> Result<(Uuid, Uuid), WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        if let Some(row) = sqlx::query(
            "SELECT merchant_id,version_id FROM oxsum.merchant_environment_bootstrap WHERE rail=$1",
        )
        .bind(input.rail.as_str())
        .fetch_optional(&mut *tx)
        .await?
        {
            return Ok((row.try_get("merchant_id")?, row.try_get("version_id")?));
        }
        input.config.validate(input.rail, input.mode)?;
        if !text(&input.account_id, 1, 200) {
            return Err(invalid("invalid environment merchant account"));
        }
        let existing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_accounts WHERE business_id=$1 AND rail=$2 AND mode=$3 AND account_id=$4)").bind(business).bind(input.rail.as_str()).bind(input.mode.as_str()).bind(&input.account_id).fetch_one(&mut *tx).await?;
        if existing {
            return Err(WalletError::Conflict("environment merchant identity already exists; configure the explicit merchant callback instead".into()));
        }
        let merchant = Uuid::new_v4();
        let version = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.merchant_accounts(merchant_id,business_id,name,rail,mode,account_id,current_version_id,enabled) VALUES($1,$2,$3,$4,$5,$6,$7,true)").bind(merchant).bind(business).bind(format!("environment-{}",input.rail.as_str())).bind(input.rail.as_str()).bind(input.mode.as_str()).bind(&input.account_id).bind(version).execute(&mut *tx).await?;
        let sealed = secret.seal(
            &serde_json::to_string(&Credentials {
                api_key: input.config.api_key.clone(),
                webhook_secret: input.config.webhook_secret.clone(),
                client_id: input.config.client_id.clone(),
            })
            .map_err(|_| invalid("invalid merchant credentials"))?,
        )?;
        sqlx::query("INSERT INTO oxsum.merchant_versions(version_id,merchant_id,revision,api_url,publishable_key,sdk_url,checkout_url,destination_policy,credentials_sealed,source) VALUES($1,$2,1,$3,$4,$5,$6,$7,$8,'environment')").bind(version).bind(merchant).bind(&input.config.api_url).bind(&input.config.publishable_key).bind(&input.config.sdk_url).bind(&input.config.checkout_url).bind(json!(input.config.destination_policy)).bind(sealed).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO oxsum.merchant_environment_bootstrap(rail,merchant_id,version_id) VALUES($1,$2,$3)").bind(input.rail.as_str()).bind(merchant).bind(version).execute(&mut *tx).await?;
        // Explicit bootstrap attributes existing single-rail commitments to the
        // deployment's declared merchant. Preserve their UUID/amount/ledger key.
        sqlx::query("INSERT INTO oxsum.merchant_payment_bindings(payment_id,merchant_id,version_id,organization_id,amount_minor,currency,purpose,legacy_unscoped_key) SELECT d.deposit_id,$1,$2,d.organization_id,d.amount_minor,'USD','legacyDeposit',true FROM oxsum.deposits d JOIN oxsum.customer_profiles c USING(organization_id) WHERE d.rail=$3 AND c.business_id=$4 ON CONFLICT DO NOTHING").bind(merchant).bind(version).bind(input.rail.as_str()).bind(business).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO oxsum.merchant_payment_refs(payment_id,merchant_id,provider_payment_id) SELECT d.deposit_id,$1,CASE d.rail WHEN 'stripe' THEN d.meta->>'stripeSessionId' WHEN 'airwallex' THEN d.meta->>'airwallexIntentId' ELSE d.meta->>'hyperswitchPaymentId' END FROM oxsum.deposits d JOIN oxsum.merchant_payment_bindings b ON b.payment_id=d.deposit_id WHERE b.merchant_id=$1 AND coalesce(CASE d.rail WHEN 'stripe' THEN d.meta->>'stripeSessionId' WHEN 'airwallex' THEN d.meta->>'airwallexIntentId' ELSE d.meta->>'hyperswitchPaymentId' END,'')<>'' ORDER BY d.created_at ON CONFLICT DO NOTHING").bind(merchant).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok((merchant, version))
    }
    /// Legacy URL scope comes from a server-owned bootstrap record, never the body.
    pub async fn environment_merchant(
        &self,
        rail: MerchantRail,
        secret: &SecretKey,
    ) -> Result<Option<MerchantSigningContext>, WalletError> {
        let row=sqlx::query("SELECT m.merchant_id,m.current_version_id FROM oxsum.merchant_environment_bootstrap e JOIN oxsum.merchant_accounts m USING(merchant_id) WHERE e.rail=$1").bind(rail.as_str()).fetch_optional(self.pool()).await?;
        match row {
            Some(row) => {
                self.merchant_signing_context(
                    row.try_get("merchant_id")?,
                    row.try_get("current_version_id")?,
                    secret,
                )
                .await
            }
            None => Ok(None),
        }
    }
    /// The trusted compatibility URL may verify retained versions for its one
    /// bootstrap merchant. This lookup never accepts a payload-selected secret.
    pub async fn merchant_signing_versions(
        &self,
        merchant: Uuid,
        secret: &SecretKey,
    ) -> Result<Vec<MerchantSigningContext>, WalletError> {
        let versions:Vec<Uuid>=sqlx::query_scalar("SELECT version_id FROM oxsum.merchant_versions WHERE merchant_id=$1 ORDER BY revision DESC").bind(merchant).fetch_all(self.pool()).await?;
        let mut result = Vec::new();
        for version in versions {
            if let Some(context) = self
                .merchant_signing_context(merchant, version, secret)
                .await?
            {
                result.push(context);
            }
        }
        Ok(result)
    }
}
