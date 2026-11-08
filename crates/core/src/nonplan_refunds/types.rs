//! Customer-visible cash facts and exact local recovery requests.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Actual verified non-plan refund cash, separate from retained approval state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NonplanRefundEffect {
    /// Source-allocation policy; currently `nonplan-refund-v1`.
    pub policy: String,
    /// Exact refund whose external cash returned.
    pub refund_id: Uuid,
    /// Original paid order.
    pub order_id: Uuid,
    /// Customer owning the order and ledger.
    pub organization_id: Uuid,
    /// Complete external Cash outflow, in ledger credit minor units.
    pub cash_minor: i64,
    /// Proven original Wallet funding taken back.
    pub wallet_minor: i64,
    /// Remaining Cash outflow retained as review suspense.
    pub suspense_minor: i64,
    /// Historical wrong-source Wallet debit restored without another Cash leg.
    pub wallet_restored_minor: i64,
    /// Entry containing the original complete Cash return.
    pub cash_entry_id: Uuid,
    /// Verified content hash of the Cash entry.
    pub cash_entry_hash: String,
    /// Original Cash entry accounting day, as an ISO date.
    pub cash_booking_date: String,
    /// Append-only Wallet restitution entry, when needed.
    pub correction_entry_id: Option<Uuid>,
    /// Content hash of the restitution entry.
    pub correction_entry_hash: Option<String>,
    /// Original payment funding entry, when proven.
    pub original_funding_entry_id: Option<Uuid>,
    /// Verified hash of the original funding entry.
    pub original_funding_entry_hash: Option<String>,
    /// Trustworthy exact retained provider success event.
    pub event_receipt_id: Uuid,
    /// Whether an explicit historical recovery created this allocation.
    pub legacy_recovery: bool,
    /// Financial source or approval review remains necessary.
    pub requires_review: bool,
    /// Machine-readable reasons requiring source review.
    pub review_reasons: Vec<String>,
    /// Metadata commit instant; distinct from the original Cash accounting day.
    #[serde(with = "time::serde::rfc3339")]
    pub booked_at: OffsetDateTime,
}

/// One exact historical refund and authenticated receipt selected for recovery.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NonplanRefundRecoveryRequest {
    /// Exact local refund; never a customer-wide batch selector.
    pub refund_id: Uuid,
    /// Trustworthy retained success receipt; a state alone is insufficient.
    pub event_receipt_id: Uuid,
    /// Binds actor, target, reason, receipt and revision on apply.
    pub idempotency_key: String,
    /// Operator explanation retained in finance audit.
    pub reason: String,
    /// Required on apply; the exact preceding preview's source revision.
    pub expected_revision: Option<String>,
}

/// Proposed, retained or explicitly refused source-bound correction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NonplanRefundRecovery {
    /// Exact local refund inspected.
    pub refund_id: Uuid,
    /// `proposed`, `applied` or `refused`; refusal is never successful repair.
    pub status: String,
    /// Precise source refusal; absent for supported effects.
    pub refusal: Option<String>,
    /// SHA-256 over exact inspected source facts and sequence boundaries.
    pub source_revision: String,
    /// Complete immutable source evidence proposed or retained.
    pub source: serde_json::Value,
    /// Verified actual cash effect; absent until committed.
    pub effect: Option<NonplanRefundEffect>,
    /// Cash already present before the proposed apply.
    pub existing_cash_minor: i64,
    /// Additional Cash credit proposed; zero for retained old money-out.
    pub cash_to_book_minor: i64,
    /// Original Wallet funding justified by the inspected boundary.
    pub wallet_minor: i64,
    /// Cash whose source remains review suspense.
    pub suspense_minor: i64,
    /// Historical Wallet debit proposed for restitution, without Cash.
    pub wallet_to_restore_minor: i64,
    /// True when a committed correction is returned without new effects.
    pub already_applied: bool,
}
