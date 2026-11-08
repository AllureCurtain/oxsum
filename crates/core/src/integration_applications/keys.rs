//! Application-only customer key authority with durable one-time-secret receipts.

use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::customers;
use crate::{
    ActingService, ApiKey, ApplicationCustomerArchiveRequest, Db, IntegrationScope, KeyConstraints,
    WalletError, business_staff as authority, integration_credentials, keys,
};

/// Mandatory limits on a newly issued customer consumption key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationCustomerKeyRequest {
    /// Stable application operation key, never a consumption secret.
    pub idempotency_key: String,
    /// Customer-visible label, at most 80 characters.
    pub name: String,
    /// Audited reason for creating the key.
    pub reason: String,
    /// Future expiry bounded by the issuing management credential and 365 days.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Finite positive cumulative settled-plus-pending budget in minor credits.
    pub spend_limit_minor: i64,
    /// Explicit nonempty model allowlist; organization/channel constraints still apply.
    pub model_allowlist: Vec<String>,
    /// Positive request allowance up to 10,000 per minute.
    pub requests_per_minute: i32,
    /// Positive concurrency cap up to 1,000 requests.
    pub max_concurrent_holds: i32,
}

/// Original issuance receipt. The plaintext is absent on every replay.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCustomerKeyResult {
    /// Original key metadata, without subsequent mutations.
    pub key: ApiKey,
    /// Plaintext only from the first committed issuance; never persisted in receipts.
    pub secret: Option<String>,
}

/// Current metadata for only this application's issued customer keys.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCustomerKeyPage {
    /// Keys sorted by identifier, including expired/revoked keys.
    pub keys: Vec<ApiKey>,
    /// Last returned identifier when more results exist.
    pub next_cursor: Option<Uuid>,
}

fn invalid_receipt(error: impl std::fmt::Display) -> WalletError {
    WalletError::Misconfigured(format!("invalid stored application key receipt: {error}"))
}

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    organization: Uuid,
) -> Result<Uuid, WalletError> {
    integration_credentials::require_integration(
        tx,
        service,
        IntegrationScope::CustomerKeysWrite,
        Some(organization),
    )
    .await?;
    service
        .application_id
        .ok_or_else(|| WalletError::Forbidden("application credential required".into()))
}

fn normalize(
    request: &ApplicationCustomerKeyRequest,
    organization: Uuid,
) -> Result<serde_json::Value, WalletError> {
    authority::validate(&request.idempotency_key, Some(&request.reason))?;
    if request.name.trim().is_empty()
        || request.name.trim().chars().count() > 80
        || request.name.chars().any(char::is_control)
        || request.spend_limit_minor <= 0
        || !(1..=10000).contains(&request.requests_per_minute)
        || !(1..=1000).contains(&request.max_concurrent_holds)
        || request.model_allowlist.is_empty()
        || request.model_allowlist.len() > 100
    {
        return Err(WalletError::InvalidInput(
            "key requires a name, explicit models and finite positive limits".into(),
        ));
    }
    let mut models = request.model_allowlist.clone();
    keys::validate_model_allowlist(&models)?;
    models.sort();
    models.dedup();
    if models.len() != request.model_allowlist.len()
        || models
            .iter()
            .any(|model| model.trim() != model || model.chars().any(char::is_control))
    {
        return Err(WalletError::InvalidInput(
            "model names must be distinct and normalized".into(),
        ));
    }
    Ok(
        serde_json::json!({"organizationId":organization,"name":request.name.trim(),"reason":request.reason.trim(),
        "expiresAt":request.expires_at.format(&time::format_description::well_known::Rfc3339).map_err(invalid_receipt)?,
        "spendLimitMinor":request.spend_limit_minor,"modelAllowlist":models,"requestsPerMinute":request.requests_per_minute,"maxConcurrentHolds":request.max_concurrent_holds}),
    )
}

