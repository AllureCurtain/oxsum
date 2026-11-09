//! Exact statement debt intents; cash application and returns retain original sources.

mod apply;
pub(crate) mod evidence;
mod quote;
mod returns;
pub(crate) mod voiding;

pub(crate) use apply::fulfill;
pub(crate) use quote::{guard_journal_in, lock_owner_in, quote_in};
pub(crate) use returns::{refund, reverse_in};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{WalletError, error};

/// Exact original journal share of a named statement repayment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatementPaymentSlice {
    /// Receivable origin, independent of the statement wrapper.
    pub journal_id: Uuid,
    /// Original plan; payment does not renew or activate it.
    pub customer_plan_id: Uuid,
    /// Exact credit-minor share.
    pub amount_minor: i64,
}

/// Immutable quote snapshot bound to a payment order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatementPaymentTerms {
    /// Collecting business.
    pub business_id: Uuid,
    /// Paying customer account.
    pub organization_id: Uuid,
    /// Exact customer document selected for payment.
    pub statement_id: Uuid,
    /// Verified source and live reservation fingerprint.
    pub source_revision: String,
    /// Exact debt paid; fiat rounding never changes it.
    pub amount_minor: i64,
    /// Immutable original receivable allocations in journal order.
    pub allocations: Vec<StatementPaymentSlice>,
    /// Versioned collection policy.
    pub collection_revision: i64,
    /// Fiat currency the provider charges.
    pub display_currency: String,
    /// Fiat ceiling in minor units.
    pub display_amount_minor: i64,
    /// Exact decimal numerator of currency major units per credit.
    pub rate_mantissa: String,
    /// Decimal rate scale.
    pub rate_scale: u32,
}
impl StatementPaymentTerms {
    /// Stable unchanged-quote precondition over all immutable terms.
    pub fn fingerprint(&self) -> Result<String, WalletError> {
        let bytes = serde_json::to_vec(self).map_err(error::invalid)?;
        let mut hash = Sha256::new();
        hash.update(b"oxsum/statement-payment/v1\0");
        hash.update(bytes);
        Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }
}

/// Read-only available debt, standing checkouts and optional proven payment terms.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementPaymentQuote {
    /// Selected customer statement.
    pub statement_id: Uuid,
    /// Displayed unpaid debt, including source-review cases.
    pub outstanding_minor: i64,
    /// Debt held by unresolved payment intents.
    pub reserved_minor: i64,
    /// Unreserved debt; review reasons must be resolved before payment.
    pub available_minor: i64,
    /// Existing checkouts to recover after refresh or a lost response.
    pub pending_order_ids: Vec<Uuid>,
    /// Explicit nonpayable or source-review reason.
    pub reason: Option<String>,
    /// Proven exact terms; absent when payment cannot safely be quoted.
    pub terms: Option<StatementPaymentTerms>,
    /// Required precondition for creating a new payment.
    pub fingerprint: Option<String>,
}
