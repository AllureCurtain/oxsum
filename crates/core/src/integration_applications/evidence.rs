//! Bounded original-byte evidence reads under live application/customer authority.
use std::collections::BTreeSet;

use serde::Serialize;
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::evidence_json;
use crate::{ActingService, Db, IntegrationScope, WalletError, heads, integration_credentials};

const MAX_BYTES: i32 = 16 * 1024 * 1024;

/// Retained descriptor metadata; download performs the integrity and disclosure checks.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationEvidenceSummary {
    /// Existing marker/bundle identifier.
    pub bundle_id: Uuid,
    /// Period or statement evidence.
    pub kind: String,
    /// Inclusive first UTC day for a period bundle.
    pub period_from: Option<String>,
    /// Inclusive last UTC day for a period bundle.
    pub period_to: Option<String>,
    /// Exact plan statement for statement evidence.
    pub statement_id: Option<Uuid>,
    /// Manifest digest retained when the bundle was published.
    pub manifest_digest: String,
    /// Original marker's log position.
    pub anchor_log_index: i64,
    /// Number of retained ledger entries, including the marker.
    pub entry_count: i64,
    /// Original publication timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
/// One ascending identifier page, scoped to an application's exact customer.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationEvidencePage {
    /// Current owner-granted application namespace.
    pub application_id: Uuid,
    /// Exact customer account that owns these retained descriptors.
    pub organization_id: Uuid,
    /// At most fifty retained descriptors, never evidence or supplier costs.
    pub bundles: Vec<ApplicationEvidenceSummary>,
    /// Last included identifier if a following page exists.
    pub next_cursor: Option<Uuid>,
}
fn summary(row: &PgRow) -> Result<ApplicationEvidenceSummary, WalletError> {
    Ok(ApplicationEvidenceSummary {
        bundle_id: row.try_get("bundle_id")?,
        kind: row.try_get("kind")?,
        period_from: row
            .try_get::<Option<Date>, _>("period_from")?
            .map(|d| d.to_string()),
        period_to: row
            .try_get::<Option<Date>, _>("period_to")?
            .map(|d| d.to_string()),
        statement_id: row.try_get("statement_id")?,
        manifest_digest: row.try_get("manifest_digest")?,
        anchor_log_index: row.try_get("anchor_log_index")?,
        entry_count: row.try_get("entry_count")?,
        created_at: row.try_get("created_at")?,
    })
}
async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    org: Uuid,
) -> Result<Uuid, WalletError> {
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut **tx)
        .await?;
    integration_credentials::require_integration(
        tx,
        service,
        IntegrationScope::EvidenceRead,
        Some(org),
    )
    .await?;
    service
        .application_id
        .ok_or_else(|| WalletError::Forbidden("application credential required".into()))
}
fn unavailable() -> WalletError {
    WalletError::Conflict(
        "retained evidence is not a valid customer-safe export; request operator recovery".into(),
    )
}
fn has_private_cost(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            matches!(key.as_str(), "upstreamCostMinor" | "upstream_cost_minor")
                || has_private_cost(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(has_private_cost),
        _ => false,
    }
}
fn validate(
    bytes: &[u8],
    descriptor: &ApplicationEvidenceSummary,
    org: Uuid,
    tenant: &str,
) -> Result<(), WalletError> {
    let text = std::str::from_utf8(bytes).map_err(|_| unavailable())?;
    let value = evidence_json::parse(text).map_err(|_| unavailable())?;
    let object = value.as_object().ok_or_else(unavailable)?;
    let allowed = [
        "version",
        "kind",
        "bundleId",
        "organizationId",
        "manifestDigest",
        "periodFrom",
        "periodTo",
        "statementId",
        "anchorLogIndex",
        "scope",
        "artifacts",
        "manifest",
        "entries",
        "head",
        "note",
        "keyName",
        "publicKey",
        "keyHash",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || value["version"] != 2
        || value["bundleId"] != descriptor.bundle_id.to_string()
        || value["organizationId"] != org.to_string()
        || value["manifestDigest"] != descriptor.manifest_digest
        || value["kind"] != descriptor.kind
        || value["anchorLogIndex"] != descriptor.anchor_log_index
        || value["periodFrom"] != serde_json::json!(descriptor.period_from)
        || value["periodTo"] != serde_json::json!(descriptor.period_to)
        || value["statementId"] != serde_json::json!(descriptor.statement_id)
        || value["scope"]["upstreamCostIncluded"] != false
        || has_private_cost(&value)
    {
        return Err(unavailable());
    }
    let excluded = value["scope"]["excluded"]
        .as_array()
        .ok_or_else(unavailable)?;
    if !excluded.iter().any(|v| v == "upstreamCostFigures") {
        return Err(unavailable());
    }
    let entries = value["entries"].as_array().ok_or_else(unavailable)?;
    if i64::try_from(entries.len()).ok() != Some(descriptor.entry_count)
        || entries.first().is_none_or(|entry| {
            entry["entry"]["id"] != descriptor.bundle_id.to_string()
                || entry["proof"]["leaf_index"] != descriptor.anchor_log_index
        })
    {
        return Err(unavailable());
    }
    // No uncommitted artifact may hitchhike beside the signed manifest.
    let manifest = value["manifest"].as_array().ok_or_else(unavailable)?;
    let mut names = BTreeSet::new();
    for entry in manifest {
        let kind = entry["kind"].as_str().ok_or_else(unavailable)?;
        let id = entry["id"].as_str().ok_or_else(unavailable)?;
        if !names.insert((kind, id)) {
            return Err(unavailable());
        }
    }
    let artifacts = value["artifacts"].as_object().ok_or_else(unavailable)?;
    let mut count = 0;
    for (kind, group) in artifacts {
        for id in group.as_object().ok_or_else(unavailable)?.keys() {
            if !names.contains(&(kind.as_str(), id.as_str())) {
                return Err(unavailable());
            }
            count += 1;
        }
    }
    if count != names.len() {
        return Err(unavailable());
    }
    let key = match (
        value["keyName"].as_str(),
        value["publicKey"].as_str(),
        value["keyHash"].as_str(),
    ) {
        (Some(name), Some(public), Some(hash)) => {
            Some(oxsum_verify::PublishedKey::new(name, public, hash))
        }
        (None, None, None)
            if ["note", "keyName", "publicKey", "keyHash"]
                .iter()
                .all(|field| value[field].is_null()) =>
        {
            None
        }
        _ => return Err(unavailable()),
    };
    let origin = heads::origin_for(tenant).map_err(|_| unavailable())?;
    // The retained key establishes internal consistency only. Consumers still
    // verify the original bytes against independently retained trust inputs.
    oxsum_verify::verify_evidence_bundle(text, origin.as_str(), &org.to_string(), key.as_ref())
        .map_err(|_| unavailable())
}
impl Db {
    /// List retained descriptors without opening a wallet or publishing evidence.
    pub async fn application_evidence_bundles(
        &self,
        service: &ActingService,
        org: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<ApplicationEvidencePage, WalletError> {
        if !(1..=50).contains(&limit) {
            return Err(WalletError::InvalidInput(
                "limit must be between 1 and 50".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let application_id = authorize(&mut tx, service, org).await?;
        let rows=sqlx::query("SELECT bundle_id,kind,period_from,period_to,statement_id,manifest_digest,anchor_log_index,entry_count,created_at FROM oxsum.evidence_bundles WHERE organization_id=$1 AND ($2::uuid IS NULL OR bundle_id>$2) ORDER BY bundle_id LIMIT $3")
            .bind(org).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut bundles = rows.iter().map(summary).collect::<Result<Vec<_>, _>>()?;
        let more = bundles.len() > limit as usize;
        bundles.truncate(limit as usize);
        let next_cursor = if more {
            bundles.last().map(|row| row.bundle_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(ApplicationEvidencePage {
            application_id,
            organization_id: org,
            bundles,
            next_cursor,
        })
    }
    /// Return original customer-safe evidence bytes under the retained credential lock.
    pub async fn application_evidence_json(
        &self,
        service: &ActingService,
        org: Uuid,
        bundle: Uuid,
    ) -> Result<Vec<u8>, WalletError> {
        let mut tx = self.pool().begin().await?;
        authorize(&mut tx, service, org).await?;
        let row=sqlx::query("SELECT b.bundle_id,b.kind,b.period_from,b.period_to,b.statement_id,b.manifest_digest,b.anchor_log_index,b.entry_count,b.created_at,o.tenant_id,CASE WHEN octet_length(b.bundle_json)<=$3 THEN b.bundle_json END AS bytes FROM oxsum.evidence_bundles b JOIN oxsum.organizations o USING(organization_id) WHERE b.organization_id=$1 AND b.bundle_id=$2")
            .bind(org).bind(bundle).bind(MAX_BYTES).fetch_optional(&mut *tx).await?.ok_or_else(||WalletError::NotFound("evidence bundle is not known".into()))?;
        let bytes:Vec<u8>=row.try_get::<Option<Vec<u8>>,_>("bytes")?.ok_or_else(||WalletError::Conflict("retained evidence exceeds the 16 MiB download bound; request a smaller scoped export".into()))?;
        validate(&bytes, &summary(&row)?, org, row.try_get("tenant_id")?)?;
        tx.commit().await?;
        Ok(bytes)
    }
}
