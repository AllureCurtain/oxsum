//! Explicit, source-verified correction of historical duplicated plan usage.

mod apply;
mod evidence;
mod sources;

pub(crate) use sources::corrected_order_journals_in;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One exact historical journal selected for an operator correction.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanUsageCorrectionRequest {
    /// Journal containing the original duplicated usage charge.
    pub journal_id: Uuid,
    /// Operator explanation retained with the outcome.
    pub reason: String,
    /// Binds one apply attempt and its outcome to these request terms.
    pub idempotency_key: String,
}

/// A proposed, committed or explicitly refused historical correction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanUsageCorrection {
    /// Exact original journal, never a customer-wide batch selection.
    pub journal_id: Uuid,
    /// Customer whose ledger and documents supply the evidence.
    pub organization_id: Uuid,
    /// Original customer statement when its source has been validated.
    pub statement_id: Option<Uuid>,
    /// Stable identity shared by the correction and its appended entry.
    pub correction_id: Uuid,
    /// `proposed`, `applied` or `refused`; refusal is never a successful repair.
    pub status: String,
    /// Precise unsupported or inconsistent source requiring manual review.
    pub refusal: Option<String>,
    /// Earned amount recognized a second time by the historical plan close.
    pub duplicate_minor: i64,
    /// Part that removes still-open original journal debt.
    pub receivable_minor: i64,
    /// Previously collected excess now owed back, without a cash outflow.
    pub payable_minor: i64,
    /// Original journal gross charge before the correction.
    pub before_total_minor: i64,
    /// Proven payments allocated to the original journal.
    pub before_settled_minor: i64,
    /// Corrected gross charge after removing the duplicated usage.
    pub after_total_minor: i64,
    /// Collected amount retained against the corrected charge.
    pub after_settled_minor: i64,
    /// Original accrual entry, which is retained unchanged.
    pub original_entry_id: Uuid,
    /// Appended correction entry; absent in previews and refusals.
    pub entry_id: Option<Uuid>,
    /// True when this returns a standing correction without new effects.
    pub already_applied: bool,
    /// Exact source line, usage, payment and content-hash evidence inspected.
    pub evidence: serde_json::Value,
    /// SHA-256 over the complete source evidence, also bound into the ledger entry.
    pub evidence_sha256: String,
}
