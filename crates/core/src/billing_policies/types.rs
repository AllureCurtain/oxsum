//! Versioned policy contracts and durable admission data.
use crate::Price;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Operator publication, independent from customer consent.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingPolicyPublish {
    /// Exact retry identity.
    pub idempotency_key: String,
    /// Latest version the operator reviewed, or zero.
    pub expected_version: i64,
    /// Known implementation, never an executable free-form rule.
    pub implementation: String,
    /// Maximum window after terminal observation, bounded to 900 seconds.
    pub evidence_window_secs: i64,
    /// Audited publication reason.
    pub reason: String,
}
/// Immutable disclosed offer; readiness is the current runtime's capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BillingPolicyVersion {
    /// Monotonic Business version.
    pub version: i64,
    /// Domain-separated canonical offer digest.
    pub digest: String,
    /// Exact immutable JSON bytes for independent digest recomputation.
    pub canonical: String,
    /// Known implementation identity.
    pub implementation: String,
    /// Maximum terminal evidence duration.
    pub evidence_window_secs: i64,
    /// Exact supported scope.
    pub scope: String,
    /// Fixed semantics for this implementation and duration.
    pub disclosure: String,
    /// Whether accepting this offer is supported by this runtime.
    pub runtime_ready: bool,
    /// Publication timestamp.
    pub published_at: String,
}
/// Explicit acceptance intent from the authenticated customer manager.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingPolicyAccept {
    /// Reviewed customer identity; must equal the live session context.
    pub expected_organization_id: Uuid,
    /// Exact retry identity.
    pub idempotency_key: String,
    /// Concrete published version.
    pub version: i64,
    /// Reviewed digest.
    pub digest: String,
    /// Current selection observed by the user; null for historical legacy.
    pub expected_acceptance_id: Option<Uuid>,
}
/// Retained authenticated consent, effective on subsequent admissions only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BillingPolicyAcceptance {
    /// Immutable acceptance identity.
    pub id: Uuid,
    /// Accepted version.
    pub version: i64,
    /// Accepted digest.
    pub digest: String,
    /// Customer manager who accepted.
    pub accepted_by: Uuid,
    /// Accepted timestamp.
    pub accepted_at: String,
}
/// Customer policy selection and offers.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerBillingPolicy {
    /// Live customer context used for review.
    pub organization_id: Uuid,
    /// Customer display name.
    pub organization_name: String,
    /// None preserves historical semantics without inventing consent.
    pub acceptance: Option<BillingPolicyAcceptance>,
    /// Selected policy even when it is older than the history page.
    pub current_policy: Option<BillingPolicyVersion>,
    /// Published versions, newest first.
    pub versions: Vec<BillingPolicyVersion>,
    /// Historical absence interpretation.
    pub legacy_disclosure: String,
}
/// Selection compared again inside the admission transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PolicySelection {
    /// Concrete customer acceptance, or no explicit acceptance.
    pub acceptance: Option<BillingPolicyAcceptance>,
    /// Matching immutable policy.
    pub policy: Option<BillingPolicyVersion>,
}
/// Complete retail terms for one permitted route, with no credentials or wholesale costs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BillingRouteTerms {
    /// Local provider route identity.
    pub channel_id: Uuid,
    /// Historical channel label.
    pub channel: String,
    /// Pinned channel price version.
    pub price_version: i64,
    /// Served public model.
    pub model: String,
    /// Alias target if any.
    pub upstream_model: Option<String>,
    /// Exact sanitized retail rates, including conditional rules.
    pub price: Price,
}
/// Durable admission contract, separate from legacy sale/settlement encodings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayBillingTerms {
    /// Business identity committed with this contract.
    pub business_id: Uuid,
    /// Customer identity, checked against the admission and read scope.
    pub organization_id: Uuid,
    /// Durable operation identity even after diagnostics retention.
    pub operation_id: Uuid,
    /// Authenticated consumption key.
    pub key_id: Uuid,
    /// Original request-derived reservation identity.
    pub hold_key: String,
    /// Estimator implementation for explicit legacy terms; never inferred from current config.
    pub estimator: String,
    /// Envelope version.
    pub version: i64,
    /// Exact protocol route, independent of the operation's coarse surface.
    pub surface: String,
    /// Whether this request streams.
    pub stream: bool,
    /// Fixed machine-readable arithmetic implementation.
    pub arithmetic_version: String,
    /// Exact scope applicability, checked against surface and stream at admission.
    pub policy_applies: bool,
    /// Accepted selection compared at admission, even outside the new scope.
    pub selection: PolicySelection,
    /// Immutable retail sale digest if present.
    pub sale_digest: Option<String>,
    /// Discount applied to all selected retail routes.
    pub discount_percent: Option<i64>,
    /// Original requested model, before alias/experiment selection.
    pub requested_model: String,
    /// Effective output bound.
    pub output_bound: i64,
    /// Effective customer ceiling, integer credit minor units.
    pub ceiling_minor: i64,
    /// Complete selected route rate snapshots; array index is the immutable route identity.
    pub routes: Vec<BillingRouteTerms>,
    /// Fixed arithmetic interpretation.
    pub arithmetic: String,
}

impl GatewayBillingTerms {
    /// Whether this request uses the new provider-reported runtime path.
    #[must_use]
    pub fn provider_reported(&self) -> bool {
        self.policy_applies
            && self
                .selection
                .policy
                .as_ref()
                .is_some_and(|p| p.implementation == "provider-reported-text-v1")
    }
}
