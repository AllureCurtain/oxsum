//! Exact application checkout inputs, server quote and immutable retry identity.
use crate::{
    CheckoutChoice, MerchantMode, MerchantRail, PaymentOrderPurpose, PlanPaymentTerms,
    StatementPaymentTerms,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Select wallet funding, a bundle, an assigned plan or exact named statement debt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationCheckoutSelection {
    /// Only walletTopup, bundle, planPeriod and statementRepayment are supported.
    pub purpose: PaymentOrderPurpose,
    /// Owner-selected collection rail.
    pub rail: MerchantRail,
    /// Positive fiat minor amount for wallet top-up; absent for a bundle.
    pub display_amount_minor: Option<i64>,
    /// Published bundle version; absent for wallet top-up.
    pub bundle_version_id: Option<Uuid>,
    /// Assigned customer plan; absent for wallet funding or bundles.
    /// Omission on serialization preserves already-retained wallet/bundle requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_plan_id: Option<Uuid>,
    /// Exact owned statement, only for statementRepayment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<Uuid>,
    /// Exact credit-minor debt; absent selects all verified unreserved debt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_amount_minor: Option<i64>,
}
/// Server-derived immutable quote, checked again when the order is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationCheckoutTerms {
    /// Stable application namespace, never an owner identity.
    pub application_id: Uuid,
    /// Exact application-owned customer account.
    pub organization_id: Uuid,
    /// Original selection.
    pub selection: ApplicationCheckoutSelection,
    /// Selected merchant/version/revision, without credentials.
    pub checkout: CheckoutChoice,
    /// Test or live collection mode, shown before payment.
    pub merchant_mode: MerchantMode,
    /// Owner-chosen public merchant name, without account or secret fields.
    pub merchant_name: String,
    /// Current business collection terms revision.
    pub collection_revision: i64,
    /// Fiat currency selected by the business.
    pub display_currency: String,
    /// Fiat amount collected by the provider.
    pub display_amount_minor: i64,
    /// Purchased wallet credits; zero for plan payments, which grant entitlement.
    pub purchased_minor: i64,
    /// Published bundle promotion, never caller-supplied.
    pub promo_minor: i64,
    /// Exact sold fee and period terms, present only for planPeriod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_payment: Option<PlanPaymentTerms>,
    /// Exact original receivable allocations, only for statementRepayment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_payment: Option<StatementPaymentTerms>,
}
/// A checkout operation retaining its original return URLs and quote on retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationCheckoutRequest {
    /// Caller operation key, namespaced independently from portal orders.
    pub idempotency_key: String,
    /// Exact prior server quote.
    pub quote: ApplicationCheckoutTerms,
    /// Absolute HTTPS destination or explicit loopback HTTP for local development.
    pub return_url: String,
    /// Same policy as return URL; retained even when a rail has no separate cancel.
    pub cancel_url: String,
}
