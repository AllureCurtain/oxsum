//! Restricted worker context and allowlisted receipt projection.
use super::ObservedTextUsage;
use crate::ProviderDestinationPolicy;
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

/// Durable dispatch coordinates, restricted to the gateway and recovery worker.
pub struct RecoveryDispatch<'a> {
    /// Selected immutable route array index.
    pub route_index: usize,
    /// Route configuration captured before dispatch.
    pub serving: &'a crate::Serving,
    /// Selected credential in that configuration.
    pub credential_id: Uuid,
    /// Caller explicitly permitted provider storage.
    pub stored: bool,
    /// Experiment attribution selected before admission.
    pub experiment: Option<&'a crate::ExperimentBinding>,
}
/// A fenced recovery ownership token, never customer input.
#[derive(Debug, Clone)]
pub struct RecoveryClaim {
    /// Operation identity.
    pub operation_id: Uuid,
    /// Customer identity.
    pub organization_id: Uuid,
    /// Unique ownership generation.
    pub token: Uuid,
    /// Immutable evidence deadline.
    pub deadline: OffsetDateTime,
    /// Current sanitized evidence.
    pub observed: ObservedTextUsage,
    /// Whether a stored-completion lookup could still help.
    pub needs_lookup: bool,
}
/// Credential-bearing lookup context; deliberately neither Debug nor Serialize.
pub struct RecoveryLookup {
    /// Original checked origin.
    pub base_url: String,
    /// Rechecked egress restriction.
    pub policy: ProviderDestinationPolicy,
    /// Original still-current secret, for one GET only.
    pub api_key: String,
    /// Exact provider completion identity.
    pub completion_id: String,
}
/// Customer-safe execution, evidence and financial state.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecoveryReceipt {
    /// Executing, pending, or financially closed.
    pub state: String,
    /// First terminal execution observation, independent of delivery.
    pub terminal_observation: Option<String>,
    /// First terminal observation UTC.
    pub terminal_at: Option<String>,
    /// Fixed terminal evidence deadline UTC.
    pub deadline: Option<String>,
    /// Financial close UTC.
    pub closed_at: Option<String>,
    /// Final decision explanation.
    pub close_reason: Option<String>,
    /// Retained provider components; null means unknown.
    pub observed: ObservedTextUsage,
    /// Components excluded from customer billing.
    pub waived: Vec<String>,
    /// Whether supplier cost could be priced independently.
    pub supplier_cost_known: bool,
}

/// Business-scoped queue projection without supplier credentials or content.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecoveryQueueRow {
    /// Durable operation identity.
    pub operation_id: Uuid,
    /// Owning customer account.
    pub organization_id: Uuid,
    /// Requested model pinned at admission.
    pub model: String,
    /// Maximum customer charge, never a supplier cost limit.
    pub ceiling_minor: i64,
    /// Safe current evidence and closure state.
    pub recovery: UsageRecoveryReceipt,
}
