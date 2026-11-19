//! Publication and current-customer acceptance with existing authority lock order.
use super::{
    BillingPolicyAccept, BillingPolicyAcceptance, BillingPolicyPublish, BillingPolicyVersion,
    CustomerBillingPolicy,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{BusinessCapability, Db, WalletError, business_staff as authority, orgs, wallet};

/// Exact scope of the initial exception offer. Other surfaces retain legacy behavior.
pub const POLICY_SCOPE: &str =
    "OpenAI-compatible nonstreaming chat text only; other surfaces retain legacy terms";
/// Historical behavior without fabricated consent.
pub const LEGACY_DISCLOSURE: &str = "Legacy estimation: missing completion usage may charge locally estimated input; cancellation may charge a bounded forwarded prefix. No explicit exception-policy acceptance was recorded.";
/// The provider-reported implementation is enabled only when its recovery writer exists.
pub const PROVIDER_REPORTED_READY: bool = false;

fn disclosure(implementation: &str, window: i64) -> Result<String, WalletError> {
    match implementation {
        "legacy-estimation-v1" => Ok("Explicit legacy estimation: method oxsum-o200k-text-v1 (tiktoken-rs 0.12.1 o200k_base encode_ordinary over each normalized message text slice). If tokenizer construction fails, use UTF-8 byte count plus 16 units per slice. Completed responses without usage use input only; cancellation may additionally estimate the first 256 KiB of forwarded visible text, within the pinned retail rates, discount and admission ceiling. Hidden reasoning/cache/media/tool quantities are never inferred. This offer does not change historical charges.".into()),
        "provider-reported-text-v1" => Ok(format!("Bill valid request-correlated provider-reported text usage, including eligible partial or cancelled usage, under pinned rates and the admission ceiling. Recover evidence for at most {window} seconds after terminal observation; close earlier when lookup is unavailable. Waive unresolved components and release unused reservation. Supplier consumption may remain unknown. No automatic additional customer debit after closure. Other surfaces retain legacy terms.")),
        _ => Err(WalletError::InvalidInput("unknown exception-policy implementation".into())),
    }
}
pub(super) fn digest(domain: &str, canonical: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(canonical.as_bytes());
    doubleentry::Hash::from_bytes(hash.finalize().into()).to_hex()
}
pub(super) fn canonical<T: Serialize>(value: &T) -> Result<String, WalletError> {
    serde_json::to_string(value).map_err(crate::error::invalid)
}
#[derive(Serialize, Deserialize)]
struct PolicyDocument {
    business_id: Uuid,
    version: i64,
    implementation: String,
    evidence_window_secs: i64,
    scope: String,
    disclosure: String,
}
fn version_row(row: &sqlx::postgres::PgRow) -> Result<BillingPolicyVersion, WalletError> {
    let canonical: String = row.try_get("canonical")?;
    let expected: String = row.try_get("digest")?;
    if digest("oxsum/exception-policy/v1", &canonical) != expected {
        return Err(WalletError::Misconfigured("policy digest mismatch".into()));
    }
    let document: PolicyDocument =
        serde_json::from_str(&canonical).map_err(crate::error::invalid)?;
    if document.version != row.try_get::<i64, _>("version")?
        || document.business_id != row.try_get::<Uuid, _>("business_id")?
    {
        return Err(WalletError::Misconfigured(
            "policy identity mismatch".into(),
        ));
    }
    let runtime_ready = document.implementation == "legacy-estimation-v1"
        || (document.implementation == "provider-reported-text-v1" && PROVIDER_REPORTED_READY);
    Ok(BillingPolicyVersion {
        version: document.version,
        digest: expected,
        canonical,
        implementation: document.implementation,
        evidence_window_secs: document.evidence_window_secs,
        scope: document.scope,
        disclosure: document.disclosure,
        runtime_ready,
        published_at: row
            .try_get::<OffsetDateTime, _>("published_at")?
            .to_string(),
    })
}
pub(super) async fn version_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    version: i64,
) -> Result<BillingPolicyVersion, WalletError> {
    let row = sqlx::query(
        "SELECT * FROM oxsum.billing_policy_versions WHERE business_id=$1 AND version=$2",
    )
    .bind(business)
    .bind(version)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| WalletError::NotFound("policy version not found".into()))?;
    version_row(&row)
}
fn acceptance_row(row: &sqlx::postgres::PgRow) -> Result<BillingPolicyAcceptance, WalletError> {
    Ok(BillingPolicyAcceptance {
        id: row.try_get("acceptance_id")?,
        version: row.try_get("version")?,
        digest: row.try_get("digest")?,
        accepted_by: row.try_get("accepted_by")?,
        accepted_at: row.try_get::<OffsetDateTime, _>("accepted_at")?.to_string(),
    })
}
pub(super) async fn current_in(
    tx: &mut Transaction<'_, Postgres>,
    org: Uuid,
) -> Result<Option<BillingPolicyAcceptance>, WalletError> {
    sqlx::query("SELECT * FROM oxsum.billing_policy_acceptances WHERE organization_id=$1 ORDER BY acceptance_revision DESC LIMIT 1").bind(org).fetch_optional(&mut **tx).await?.as_ref().map(acceptance_row).transpose()
}
async fn versions_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
) -> Result<Vec<BillingPolicyVersion>, WalletError> {
    sqlx::query("SELECT * FROM oxsum.billing_policy_versions WHERE business_id=$1 ORDER BY version DESC LIMIT 100").bind(business).fetch_all(&mut **tx).await?.iter().map(version_row).collect()
}
async fn customer_identity(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
) -> Result<(Uuid, Uuid, Uuid), WalletError> {
    authority::lock_business(tx).await?;
    let actor = authority::personal_identity(tx, token).await?;
    if actor.context != "customer" {
        return Err(WalletError::Forbidden(
            "customer context required to accept billing terms".into(),
        ));
    }
    let org = actor.organization.ok_or(WalletError::Unauthenticated)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || $2::text,0))")
        .bind(wallet::ORG_ADMISSION_LOCK)
        .bind(org.to_string())
        .execute(&mut **tx)
        .await?;
    orgs::lock_organization(tx, org).await?;
    let business:Option<Uuid>=sqlx::query_scalar("SELECT c.business_id FROM oxsum.customer_profiles c JOIN oxsum.memberships m USING(organization_id) WHERE c.organization_id=$1 AND m.user_id=$2 AND m.role IN ('owner','admin') AND c.state='active'").bind(org).bind(actor.user.id).fetch_optional(&mut **tx).await?;
    Ok((
        business.ok_or_else(|| {
            WalletError::Forbidden("current customer owner/admin membership required".into())
        })?,
        org,
        actor.user.id,
    ))
}
impl Db {
    /// Read published policies under live Business catalog authority.
    pub async fn business_billing_policies(
        &self,
        token: &str,
    ) -> Result<Vec<BillingPolicyVersion>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        versions_in(&mut tx, business).await
    }
    /// Publish immutable terms; this never changes a customer's acceptance.
    pub async fn publish_billing_policy(
        &self,
        token: &str,
        request: &BillingPolicyPublish,
    ) -> Result<BillingPolicyVersion, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        if !(1..=900).contains(&request.evidence_window_secs) {
            return Err(WalletError::InvalidInput(
                "evidence window must be 1..=900 seconds".into(),
            ));
        }
        let text = disclosure(&request.implementation, request.evidence_window_secs)?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::CatalogWrite,
        )
        .await?;
        let detail = serde_json::to_value(request).map_err(crate::error::invalid)?;
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "billing_policy.publish",
            &detail,
        )
        .await?
        {
            let version: i64 = sqlx::query_scalar(
                "SELECT version FROM oxsum.billing_policy_versions WHERE mutation_id=$1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            return version_in(&mut tx, business, version).await;
        }
        let current:i64=sqlx::query_scalar("SELECT COALESCE(max(version),0) FROM oxsum.billing_policy_versions WHERE business_id=$1").bind(business).fetch_one(&mut *tx).await?;
        if current != request.expected_version {
            return Err(WalletError::Conflict(
                "policy history changed; review again".into(),
            ));
        }
        let version = current
            .checked_add(1)
            .ok_or_else(|| WalletError::InvalidInput("policy version overflow".into()))?;
        let row = PolicyDocument {
            business_id: business,
            version,
            implementation: request.implementation.clone(),
            evidence_window_secs: request.evidence_window_secs,
            scope: POLICY_SCOPE.into(),
            disclosure: text,
        };
        let canonical = canonical(&row)?;
        let hash = digest("oxsum/exception-policy/v1", &canonical);
        let mutation = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "billing_policy.publish",
            &detail,
        )
        .await?;
        sqlx::query("INSERT INTO oxsum.billing_policy_versions(business_id,version,canonical,digest,mutation_id) VALUES($1,$2,$3,$4,$5)").bind(business).bind(version).bind(canonical).bind(hash).bind(mutation.id).execute(&mut *tx).await?;
        let result = version_in(&mut tx, business, version).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Customer current selection plus immutable offers, under current membership.
    pub async fn customer_billing_policy(
        &self,
        token: &str,
    ) -> Result<CustomerBillingPolicy, WalletError> {
        let mut tx = self.pool().begin().await?;
        let (business, org, _) = customer_identity(&mut tx, token).await?;
        let acceptance = current_in(&mut tx, org).await?;
        let current_policy = if let Some(a) = &acceptance {
            Some(version_in(&mut tx, business, a.version).await?)
        } else {
            None
        };
        let organization_name: String =
            sqlx::query_scalar("SELECT name FROM oxsum.organizations WHERE organization_id=$1")
                .bind(org)
                .fetch_one(&mut *tx)
                .await?;
        Ok(CustomerBillingPolicy {
            organization_id: org,
            organization_name,
            acceptance,
            current_policy,
            versions: versions_in(&mut tx, business).await?,
            legacy_disclosure: LEGACY_DISCLOSURE.into(),
        })
    }

    /// Intentional exact-version acceptance, prospectively serialized with admissions.
    pub async fn accept_billing_policy(
        &self,
        token: &str,
        request: &BillingPolicyAccept,
    ) -> Result<BillingPolicyAcceptance, WalletError> {
        authority::validate(&request.idempotency_key, None)?;
        let mut tx = self.pool().begin().await?;
        let (business, org, actor) = customer_identity(&mut tx, token).await?;
        if org != request.expected_organization_id {
            return Err(WalletError::Conflict(
                "customer context changed; review billing terms again".into(),
            ));
        }
        let detail = serde_json::to_value(request).map_err(crate::error::invalid)?;
        if let Some(id) = authority::replay(
            &mut tx,
            org,
            actor,
            &request.idempotency_key,
            "billing_policy.accept",
            &detail,
        )
        .await?
        {
            let row=sqlx::query("SELECT * FROM oxsum.billing_policy_acceptances WHERE acceptance_id=$1 AND organization_id=$2").bind(id).bind(org).fetch_one(&mut *tx).await?;
            return acceptance_row(&row);
        }
        if current_in(&mut tx, org).await?.map(|a| a.id) != request.expected_acceptance_id {
            return Err(WalletError::Conflict(
                "acceptance changed; review again".into(),
            ));
        }
        let policy = version_in(&mut tx, business, request.version).await?;
        if policy.digest != request.digest {
            return Err(WalletError::Conflict(
                "policy digest differs from reviewed offer".into(),
            ));
        }
        if !policy.runtime_ready {
            return Err(WalletError::Conflict(
                "this policy implementation is not enabled in this runtime".into(),
            ));
        }
        let mutation = authority::record(
            &mut tx,
            org,
            actor,
            &request.idempotency_key,
            "billing_policy.accept",
            &detail,
        )
        .await?;
        let revision:i64=sqlx::query_scalar("SELECT COALESCE(max(acceptance_revision),0)+1 FROM oxsum.billing_policy_acceptances WHERE organization_id=$1").bind(org).fetch_one(&mut *tx).await?;
        let row=sqlx::query("INSERT INTO oxsum.billing_policy_acceptances(acceptance_id,organization_id,business_id,version,digest,accepted_by,acceptance_revision) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *").bind(mutation.id).bind(org).bind(business).bind(policy.version).bind(policy.digest).bind(actor).bind(revision).fetch_one(&mut *tx).await?;
        let result = acceptance_row(&row)?;
        tx.commit().await?;
        Ok(result)
    }
}