impl Db {
    /// Issue one bounded consumption key within an application's customer namespace.
    /// Replays recheck authority and return the original metadata without the secret.
    pub async fn create_application_customer_key(
        &self,
        service: &ActingService,
        organization: Uuid,
        request: &ApplicationCustomerKeyRequest,
    ) -> Result<ApplicationCustomerKeyResult, WalletError> {
        let terms = normalize(request, organization)?;
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        let app = authorize(&mut tx, service, organization).await?;
        if let Some(result) =
            customers::replay_in(&mut tx, app, &request.idempotency_key, "key.create", &terms)
                .await?
        {
            let key = serde_json::from_value(result).map_err(invalid_receipt)?;
            tx.commit().await?;
            return Ok(ApplicationCustomerKeyResult { key, secret: None });
        }
        // Parent/customer locks precede key insertion and its foreign-key locks.
        let unblocked: bool = sqlx::query_scalar("SELECT suspended_at IS NULL AND dunning_suspended_at IS NULL FROM oxsum.organizations WHERE organization_id=$1 FOR SHARE")
            .bind(organization).fetch_one(&mut *tx).await?;
        let active: bool = sqlx::query_scalar("SELECT state='active' FROM oxsum.customer_profiles WHERE organization_id=$1 AND business_id=$2 FOR SHARE")
            .bind(organization).bind(service.business_id).fetch_one(&mut *tx).await?;
        let active = active && unblocked;
        if !active {
            return Err(WalletError::Forbidden(
                "customer is not active for key issuance".into(),
            ));
        }
        let expiry: OffsetDateTime = sqlx::query_scalar(
            "SELECT expires_at FROM oxsum.integration_credentials WHERE credential_id=$1",
        )
        .bind(service.credential_id)
        .fetch_one(&mut *tx)
        .await?;
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        if request.expires_at <= now
            || request.expires_at > now + Duration::days(365)
            || request.expires_at > expiry
        {
            return Err(WalletError::InvalidInput(
                "key expiry must be future and within the management credential and 365 days"
                    .into(),
            ));
        }
        let mut models = request.model_allowlist.clone();
        models.sort();
        let constraints = KeyConstraints {
            spend_limit_minor: Some(request.spend_limit_minor),
            model_allowlist: Some(models),
            requests_per_minute: Some(request.requests_per_minute),
            max_concurrent_holds: Some(request.max_concurrent_holds),
            ..KeyConstraints::default()
        };
        constraints.validate()?;
        let created = keys::insert(
            &mut tx,
            organization,
            Some(request.name.trim().to_owned()),
            Some(request.expires_at),
            None,
            constraints,
        )
        .await?;
        sqlx::query("INSERT INTO oxsum.application_customer_keys(key_id,business_id,application_id,organization_id,credential_id) VALUES($1,$2,$3,$4,$5)")
            .bind(created.key.id).bind(service.business_id).bind(app).bind(organization).bind(service.credential_id).execute(&mut *tx).await?;
        customers::record_in(
            &mut tx,
            service,
            app,
            &request.idempotency_key,
            "key.create",
            &terms,
            &serde_json::json!(created.key),
            organization,
        )
        .await?;
        tx.commit().await?;
        Ok(ApplicationCustomerKeyResult {
            key: created.key,
            secret: Some(created.secret),
        })
    }

    /// Read only this application's key metadata for its own customer.
    pub async fn application_customer_keys(
        &self,
        service: &ActingService,
        organization: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<ApplicationCustomerKeyPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput("limit must be 1 to 100".into()));
        }
        let mut tx = self.pool().begin().await?;
        let app = authorize(&mut tx, service, organization).await?;
        let rows=sqlx::query(&format!("SELECT {} FROM oxsum.api_keys WHERE organization_id=$1 AND ($3::uuid IS NULL OR key_id>$3) AND key_id IN(SELECT key_id FROM oxsum.application_customer_keys WHERE application_id=$2 AND organization_id=$1) ORDER BY key_id LIMIT $4",keys::KEY_COLS))
            .bind(organization).bind(app).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let more = rows.len() > limit as usize;
        let keys = rows
            .iter()
            .take(limit as usize)
            .map(keys::key_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if more {
            keys.last().map(|key| key.id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(ApplicationCustomerKeyPage { keys, next_cursor })
    }

    /// Revoke an application-issued key, including after customer archive. Replays
    /// preserve their original receipt and never bypass current management authority.
    pub async fn revoke_application_customer_key(
        &self,
        service: &ActingService,
        organization: Uuid,
        key: Uuid,
        request: &ApplicationCustomerArchiveRequest,
    ) -> Result<ApiKey, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let terms = serde_json::json!({"organizationId":organization,"keyId":key,"reason":request.reason.trim()});
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        let app = authorize(&mut tx, service, organization).await?;
        if let Some(result) =
            customers::replay_in(&mut tx, app, &request.idempotency_key, "key.revoke", &terms)
                .await?
        {
            let result = serde_json::from_value(result).map_err(invalid_receipt)?;
            tx.commit().await?;
            return Ok(result);
        }
        // Take the customer parent before the key row, matching human key
        // management and preventing the audit FK from inverting that order.
        sqlx::query("SELECT organization_id FROM oxsum.organizations WHERE organization_id=$1 FOR KEY SHARE")
            .bind(organization).fetch_one(&mut *tx).await?;
        let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.application_customer_keys WHERE application_id=$1 AND organization_id=$2 AND key_id=$3)")
            .bind(app).bind(organization).bind(key).fetch_one(&mut *tx).await?;
        if !owned {
            return Err(WalletError::NotFound("application key is not known".into()));
        }
        let row=sqlx::query(&format!("UPDATE oxsum.api_keys SET revoked_at=COALESCE(revoked_at,clock_timestamp()) WHERE organization_id=$1 AND key_id=$2 RETURNING {}",keys::KEY_COLS))
            .bind(organization).bind(key).fetch_one(&mut *tx).await?;
        let result = keys::key_from_row(&row)?;
        customers::record_in(
            &mut tx,
            service,
            app,
            &request.idempotency_key,
            "key.revoke",
            &terms,
            &serde_json::json!(result),
            organization,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
}
