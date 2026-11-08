//! Retained correction evidence and exact preview/apply request terms.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Append-only compensation of a proven old non-plan DrCash/CrCreditLine loss.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NonplanChargebackCorrection {
    /// Versioned recovery policy, `nonplan-chargeback-recovery-v1`.
    pub policy: String,
    /// Exact terminal lost dispute.
    pub dispute_id: Uuid,
    /// Original payment order.
    pub order_id: Uuid,
    /// Customer owning both original and corrected ledger entries.
    pub organization_id: Uuid,
    /// Erroneous old Cash debit B cancelled by this correction.
    pub false_receipt_minor: i64,
    /// Independently authenticated and audited actual cash loss L.
    pub actual_loss_minor: i64,
    /// Appended Cash credit B + L, distinct from actual provider loss L.
    pub cash_correction_minor: i64,
    /// Proven retained original purchased Wallet allocated only to L.
    pub wallet_minor: i64,
    /// Full correction less original Wallet, retained as Adjustments suspense.
    pub suspense_minor: i64,
    /// Old CreditLine credit B preserved without reopening debt or statements.
    pub preserved_credit_benefit_minor: i64,
    /// Unchanged wrong-direction entry.
    pub original_entry_id: Uuid,
    /// Verified original content hash.
    pub original_entry_hash: String,
    /// New entry cancelling false Cash and retaining the actual loss.
    pub correction_entry_id: Uuid,
    /// Verified correction content hash.
    pub correction_entry_hash: String,
    /// Actual appended correction accounting date, as ISO date.
    pub correction_booking_date: String,
    /// Exact retained authenticated case receipt.
    pub event_receipt_id: Uuid,
    /// SHA256 of the reviewed original source snapshot.
    pub source_revision: String,
    /// Always true: the historical credit benefit needs explicit review.
    pub requires_review: bool,
    /// Source-linked reasons requiring review, including preserved credit benefit.
    pub review_reasons: Vec<String>,
    /// Immutable metadata commit timestamp.
    #[serde(with = "time::serde::rfc3339")]
    pub booked_at: OffsetDateTime,
}

/// One exact historical dispute and its independently retained receipt.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NonplanChargebackRecoveryRequest {
    /// Exact local dispute, never a batch selector.
    pub dispute_id: Uuid,
    /// Its exact retained authenticated case receipt.
    pub event_receipt_id: Uuid,
    /// Actor- and terms-bound recovery operation identity.
    pub idempotency_key: String,
    /// Inspected source explanation retained in finance audit.
    pub reason: String,
    /// Required on apply: the inspected preview SHA256.
    pub expected_revision: Option<String>,
}

/// Read-only proposal, retained correction or explicit source refusal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NonplanChargebackRecovery {
    /// Exact dispute inspected.
    pub dispute_id: Uuid,
    /// `proposed`, `applied` or `refused`.
    pub status: String,
    /// Source refusal explanation when unsupported.
    pub refusal: Option<String>,
    /// SHA256 of the exact source and ledger boundary.
    pub source_revision: String,
    /// Full original source and allocation evidence.
    pub source: serde_json::Value,
    /// Wrong old receipt B being cancelled.
    pub false_receipt_minor: i64,
    /// Actual authenticated loss L retained once.
    pub actual_loss_minor: i64,
    /// Proposed additional Cash credit, B + L; zero for retained replay.
    pub cash_to_book_minor: i64,
    /// Proven original Wallet allocated only to actual loss L.
    pub wallet_minor: i64,
    /// Proposed source-linked Adjustments suspense.
    pub suspense_minor: i64,
    /// Verified immutable effect once committed.
    pub effect: Option<NonplanChargebackCorrection>,
    /// A committed result returned without new money or documents.
    pub already_applied: bool,
}
