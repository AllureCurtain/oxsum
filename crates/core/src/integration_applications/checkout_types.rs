//! Exact application checkout inputs, server quote and immutable retry identity.
use crate::{CheckoutChoice, MerchantMode, MerchantRail, PaymentOrderPurpose};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Select a wallet fiat amount or one already-published bundle version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationCheckoutSelection {
    /// Only walletTopup and bundle are supported by this bounded surface.
    pub purpose: PaymentOrderPurpose,
    /// Owner-selected collection rail.
    pub rail: MerchantRail,
    /// Positive fiat minor amount for wallet top-up; absent for a bundle.
    pub display_amount_minor: Option<i64>,
    /// Published bundle version; absent for wallet top-up.
    pub bundle_version_id: Option<Uuid>,
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
    /// Exact purchased credits credited only on verified payment.
    pub purchased_minor: i64,
    /// Published bundle promotion, never caller-supplied.
    pub promo_minor: i64,
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
