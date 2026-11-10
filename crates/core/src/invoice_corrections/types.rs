//! Exact source selections and the financial consequences shown before reissue.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::JournalLineKind;

/// A reduced charge tied to one immutable original invoice line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvoiceCorrectionLineRequest {
    /// Original charge line; credits cannot be selected for removal.
    pub source_line_id: Uuid,
    /// Exact nonnegative replacement amount, at most the original charge.
    pub amount_minor: i64,
}

/// Financial selections to preview without making a document or cash mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvoiceCorrectionPreviewRequest {
    /// Between one and one hundred distinct original charge lines.
    pub lines: Vec<InvoiceCorrectionLineRequest>,
}

/// One retained original line and its proposed successor amount.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceCorrectionPreviewLine {
    /// Immediate predecessor line, never a void reversal or an unrelated line.
    pub source_line_id: Uuid,
    /// Original charge or credit classification.
    pub kind: JournalLineKind,
    /// Original human-readable explanation.
    pub description: String,
    /// Signed amount on the predecessor before its financial void.
    pub original_minor: i64,
    /// Signed successor amount; unselected charges and credits are unchanged.
    pub corrected_minor: i64,
}

/// Cashless correction outcomes; original payable and successor debt stay separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceCorrectionAmounts {
    /// Proven predecessor consideration after its existing credits.
    pub original_total_minor: i64,
    /// Proposed successor consideration after retaining those credits.
    pub corrected_total_minor: i64,
    /// Always zero: old collections never implicitly pay the successor.
    pub successor_collected_minor: i64,
    /// Complete corrected consideration awaiting an explicit successor payment.
    pub successor_receivable_minor: i64,
    /// Policy-specific cumulative earned portion at the completed service boundary.
    pub successor_earned_minor: i64,
    /// Remaining consideration to recognize over the same original service interval.
    pub successor_deferred_minor: i64,
    /// Retained original-source funds owed back after cancellation, not a cash refund.
    pub original_payable_minor: i64,
    /// Always zero: cancellation/reissue itself does not move external cash.
    pub cash_movement_minor: i64,
}

/// Verified source revision and exact correction consequences for staff confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceCorrectionPreview {
    /// Immediate predecessor invoice.
    pub journal_id: Uuid,
    /// Original customer statement corresponding to that invoice.
    pub statement_id: Uuid,
    /// Customer owning the source and successor.
    pub organization_id: Uuid,
    /// Original customer plan, unchanged by reissue.
    pub customer_plan_id: Uuid,
    /// Original service period, never a newly granted period.
    pub plan_period_id: Uuid,
    /// Immutable sold version that originally authorized this fee.
    pub plan_version_id: Uuid,
    /// Original currency, unchanged by correction.
    pub currency: String,
    /// Original recognition policy, either close-time or daily service recognition.
    pub accounting_policy_version: i16,
    /// Original inclusive service start.
    #[serde(with = "time::serde::rfc3339")]
    pub service_start: OffsetDateTime,
    /// Original exclusive service end.
    #[serde(with = "time::serde::rfc3339")]
    pub service_end: OffsetDateTime,
    /// Completed service boundary frozen into the source revision.
    #[serde(with = "time::serde::rfc3339")]
    pub recognition_boundary: OffsetDateTime,
    /// Domain-separated digest of the bounded original sources and proposed amounts.
    pub source_revision: String,
    /// Complete retained source lines, including unchanged credits.
    pub lines: Vec<InvoiceCorrectionPreviewLine>,
    /// Distinct successor debt, original payable and recognition consequences.
    #[serde(flatten)]
    pub amounts: InvoiceCorrectionAmounts,
}
