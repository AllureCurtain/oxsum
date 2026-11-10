//! Public, provider-free projections for an explicitly scoped application reader.

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{CustomerPlanState, PlanCreditStanding, SaleTerms};

/// Recorded customer standing, not permission to execute a future request.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCustomerEntitlement {
    /// Exact owner-granted application namespace.
    pub application_id: Uuid,
    /// Application-owned customer account.
    pub organization_id: Uuid,
    /// Database clock used for this coherent snapshot.
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    /// Current profile lifecycle: active or archived.
    pub customer_state: String,
    /// Independent manual account restriction.
    pub manually_suspended: bool,
    /// Independent organization-wide dunning restriction.
    pub organization_dunning_suspended: bool,
    /// Shared plan debt/dunning projection, which governs plan overage admission.
    pub plan_overage_suspended: bool,
    /// Retained debt across all customer plans; excludes pending holds.
    pub outstanding_plan_debt_minor: i64,
    /// Live sold-plan binding, or the latest terminal binding.
    pub plan: Option<ApplicationPlanStanding>,
    /// Active subscription or the selected plan's retained subscription window.
    pub quota: Option<ApplicationQuotaStanding>,
}

/// Minimal current-or-terminal sold-plan state without provider identifiers.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationPlanStanding {
    /// Selected customer binding.
    pub customer_plan_id: Uuid,
    /// Exact immutable sold-plan version.
    pub plan_version_id: Uuid,
    /// Recorded lifecycle; a read never advances it.
    pub state: CustomerPlanState,
    /// Recorded cancellation request, including one whose boundary has passed.
    pub cancel_at_period_end: bool,
    /// A booked next-version change, not a paid authorization.
    pub scheduled_change: Option<Uuid>,
    /// Shared outstanding-debt projection for this binding.
    pub credit: PlanCreditStanding,
}

/// Quota counters belong only to this recorded window and include pending draws.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationQuotaStanding {
    /// Quota engine identity.
    pub subscription_id: Uuid,
    /// Authorized sold-plan period matching this exact quota window, when present.
    pub plan_period_id: Option<Uuid>,
    /// Recorded active, cancelled or expired state.
    pub state: String,
    /// Clock position: notStarted, current or ended; never overrides lifecycle.
    pub window_state: &'static str,
    /// Inclusive recorded window start.
    #[serde(with = "time::serde::rfc3339")]
    pub period_start: OffsetDateTime,
    /// Exclusive recorded window end; absent for a never-resetting subscription.
    #[serde(with = "time::serde::rfc3339::option")]
    pub period_end: Option<OffsetDateTime>,
    /// Recorded authorized quota; absent means unmetered.
    pub quota_minor: Option<i64>,
    /// Recorded consumption, including pending reservations.
    pub consumed_minor: i64,
    /// Nonnegative recorded quota remainder, never a promised spendable balance.
    pub remaining_recorded_minor: Option<i64>,
    /// Recorded policy; future admission still checks all restrictions and debt.
    pub allow_wallet_overflow: bool,
}

/// Effective published customer sale terms, without wholesale prices or proof writes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCustomerSaleTerms {
    /// Override, assigned book or default book, in that precedence order.
    pub source: String,
    /// Immutable effective schedule identity.
    pub schedule_id: Uuid,
    /// Published book identity; absent for a customer override.
    pub book_id: Option<Uuid>,
    /// Exact published version; absent for a customer override.
    pub version: Option<i64>,
    /// Inclusive effective boundary.
    #[serde(with = "time::serde::rfc3339")]
    pub effective_from: OffsetDateTime,
    /// Exclusive effective boundary, if any.
    #[serde(with = "time::serde::rfc3339::option")]
    pub effective_until: Option<OffsetDateTime>,
    /// Customer-only prices and discount, validated by the shared sale resolver.
    pub terms: SaleTerms,
}

/// One configured permitted public model, not a provider reachability assertion.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCustomerModel {
    /// Public model name, never a hidden alias target.
    pub model: String,
    /// None explicitly means no active published customer sale for this model.
    pub sale_terms: Option<ApplicationCustomerSaleTerms>,
}

/// Bounded public model page; each read chooses a new coherent snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCustomerModelPage {
    /// Exact owner-granted application namespace.
    pub application_id: Uuid,
    /// Application-owned customer.
    pub organization_id: Uuid,
    /// Optional live application-issued consumption key used to narrow authority.
    pub key_id: Option<Uuid>,
    /// Database clock used to resolve every returned customer's sale.
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    /// Distinct public models in UTF-8 name order, at most fifty.
    pub models: Vec<ApplicationCustomerModel>,
    /// Last returned name when another page exists.
    pub next_cursor: Option<String>,
}
