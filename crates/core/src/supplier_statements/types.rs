//! Explicit supplier source assertions and lossless raw-page input.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Restricted OpenAI Costs query; unsupported filters cannot silently narrow scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CostsQuery {
    /// Inclusive UTC midnight Unix seconds.
    pub start_time: i64,
    /// Exclusive next UTC midnight Unix seconds.
    pub end_time: i64,
    /// Only daily buckets are supported.
    pub bucket_width: String,
    /// Empty or exactly project_id.
    pub group_by: Vec<String>,
    /// Explicit project filter, empty for organization scope.
    pub project_ids: Vec<String>,
    /// Provider page size, 1..=180.
    pub limit: i64,
}
/// Original response text and the cursor used to fetch it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CostsPageInput {
    /// None for the first page.
    pub request_page: Option<String>,
    /// Exact JSON bytes decoded as UTF-8, not a reserialized numeric object.
    pub body: String,
}
/// Operator assertion plus the exact material being imported.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplierStatementImport {
    /// Exact replay identity.
    pub idempotency_key: String,
    /// Operator justification.
    pub reason: String,
    /// Asserted provider organization; contents do not authenticate it.
    pub provider_organization_id: String,
    /// Complete UTC day, YYYY-MM-DD.
    pub bill_date: String,
    /// Asserted fetch timestamp, RFC3339.
    pub fetched_at: String,
    /// organization or projects.
    pub scope_kind: String,
    /// Selected projects, sorted and unique.
    pub project_ids: Vec<String>,
    /// Asserted complete local channel set, unique.
    pub channel_ids: Vec<Uuid>,
    /// Positive USD per local supplier-cost credit, exact decimal.
    pub usd_per_credit: String,
    /// Scope mapping and unit conversion basis; not independently authenticated.
    pub mapping_evidence: String,
    /// Exact supported query shape.
    pub query: CostsQuery,
    /// Complete bounded cursor chain.
    pub pages: Vec<CostsPageInput>,
}
/// New comparison intent; exact retries keep the original capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplierComparisonRequest {
    /// Exact replay identity.
    pub idempotency_key: String,
    /// Reason for capturing another version.
    pub reason: String,
}
