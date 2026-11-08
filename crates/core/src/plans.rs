//! Sold plans and the customer plan lifecycle (issue #610, T19).
//!
//! The quota engine stays [`crate::subscriptions`]: a plan-bound entitlement
//! still counts its draws on `org_subscriptions.consumed_minor` and still
//! records them on `subscription_draws`, so `admit_subscription_draw` /
//! `settle_subscription_draw_in` / the v9 settlement description are unchanged.
//! What this module adds is the *sold* layer D06 requires on top:
//!
//! - [`SoldPlan`] is a versioned catalogue row — the terms a customer bought,
//!   immutable once published. A binding references one version; a later
//!   catalogue edit mints a new version and never rewrites a sold form.
//! - [`CustomerPlan`] is one customer's link to a version — at most one live
//!   row per organization — with the D06 state machine
//!   (`pending_payment → active → renewal_pending/past_due → active|expired`,
//!   `cancelled` the explicit exit) and the two boundary modifiers
//!   (`cancel_at_period_end`, `scheduled_change`) as explicit columns.
//! - [`PlanPeriod`] is the per-period authorization: a period exists only once
//!   its authorization does — a `payment_orders` row that fulfilled for a paid
//!   period, or a `complimentary` grant for a free plan. A timer can never
//!   open an unpaid period: the rollover path inserts the next row only when
//!   its authorization already exists.
//!
//! The boundary policy is versioned on the sold plan: `calendar_month`
//! preserves the legacy UTC-month semantics `subscription_periods` already
//! records, `anniversary` rolls on the activation date's monthly anniversary —
//! a distinct behavior, never a silent reinterpretation of the old one.

use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}

/// How a sold plan's periods bound: the legacy UTC-month calendar boundary, or
/// the activation date's monthly anniversary. The wire spelling is camelCase
/// like every API enum; `as_str`/`parse` hold the snake_case the database
/// stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PeriodKind {
    /// The legacy policy: periods close at the next UTC month boundary,
    /// matching `subscription_periods`' existing semantics.
    CalendarMonth,
    /// The versioned policy: periods roll on the activation date's monthly
    /// anniversary, clamped to the month's last day for short months (a
    /// January 31 activation renews on February 28, then back to March 31 —
    /// the anchor is the activation's day-of-month, only clamped into months
    /// too short to hold it, never permanently drifted by a clamp).
    Anniversary,
}

impl PeriodKind {
    /// The spelling `sold_plans.period_kind` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CalendarMonth => "calendar_month",
            Self::Anniversary => "anniversary",
        }
    }

    /// Reads a stored kind back; an unknown one refuses rather than guesses.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "calendar_month" => Some(Self::CalendarMonth),
            "anniversary" => Some(Self::Anniversary),
            _ => None,
        }
    }
}

/// The next period's close under `kind`, given the period that just ended.
///
/// `calendar_month` returns the first of the UTC month following `start`'s —
/// the same boundary `subscriptions::period_window(Monthly, _)` computes, so a
/// plan-bound subscription under the legacy policy closes on exactly the rows
/// the quota machinery would have written.
///
/// `anniversary` rolls on `anchor`'s day-of-month each month, clamped to the
/// target month's last day — the leap-month clamp. The anchor is the
/// activation date, carried unchanged through every roll so a clamped month
/// does not drift the anniversary back.
#[must_use]
pub fn period_end_for(
    kind: PeriodKind,
    anchor: time::Date,
    start: OffsetDateTime,
) -> OffsetDateTime {
    match kind {
        PeriodKind::CalendarMonth => {
            let date = start.date();
            let (year, month) = if date.month() == time::Month::December {
                (date.year() + 1, time::Month::January)
            } else {
                (date.year(), date.month().next())
            };
            // Day 1 always exists.
            let first = time::Date::from_calendar_date(year, month, 1)
                .unwrap_or_else(|_| unreachable!("the first of a month exists"));
            first.midnight().assume_utc()
        }
        PeriodKind::Anniversary => {
            let date = start.date();
            let (year, month) = if date.month() == time::Month::December {
                (date.year() + 1, time::Month::January)
            } else {
                (date.year(), date.month().next())
            };
            let day = anchor.day().min(days_in_month(year, month));
            let close = time::Date::from_calendar_date(year, month, day)
                .unwrap_or_else(|_| unreachable!("a clamped day exists"));
            close.midnight().assume_utc()
        }
    }
}

/// How many days `month` of `year` carries — the leap-month clamp's bound.
fn days_in_month(year: i32, month: time::Month) -> u8 {
    use time::Month::*;
    match month {
        January | March | May | July | August | October | December => 31,
        April | June | September | November => 30,
        February => {
            if time::util::is_leap_year(year) {
                29
            } else {
                28
            }
        }
    }
}

/// One version of a plan the business sells — the immutable terms a binding
/// references.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoldPlan {
    /// The version row's own id — what a binding names.
    pub plan_version_id: Uuid,
    /// The plan's stable identity across versions.
    pub plan_id: Uuid,
    /// Business that owns the catalogue entry.
    pub business_id: Uuid,
    /// Monotonically increasing version number within `plan_id`.
    pub version: i32,
    /// Display name of the plan version.
    pub name: String,
    /// The catalogue price in ledger minor units; 0 is a free plan.
    pub price_minor: i64,
    /// The display currency the price is quoted in.
    pub currency: String,
    /// Quota per period in credit minor units; `None` is unmetered.
    pub quota_minor: Option<i64>,
    /// The boundary policy the version sells under.
    pub period_kind: PeriodKind,
    /// Whether an exhausted period may overflow into the wallet — opt-in.
    pub overage_enabled: bool,
    /// Catalogue display order, lowest first.
    pub sort_order: i32,
    /// Set once the version may be sold; `None` is a draft.
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    /// Set when the version stops accepting new binds.
    #[serde(with = "time::serde::rfc3339::option")]
    pub retired_at: Option<OffsetDateTime>,
    /// When the version row was created.
    pub created_at: OffsetDateTime,
}

/// What a create or publish-version call carries.
#[derive(Debug, Clone)]
pub struct NewSoldPlan {
    /// The plan's identity; `None` mints a new one (version 1).
    pub plan_id: Option<Uuid>,
    /// Display name for the new version.
    pub name: String,
    /// Price in ledger minor units; 0 makes the plan free.
    pub price_minor: i64,
    /// Three-letter uppercase ISO 4217 code the price is quoted in.
    pub currency: String,
    /// Quota per period in credit minor units; `None` is unmetered.
    pub quota_minor: Option<i64>,
    /// Boundary policy the version sells under.
    pub period_kind: PeriodKind,
    /// Whether an exhausted period may overflow into the wallet.
    pub overage_enabled: bool,
    /// Catalogue display order, lowest first.
    pub sort_order: i32,
}

/// The customer plan's lifecycle state — D06's machine. The wire spelling is
/// camelCase; `as_str`/`parse` hold the snake_case the database stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CustomerPlanState {
    /// Bound, awaiting the first period's authorization.
    PendingPayment,
    /// An authorized period is running.
    Active,
    /// The boundary passed; the booked change awaits its authorization.
    RenewalPending,
    /// A renewal is unpaid past the boundary — the ended period's remainder
    /// still serves, no fresh quota.
    PastDue,
    /// The past-due grace closed.
    Expired,
    /// The explicit exit — a boundary cancel or an immediate one.
    Cancelled,
}

impl CustomerPlanState {
    /// The spelling `customer_plans.state` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PendingPayment => "pending_payment",
            Self::Active => "active",
            Self::RenewalPending => "renewal_pending",
            Self::PastDue => "past_due",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
    /// Reads a stored state back; an unknown one refuses rather than guesses.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "pending_payment" => Some(Self::PendingPayment),
            "active" => Some(Self::Active),
            "renewal_pending" => Some(Self::RenewalPending),
            "past_due" => Some(Self::PastDue),
            "expired" => Some(Self::Expired),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// What authorized a plan period — the wire is camelCase, the row snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanAuthorizationKind {
    /// A `payment_orders` row funded it.
    PaymentOrder,
    /// A free plan's explicit grant.
    Complimentary,
}
impl PlanAuthorizationKind {
    /// Reads a stored authorization kind back; an unknown one refuses rather
    /// than guesses.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "payment_order" => Some(Self::PaymentOrder),
            "complimentary" => Some(Self::Complimentary),
            _ => None,
        }
    }
}

/// A plan period's own lifecycle — camelCase on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanPeriodState {
    /// The period is running and may serve draws.
    Active,
    /// The period ended normally at its boundary.
    Closed,
    /// The period ended without a renewal authorization.
    Expired,
}
impl PlanPeriodState {
    /// Reads a stored period state back; an unknown one refuses rather than
    /// guesses.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "active" => Some(Self::Active),
            "closed" => Some(Self::Closed),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// One customer's link to a sold plan version — the lifecycle the entitlement
/// surface reads.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerPlan {
    /// Local customer plan identifier.
    pub customer_plan_id: Uuid,
    /// Customer organization the plan belongs to.
    pub organization_id: Uuid,
    /// The exact sold version the customer bought.
    pub plan_version_id: Uuid,
    /// The quota engine row this entitlement runs on; `None` until the first
    /// period activates.
    pub subscription_id: Option<Uuid>,
    /// `pendingPayment`, `active`, `renewalPending`, `pastDue`, `expired`
    /// or `cancelled` — D06's machine.
    pub state: CustomerPlanState,
    /// When the first period was authorized; `None` while pending payment.
    #[serde(with = "time::serde::rfc3339::option")]
    pub activated_at: Option<OffsetDateTime>,
    /// Serves through the paid period but blocks renewal when set.
    pub cancel_at_period_end: bool,
    /// A booked swap effective at the next boundary; `None` when unscheduled.
    pub scheduled_change: Option<Uuid>,
    /// When the customer plan row was created.
    pub created_at: OffsetDateTime,
    /// When the customer plan row last changed.
    pub updated_at: OffsetDateTime,
}

/// One authorized period — the row a timer alone can never write.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPeriod {
    /// Local period identifier.
    pub plan_period_id: Uuid,
    /// Customer plan this period belongs to.
    pub customer_plan_id: Uuid,
    /// Zero-based period number within the customer plan.
    pub period_index: i32,
    /// Inclusive start of the period.
    #[serde(with = "time::serde::rfc3339")]
    pub period_start: OffsetDateTime,
    /// Exclusive end of the period.
    #[serde(with = "time::serde::rfc3339")]
    pub period_end: OffsetDateTime,
    /// Quota granted for the period in credit minor units; `None` is unmetered.
    pub quota_minor: Option<i64>,
    /// Quota consumed so far, in credit minor units.
    pub consumed_minor: i64,
    /// `paymentOrder` or `complimentary` — what authorized the period.
    pub authorization_kind: PlanAuthorizationKind,
    /// The authorizing fact: the order id or the grant's key.
    pub authorization_ref: Uuid,
    /// `active`, `closed` or `expired`.
    pub state: PlanPeriodState,
    /// When the period row was created.
    pub created_at: OffsetDateTime,
}

/// The renewal authorization a `plan_periods` row needs: a paid order for a
/// priced plan, an explicit grant for a free one.
#[derive(Debug, Clone, Copy)]
pub enum PeriodAuthorization {
    /// A `payment_orders` row that funded the period — the order's id.
    PaymentOrder(Uuid),
    /// A free plan's complimentary grant — the grant's durable key.
    Complimentary(Uuid),
}

impl PeriodAuthorization {
    fn kind(&self) -> &'static str {
        match self {
            Self::PaymentOrder(_) => "payment_order",
            Self::Complimentary(_) => "complimentary",
        }
    }
    fn reference(&self) -> Uuid {
        match *self {
            Self::PaymentOrder(id) | Self::Complimentary(id) => id,
        }
    }
}

impl Db {
    /// The singleton business's id — the catalogue's owner, resolved for the
    /// unauthenticated catalogue read the way `lock_business` resolves it for
    /// the staff paths.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] when no business is initialized; storage
    /// failures surface as [`WalletError`].
    pub async fn default_business(&self) -> Result<Uuid, WalletError> {
        sqlx::query_scalar::<_, Uuid>("SELECT business_id FROM oxsum.businesses WHERE singleton")
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| WalletError::NotFound("no business is initialized".into()))
    }

    /// The published catalogue the business sells, newest version per plan.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn sold_plans(&self, business_id: Uuid) -> Result<Vec<SoldPlan>, WalletError> {
        let rows = sqlx::query(
            "SELECT plan_version_id, plan_id, business_id, version, name, price_minor, \
                    currency, quota_minor, period_kind, overage_enabled, sort_order, \
                    published_at, retired_at, created_at \
             FROM oxsum.sold_plans \
             WHERE business_id = $1 \
             ORDER BY plan_id, version DESC",
        )
        .bind(business_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(sold_plan_from_row).collect()
    }

    /// One sold version by id — the binding and order-create read.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn sold_plan(&self, plan_version_id: Uuid) -> Result<Option<SoldPlan>, WalletError> {
        let row = sqlx::query(
            "SELECT plan_version_id, plan_id, business_id, version, name, price_minor, \
                    currency, quota_minor, period_kind, overage_enabled, sort_order, \
                    published_at, retired_at, created_at \
             FROM oxsum.sold_plans WHERE plan_version_id = $1",
        )
        .bind(plan_version_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(sold_plan_from_row).transpose()
    }

    /// Mints a sold plan version. A `plan_id` of `None` opens a new plan at
    /// version 1; naming an existing plan's id publishes the next version, so
    /// a catalogue edit is a new row, never a rewrite of the sold form.
    ///
    /// The version lands published (`published_at = now()`): the staff call is
    /// the publish decision — drafts would need a second explicit step this
    /// surface does not carry.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for negative price or non-positive quota;
    /// storage failures surface as [`WalletError`].
    pub async fn publish_sold_plan(
        &self,
        business_id: Uuid,
        new: &NewSoldPlan,
    ) -> Result<SoldPlan, WalletError> {
        if new.price_minor < 0 {
            return Err(invalid("priceMinor must be zero or more"));
        }
        if let Some(quota) = new.quota_minor
            && quota <= 0
        {
            return Err(invalid("quotaMinor must be positive or absent"));
        }
        if new.currency.len() != 3 || !new.currency.bytes().all(|c| c.is_ascii_uppercase()) {
            return Err(invalid("currency must be a three-letter uppercase code"));
        }
        let plan_id = new.plan_id.unwrap_or_else(Uuid::new_v4);
        let mut tx = self.pool().begin().await?;
        // Serialize version minting per plan: two publishes cannot both claim
        // `max(version)+1`.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("sold-plan:{plan_id}"))
            .execute(&mut *tx)
            .await?;
        let version = sqlx::query_scalar::<_, i32>(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM oxsum.sold_plans WHERE plan_id = $1",
        )
        .bind(plan_id)
        .fetch_one(&mut *tx)
        .await?;
        let row = sqlx::query(
            "INSERT INTO oxsum.sold_plans \
                 (plan_version_id, plan_id, business_id, version, name, price_minor, \
                  currency, quota_minor, period_kind, overage_enabled, sort_order, published_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now()) \
             RETURNING plan_version_id, plan_id, business_id, version, name, price_minor, \
                       currency, quota_minor, period_kind, overage_enabled, sort_order, \
                       published_at, retired_at, created_at",
        )
        .bind(Uuid::new_v4())
        .bind(plan_id)
        .bind(business_id)
        .bind(version)
        .bind(&new.name)
        .bind(new.price_minor)
        .bind(&new.currency)
        .bind(new.quota_minor)
        .bind(new.period_kind.as_str())
        .bind(new.overage_enabled)
        .bind(new.sort_order)
        .fetch_one(&mut *tx)
        .await?;
        let plan = sold_plan_from_row(&row)?;
        tx.commit().await?;
        Ok(plan)
    }

    /// Retires a sold version: it stops accepting new binds, existing
    /// subscriptions run under it to their boundaries.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unknown version; storage failures
    /// surface as [`WalletError`].
    pub async fn retire_sold_plan(&self, plan_version_id: Uuid) -> Result<bool, WalletError> {
        let n = sqlx::query(
            "UPDATE oxsum.sold_plans SET retired_at = now() \
             WHERE plan_version_id = $1 AND retired_at IS NULL",
        )
        .bind(plan_version_id)
        .execute(self.pool())
        .await?;
        Ok(n.rows_affected() > 0)
    }

    /// The customer's live plan, when it has one — `pending_payment` through
    /// `past_due`; a terminal row is history, not the answer.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn customer_plan(
        &self,
        organization_id: Uuid,
    ) -> Result<Option<CustomerPlan>, WalletError> {
        let row = sqlx::query(
            "SELECT customer_plan_id, organization_id, plan_version_id, subscription_id, \
                    state, activated_at, cancel_at_period_end, scheduled_change, \
                    created_at, updated_at \
             FROM oxsum.customer_plans \
             WHERE organization_id = $1 \
               AND state IN ('pending_payment', 'active', 'renewal_pending', 'past_due')",
        )
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(customer_plan_from_row).transpose()
    }

    /// The periods a customer plan has authorized, in index order — the audit
    /// of what the entitlement ran under.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn plan_periods(
        &self,
        customer_plan_id: Uuid,
    ) -> Result<Vec<PlanPeriod>, WalletError> {
        let rows = sqlx::query(
            "SELECT plan_period_id, customer_plan_id, period_index, period_start, period_end, \
                    quota_minor, consumed_minor, authorization_kind, authorization_ref, \
                    state, created_at \
             FROM oxsum.plan_periods \
             WHERE customer_plan_id = $1 ORDER BY period_index",
        )
        .bind(customer_plan_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(plan_period_from_row).collect()
    }

    /// Binds a customer to a sold plan version: the `customer_plans` row opens
    /// `pending_payment` for a priced plan, or activates immediately under a
    /// `complimentary` grant for a free one. One live plan per customer — a
    /// second bind while one runs is a plan change the explicit
    /// `scheduled_change` path books, not a silent rebind.
    ///
    /// The authorization the first period needs is `auth`: for a free plan
    /// the caller supplies a `complimentary` grant key (the period opens at
    /// once); for a priced plan the returned `pending_payment` row waits on a
    /// `plan_period` payment order's fulfillment — the caller mints that order
    /// with `purpose_ref` = the `customer_plan_id` this returns.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unpublished or retired version;
    /// [`WalletError::Conflict`] when a live plan already binds the customer;
    /// [`WalletError::InvalidInput`] when a priced plan is offered a
    /// complimentary grant; storage failures surface as [`WalletError`].
    pub async fn bind_customer_plan(
        &self,
        organization_id: Uuid,
        plan_version_id: Uuid,
        auth: PeriodAuthorization,
    ) -> Result<CustomerPlan, WalletError> {
        let plan = self
            .sold_plan(plan_version_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("no such plan version".into()))?;
        if plan.published_at.is_none() || plan.retired_at.is_some() {
            return Err(WalletError::NotFound(
                "the plan version is not on sale".into(),
            ));
        }
        // The authorization must match the price: a priced plan binds
        // `pending_payment` and activates only when its `planPeriod` order
        // fulfills (`activate_plan_period`); a free plan activates at once on
        // the explicit grant — never a fabricated paid record.
        match (plan.price_minor == 0, auth) {
            // Priced: the binding waits for the order's fulfillment.
            (false, PeriodAuthorization::PaymentOrder(_)) => {}
            // Free: the grant activates it now.
            (true, PeriodAuthorization::Complimentary(_)) => {}
            (true, PeriodAuthorization::PaymentOrder(_)) => {
                return Err(invalid(
                    "a free plan takes a complimentary grant, not payment",
                ));
            }
            (false, PeriodAuthorization::Complimentary(_)) => {
                return Err(invalid("a priced plan cannot activate on a grant"));
            }
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.customer_plan' || $1::text, 0))",
        )
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await?;
        let live = sqlx::query(
            "SELECT customer_plan_id, state FROM oxsum.customer_plans \
             WHERE organization_id = $1 \
               AND state IN ('pending_payment','active','renewal_pending','past_due') \
             FOR UPDATE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await?;
        if live.is_some() {
            return Err(WalletError::Conflict(
                "the customer already has a live plan".into(),
            ));
        }
        let customer_plan_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        // A free plan's grant activates it in the same transaction; a priced
        // one stays pending until its order fulfills.
        let (state, activated_at) = match auth {
            PeriodAuthorization::Complimentary(_) => ("active", Some(now)),
            PeriodAuthorization::PaymentOrder(_) => ("pending_payment", None),
        };
        sqlx::query(
            "INSERT INTO oxsum.customer_plans \
                 (customer_plan_id, organization_id, plan_version_id, state, activated_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(customer_plan_id)
        .bind(organization_id)
        .bind(plan_version_id)
        .bind(state)
        .bind(activated_at)
        .execute(&mut *tx)
        .await?;
        if let PeriodAuthorization::Complimentary(_) = auth {
            // Open period 0 and the quota engine row under the grant — the
            // free plan's authorization lands the entitlement directly.
            let subscription_id =
                open_subscription_in(&mut tx, organization_id, &plan, now).await?;
            sqlx::query(
                "UPDATE oxsum.customer_plans SET subscription_id = $2 WHERE customer_plan_id = $1",
            )
            .bind(customer_plan_id)
            .bind(subscription_id)
            .execute(&mut *tx)
            .await?;
            insert_plan_period(
                &mut tx,
                customer_plan_id,
                0,
                now,
                period_end_for(plan.period_kind, now.date(), now),
                plan.quota_minor,
                auth,
            )
            .await?;
        }
        tx.commit().await?;
        self.customer_plan(organization_id)
            .await?
            .ok_or_else(|| invalid("the customer plan did not land"))
    }

    /// Activates the period a payment order funded — the `plan_period`
    /// fulfillment arm. The order's `purpose_ref` names the `customer_plans`
    /// row it buys for: period 0 for a `pending_payment` bind, or the next
    /// index for a `renewal_pending`/`past_due` one. The period row, the quota
    /// engine's bind or rollover, and the customer plan's state move inside
    /// one transaction — a timer never writes them.
    ///
    /// Idempotent under the order id: a replayed fulfillment finds the period
    /// it already authorized (the `(authorization_kind, authorization_ref)`
    /// unique pair) and returns the standing plan.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an order naming no live plan;
    /// [`WalletError::Conflict`] for a paid order on a cancelled or expired
    /// plan; storage failures surface as [`WalletError`].
    pub async fn activate_plan_period(
        &self,
        order_id: Uuid,
        customer_plan_id: Uuid,
    ) -> Result<CustomerPlan, WalletError> {
        let mut tx = self.pool().begin().await?;
        // Match the paid fact before taking entitlement locks. The subscription
        // advisory lock precedes its row and the customer-plan row, matching
        // admission and the boundary worker so renewal cannot deadlock them.
        let order = crate::payment_orders::order_in(&mut tx, order_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("no such payment order".into()))?;
        if order.purpose != crate::PaymentOrderPurpose::PlanPeriod
            || order.purpose_ref != Some(customer_plan_id)
            || !matches!(order.status.as_str(), "paid" | "fulfilled")
            || order.paid_at.is_none()
        {
            return Err(WalletError::Conflict(
                "plan activation requires its matching paid order".into(),
            ));
        }
        let organization_id = order.organization_id;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text,0))",
        )
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query("SELECT subscription_id FROM oxsum.org_subscriptions WHERE organization_id=$1 AND state='active' FOR UPDATE")
            .bind(organization_id).fetch_optional(&mut *tx).await?;
        let plan_row = sqlx::query(
            "SELECT customer_plan_id, organization_id, plan_version_id, subscription_id, \
                    state, activated_at, cancel_at_period_end, scheduled_change \
             FROM oxsum.customer_plans WHERE customer_plan_id=$1 AND organization_id=$2 FOR UPDATE",
        )
        .bind(customer_plan_id)
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("no such customer plan".into()))?;
        // Historical fulfilled periods retain their original authorization. A
        // replay cannot use one order to answer a different customer's plan.
        let replayed: Option<Uuid> = sqlx::query_scalar(
            "SELECT customer_plan_id FROM oxsum.plan_periods \
             WHERE authorization_kind='payment_order' AND authorization_ref=$1",
        )
        .bind(order_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(owner) = replayed {
            if owner != customer_plan_id {
                return Err(WalletError::Conflict(
                    "the order already authorized another plan".into(),
                ));
            }
            let standing = customer_plan_in(&mut tx, customer_plan_id).await?;
            tx.commit().await?;
            return standing.ok_or_else(|| invalid("the customer plan did not land"));
        }
        let returning_funds: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.refunds WHERE order_id=$1 AND (state IN ('requested','provider_pending','succeeded') OR EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=refunds.refund_id)))")
            .bind(order_id).fetch_one(&mut *tx).await?;
        if returning_funds {
            return Err(WalletError::Conflict(
                "a refund-reserved payment cannot activate a new period".into(),
            ));
        }
        let terms = order.plan_payment.as_ref().ok_or_else(|| {
            WalletError::Conflict(
                "legacy plan payment has no verified period intent; finance review is required"
                    .into(),
            )
        })?;
        crate::plan_payments::validate_order(&order, terms)?;
        let state = CustomerPlanState::parse(&plan_row.try_get::<String, _>("state")?)
            .ok_or_else(|| invalid("unknown customer plan state"))?;
        let plan_version_id: Uuid = plan_row.try_get("plan_version_id")?;
        let sold = self
            .sold_plan(plan_version_id)
            .await?
            .ok_or_else(|| invalid("the bound plan version is gone"))?;
        if terms.plan_version_id != plan_version_id
            || terms.fee_minor != sold.price_minor
            || terms.display_currency != sold.currency
            || plan_row.try_get::<bool, _>("cancel_at_period_end")?
            || plan_row
                .try_get::<Option<Uuid>, _>("scheduled_change")?
                .is_some()
        {
            return Err(WalletError::Conflict(
                "the paid intent no longer matches the plan; finance review is required".into(),
            ));
        }
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        // Open once before any ledger writes. Reopening this wallet after the
        // closing journal has appended would run bootstrap DDL on another pool
        // connection while this transaction still holds the ledger locks.
        let wallet = self.journal_wallet(&mut tx, organization_id).await?;
        match state {
            CustomerPlanState::PendingPayment => {
                if terms.period_index != 0
                    || terms.period_start.is_some()
                    || terms.period_end.is_some()
                {
                    return Err(WalletError::Conflict(
                        "payment is not for the initial plan period".into(),
                    ));
                }
                // Initial service starts when its paid activation commits.
                let subscription_id =
                    open_subscription_in(&mut tx, organization_id, &sold, now).await?;
                sqlx::query(
                    "UPDATE oxsum.customer_plans \
                     SET subscription_id = $2, state = 'active', activated_at = $3, \
                         updated_at = now() \
                     WHERE customer_plan_id = $1",
                )
                .bind(customer_plan_id)
                .bind(subscription_id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                insert_plan_period(
                    &mut tx,
                    customer_plan_id,
                    0,
                    now,
                    period_end_for(sold.period_kind, now.date(), now),
                    sold.quota_minor,
                    PeriodAuthorization::PaymentOrder(order_id),
                )
                .await?;
            }
            CustomerPlanState::Active
            | CustomerPlanState::RenewalPending
            | CustomerPlanState::PastDue => {
                // Renewal: the next index's period opens under the order. The
                // period's start is the previous period's end — never `now`,
                // which would skip the gap's audit; a past-due renewal still
                // chains off the boundary it owed.
                let last = sqlx::query(
                    "SELECT period_index, period_end FROM oxsum.plan_periods \
                     WHERE customer_plan_id = $1 ORDER BY period_index DESC LIMIT 1",
                )
                .bind(customer_plan_id)
                .fetch_one(&mut *tx)
                .await?;
                let index = last
                    .try_get::<i32, _>("period_index")?
                    .checked_add(1)
                    .ok_or_else(|| invalid("plan period index overflows"))?;
                let start: OffsetDateTime = last.try_get("period_end")?;
                let anchor = plan_row
                    .try_get::<Option<OffsetDateTime>, _>("activated_at")?
                    .unwrap_or(now)
                    .date();
                let end = period_end_for(sold.period_kind, anchor, start);
                if start > now
                    || terms.period_index != index
                    || terms.period_start != Some(start)
                    || terms.period_end != Some(end)
                {
                    return Err(WalletError::Conflict(
                        "payment is not for the due plan period".into(),
                    ));
                }
                close_open_plan_period(&mut tx, customer_plan_id, "closed").await?;
                // Close the preceding period with its own sold funding. The
                // renewal's fee prepays only the period it authorizes.
                self.write_close_journal_with_wallet(&mut tx, &plan_row, &wallet)
                    .await?;
                // The quota engine's window follows the authorized period:
                // its own rollover is bypassed for plan-bound rows, so the
                // renewal writes the window itself.
                roll_subscription_window_in(&mut tx, organization_id, start, end, &sold).await?;
                sqlx::query(
                    "UPDATE oxsum.customer_plans \
                     SET state = 'active', cancel_at_period_end = false, \
                         scheduled_change = NULL, updated_at = now() \
                     WHERE customer_plan_id = $1",
                )
                .bind(customer_plan_id)
                .execute(&mut *tx)
                .await?;
                insert_plan_period(
                    &mut tx,
                    customer_plan_id,
                    index,
                    start,
                    end,
                    sold.quota_minor,
                    PeriodAuthorization::PaymentOrder(order_id),
                )
                .await?;
            }
            other => {
                return Err(WalletError::Conflict(format!(
                    "a {} plan cannot take a renewal",
                    other.as_str()
                )));
            }
        }
        // A period fee funds this period in full. Outstanding statements have
        // their own repayment intent and never consume a renewal's prepayment.
        wallet
            .plan_prepay_in(&mut tx, &order_id, terms.fee_minor, now.date())
            .await?;
        let standing = customer_plan_in(&mut tx, customer_plan_id).await?;
        tx.commit().await?;
        standing.ok_or_else(|| invalid("the customer plan did not land"))
    }

    /// Marks `cancel_at_period_end`: the paid period still serves to its end —
    /// the draws it funded are honored — but the boundary does not renew.
    /// Cancelling mid-period keeps serving; that is the explicit choice D06
    /// makes over pro-rating.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] when the customer has no live plan; storage
    /// failures surface as [`WalletError`].
    pub async fn cancel_customer_plan(
        &self,
        organization_id: Uuid,
    ) -> Result<CustomerPlan, WalletError> {
        let mut tx = self.pool().begin().await?;
        lock_plan_change_in(&mut tx, organization_id).await?;
        let n = sqlx::query(
            "UPDATE oxsum.customer_plans \
             SET cancel_at_period_end = true, scheduled_change = NULL, updated_at = now() \
             WHERE organization_id = $1 AND state IN ('active','renewal_pending','past_due')",
        )
        .bind(organization_id)
        .execute(&mut *tx)
        .await?;
        if n.rows_affected() == 0 {
            return Err(WalletError::NotFound(
                "the customer has no live plan".into(),
            ));
        }
        tx.commit().await?;
        self.customer_plan(organization_id)
            .await?
            .ok_or_else(|| invalid("the customer plan did not land"))
    }

    /// Books a plan-version swap effective at the next boundary: the current
    /// period serves to its end, the new version's first period authorizes at
    /// that boundary (a payment order for a priced target, a grant for a free
    /// one). No mid-period pro-rating — the change is a boundary event.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for no live plan or an unsellable target;
    /// storage failures surface as [`WalletError`].
    pub async fn schedule_customer_plan_change(
        &self,
        organization_id: Uuid,
        plan_version_id: Uuid,
    ) -> Result<CustomerPlan, WalletError> {
        let target = self
            .sold_plan(plan_version_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("no such plan version".into()))?;
        if target.published_at.is_none() || target.retired_at.is_some() {
            return Err(WalletError::NotFound(
                "the plan version is not on sale".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        lock_plan_change_in(&mut tx, organization_id).await?;
        let n = sqlx::query(
            "UPDATE oxsum.customer_plans \
             SET scheduled_change = $2, cancel_at_period_end = false, updated_at = now() \
             WHERE organization_id = $1 AND state IN ('active','renewal_pending','past_due')",
        )
        .bind(organization_id)
        .bind(plan_version_id)
        .execute(&mut *tx)
        .await?;
        if n.rows_affected() == 0 {
            return Err(WalletError::NotFound(
                "the customer has no live plan".into(),
            ));
        }
        tx.commit().await?;
        self.customer_plan(organization_id)
            .await?
            .ok_or_else(|| invalid("the customer plan did not land"))
    }

    /// The boundary pass a plan-bound subscription needs when its period ends:
    /// either an authorized next period exists (a fulfilled renewal order or a
    /// grant — apply it), or the plan slips to `renewal_pending`/`past_due`
    /// and the subscription stays in its ended window rather than rolling on
    /// the timer alone. `cancel_at_period_end` closes the plan instead.
    ///
    /// Called from [`crate::subscriptions::Db::admit_subscription_draw`]'s lazy
    /// rollover and the `subscription-rollovers` job when the subscription is
    /// plan-bound — the seam that makes "a timer cannot issue unpaid
    /// entitlement" hold on the legacy quota machinery.
    ///
    /// Every path that closes the period also writes its fee journal in the
    /// same transaction (issue #611, T20) — the close and the bill are atomic.
    ///
    /// Returns whether the subscription may draw in a fresh window: `true`
    /// when a new authorized period opened, `false` when it stays bound to
    /// the ended one (past-due draws still meter against its remainder).
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub(crate) async fn plan_rollover_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        subscription_id: Uuid,
    ) -> Result<bool, WalletError> {
        let Some(plan_row) = sqlx::query(
            "SELECT customer_plan_id, organization_id, plan_version_id, state, \
                    activated_at, cancel_at_period_end, scheduled_change \
             FROM oxsum.customer_plans \
             WHERE subscription_id = $1 \
               AND state IN ('active','renewal_pending','past_due') FOR UPDATE",
        )
        .bind(subscription_id)
        .fetch_optional(&mut **tx)
        .await?
        else {
            // Not plan-bound: the caller runs the legacy free-rolling path.
            return Ok(true);
        };
        let customer_plan_id: Uuid = plan_row.try_get("customer_plan_id")?;
        let cancel_at_end: bool = plan_row.try_get("cancel_at_period_end")?;
        let scheduled: Option<Uuid> = plan_row.try_get("scheduled_change")?;
        let last = sqlx::query(
            "SELECT period_index, period_end FROM oxsum.plan_periods \
             WHERE customer_plan_id = $1 ORDER BY period_index DESC LIMIT 1",
        )
        .bind(customer_plan_id)
        .fetch_one(&mut **tx)
        .await?;
        let next_index: i32 = last.try_get::<i32, _>("period_index")? + 1;
        if cancel_at_end {
            // Boundary cancel: the paid period ends, no new one opens, the
            // plan goes `cancelled` — the wallet takes every later draw. The
            // closing period's journal lands in the same transaction.
            close_open_plan_period(tx, customer_plan_id, "closed").await?;
            self.write_close_journal(tx, &plan_row).await?;
            sqlx::query(
                "UPDATE oxsum.customer_plans \
                 SET state = 'cancelled', updated_at = now() WHERE customer_plan_id = $1",
            )
            .bind(customer_plan_id)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "UPDATE oxsum.org_subscriptions \
                 SET state = 'cancelled', updated_at = now() WHERE subscription_id = $1",
            )
            .bind(subscription_id)
            .execute(&mut **tx)
            .await?;
            return Ok(false);
        }
        // The renewal's authorization is a fulfilled order the caller already
        // ran `activate_plan_period` for, or a scheduled change whose new
        // version's first period still needs its own authorization. A pending
        // order alone cannot open a period — only a fulfilled one can, through
        // `activate_plan_period`. So the boundary finds either a standing
        // authorized next period (nothing to do) or none (go past-due).
        let authorized = sqlx::query(
            "SELECT 1 FROM oxsum.plan_periods \
             WHERE customer_plan_id = $1 AND period_index = $2",
        )
        .bind(customer_plan_id)
        .bind(next_index)
        .fetch_optional(&mut **tx)
        .await?;
        if authorized.is_some() {
            return Ok(true);
        }
        if let Some(target) = scheduled {
            // A booked change applies at the boundary: the plan's version swaps
            // and the new first period indexes from 0 under the new version —
            // but only once its own authorization lands. Until then the plan
            // waits `renewal_pending`, still drawing the ended period's
            // remainder. The journal is written under the *closing* version,
            // so it must land before the swap overwrites `plan_version_id`.
            close_open_plan_period(tx, customer_plan_id, "closed").await?;
            self.write_close_journal(tx, &plan_row).await?;
            sqlx::query(
                "UPDATE oxsum.customer_plans \
                 SET plan_version_id = $2, scheduled_change = NULL, \
                     state = 'renewal_pending', updated_at = now() \
                 WHERE customer_plan_id = $1",
            )
            .bind(customer_plan_id)
            .bind(target)
            .execute(&mut **tx)
            .await?;
            return Ok(false);
        }
        close_open_plan_period(tx, customer_plan_id, "expired").await?;
        // Past-due: the closed period's journal lands `posted` — the open
        // receivable tracked separately from the next period's renewal fee.
        self.write_close_journal(tx, &plan_row).await?;
        sqlx::query(
            "UPDATE oxsum.customer_plans \
             SET state = 'past_due', updated_at = now() WHERE customer_plan_id = $1",
        )
        .bind(customer_plan_id)
        .execute(&mut **tx)
        .await?;
        // The subscription stays bound to its ended window: the period's
        // remaining quota still draws (past-due service), but no fresh quota
        // issues until a renewal order fulfills.
        Ok(false)
    }

    /// The tenant ledger a plan-bound journal posts to (issue #611, T20):
    /// resolved inside the caller's transaction from the organization's own
    /// row, so a close or an activation never reaches outside its org.
    pub(crate) async fn journal_wallet(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization_id: Uuid,
    ) -> Result<crate::Wallet, WalletError> {
        let tenant: Option<String> = sqlx::query_scalar(
            "SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1",
        )
        .bind(organization_id)
        .fetch_optional(&mut **tx)
        .await?;
        let tenant = tenant.ok_or_else(|| invalid("the organization has no tenant"))?;
        crate::Wallet::open(self.pool().clone(), &tenant).await
    }

    /// Writes the journal for the `plan_periods` row a close just marked —
    /// the one shared write every close path (`plan_rollover_in`'s arms and
    /// `activate_plan_period`'s renewal) runs, so the bill and the period's
    /// end are one transaction (issue #611, T20). `plan_row` is the
    /// `customer_plans` read taken before the close: the sold version the
    /// period ran under is resolved from it, so a scheduled change's swap in
    /// the same transaction cannot bill the *new* version's price for the
    /// *old* period.
    ///
    /// A journal already standing for the period is a replay — the
    /// `(customer_plan_id, period_index)` partial unique refuses the second
    /// live row, and the close's ledger entries are idempotent under their
    /// journal-derived keys.
    async fn write_close_journal(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        plan_row: &sqlx::postgres::PgRow,
    ) -> Result<(), WalletError> {
        let organization_id: Uuid = plan_row.try_get("organization_id")?;
        let wallet = self.journal_wallet(tx, organization_id).await?;
        self.write_close_journal_with_wallet(tx, plan_row, &wallet)
            .await
    }

    async fn write_close_journal_with_wallet(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        plan_row: &sqlx::postgres::PgRow,
        wallet: &crate::Wallet,
    ) -> Result<(), WalletError> {
        let customer_plan_id: Uuid = plan_row.try_get("customer_plan_id")?;
        let plan_version_id: Uuid = plan_row.try_get("plan_version_id")?;
        let Some(period) = crate::journals::closing_period_in(tx, customer_plan_id).await? else {
            return Ok(());
        };
        let Some(sold) = self.sold_plan(plan_version_id).await? else {
            return Err(invalid("the bound plan version is gone"));
        };
        self.journal_closing_period_in(
            tx,
            wallet,
            plan_row,
            &period,
            &sold,
            OffsetDateTime::now_utc().date(),
        )
        .await?;
        Ok(())
    }
}

/// A captured period-payment intent must resolve before changing its assignment.
/// Lock the assignment like the quote writer; order reads need no row lock.
async fn lock_plan_change_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization: Uuid,
) -> Result<(), WalletError> {
    let plan: Option<Uuid> = sqlx::query_scalar("SELECT customer_plan_id FROM oxsum.customer_plans WHERE organization_id=$1 AND state IN ('active','renewal_pending','past_due') FOR UPDATE")
        .bind(organization).fetch_optional(&mut **tx).await?;
    let plan = plan.ok_or_else(|| WalletError::NotFound("the customer has no live plan".into()))?;
    let pending: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.payment_orders WHERE purpose='plan_period' AND purpose_ref=$1 AND status IN ('created','provider_pending','paid','needs_attention') AND plan_payment IS NOT NULL AND NOT EXISTS(SELECT 1 FROM oxsum.plan_periods p WHERE p.authorization_kind='payment_order' AND p.authorization_ref=payment_orders.order_id))")
        .bind(plan).fetch_one(&mut **tx).await?;
    if pending {
        return Err(WalletError::Conflict(
            "resolve the existing period payment before changing or cancelling the plan".into(),
        ));
    }
    Ok(())
}

/// The plan-bound check `subscriptions.rs` calls before its lazy rollover:
/// `Some` when the subscription is driven by `plan_periods`, `None` for a
/// legacy free-rolling row.
pub(crate) async fn plan_bound_subscription(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    subscription_id: Uuid,
) -> Result<Option<Uuid>, WalletError> {
    let row = sqlx::query_scalar::<_, Uuid>(
        "SELECT customer_plan_id FROM oxsum.customer_plans \
         WHERE subscription_id = $1 \
           AND state IN ('pending_payment','active','renewal_pending','past_due')",
    )
    .bind(subscription_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
}

/// Opens the quota engine row a customer plan runs on: one `org_subscriptions`
/// bound to a `subscription_plans` row mirrored off the sold version's terms —
/// created if the catalogue lacks it — so `admit_subscription_draw`, the
/// `subscription_periods` audit and the v9 settlement field all see the plan
/// through the legacy shapes they already know.
///
/// The mirrored plan row's `reset_period` is `never`: a plan-bound
/// subscription's window is set explicitly from `plan_periods` (see
/// [`roll_subscription_window_in`]), so the free-rolling cadence must not also
/// fire — `never` gives it no boundary to roll on its own.
async fn open_subscription_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    sold: &SoldPlan,
    now: OffsetDateTime,
) -> Result<Uuid, WalletError> {
    // The mirrored catalogue row: one `subscription_plans` row per sold
    // version, named by the version id so it is stable and unique. Its terms
    // snapshot the version's; `allow_wallet_overflow` is the plan's
    // `overage_enabled` — the explicit opt-in, never a silent default.
    let plan_id = mirror_sold_plan_in(tx, sold).await?;
    // Any prior live binding for this org has already been cancelled by the
    // one-live-plan index plus the bind path's conflict check, so the insert
    // under the org's advisory lock cannot meet a second active row.
    let subscription_id = Uuid::new_v4();
    let end = period_end_for(sold.period_kind, now.date(), now);
    sqlx::query(
        "INSERT INTO oxsum.org_subscriptions \
             (subscription_id, organization_id, plan_id, state, \
              current_period_start, current_period_end, consumed_minor, quota_minor, \
              allow_wallet_overflow, source) \
         VALUES ($1, $2, $3, 'active', $4, $5, 0, $6, $7, 'plan')",
    )
    .bind(subscription_id)
    .bind(organization_id)
    .bind(plan_id)
    .bind(now)
    .bind(end)
    .bind(sold.quota_minor)
    .bind(sold.overage_enabled)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO oxsum.subscription_periods \
             (period_id, subscription_id, organization_id, plan_id, \
              period_start, period_end, quota_minor, outcome) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'open')",
    )
    .bind(Uuid::new_v4())
    .bind(subscription_id)
    .bind(organization_id)
    .bind(plan_id)
    .bind(now)
    .bind(end)
    .bind(sold.quota_minor)
    .execute(&mut **tx)
    .await?;
    Ok(subscription_id)
}

/// Resolve the quota engine's immutable mirror of the sold version.
async fn mirror_sold_plan_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sold: &SoldPlan,
) -> Result<Uuid, WalletError> {
    let plan_version_id = sold.plan_version_id;
    let mirror_name = format!("sold-plan:{plan_version_id}");
    let plan_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO oxsum.subscription_plans \
             (plan_id, name, quota_minor, reset_period, allow_wallet_overflow, \
              price_minor, enabled, sort_order) \
         VALUES ($1, $2, $3, 'never', $4, $5, false, $6) \
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name \
         RETURNING plan_id",
    )
    .bind(Uuid::new_v4())
    .bind(&mirror_name)
    .bind(sold.quota_minor)
    .bind(sold.overage_enabled)
    .bind(sold.price_minor)
    .bind(sold.sort_order)
    .fetch_one(&mut **tx)
    .await?;
    Ok(plan_id)
}

/// Writes a `plan_periods` row — a period that exists because its
/// authorization does. The `(customer_plan_id, period_index)` and
/// `(authorization_kind, authorization_ref)` uniques make a replayed
/// fulfillment a no-op rather than a second grant.
async fn insert_plan_period(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    customer_plan_id: Uuid,
    period_index: i32,
    start: OffsetDateTime,
    end: OffsetDateTime,
    quota_minor: Option<i64>,
    auth: PeriodAuthorization,
) -> Result<Uuid, WalletError> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO oxsum.plan_periods \
             (plan_period_id, customer_plan_id, period_index, period_start, period_end, \
              quota_minor, authorization_kind, authorization_ref, state) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active') \
         ON CONFLICT (customer_plan_id, period_index) DO NOTHING",
    )
    .bind(id)
    .bind(customer_plan_id)
    .bind(period_index)
    .bind(start)
    .bind(end)
    .bind(quota_minor)
    .bind(auth.kind())
    .bind(auth.reference())
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Closes the open `plan_periods` row under `outcome` — the boundary's mark on
/// the authorization ledger, paired with the `subscription_periods` row the
/// quota engine closes.
async fn close_open_plan_period(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    customer_plan_id: Uuid,
    state: &str,
) -> Result<(), WalletError> {
    sqlx::query(
        "UPDATE oxsum.plan_periods SET state = $2 \
         WHERE customer_plan_id = $1 AND state = 'active'",
    )
    .bind(customer_plan_id)
    .bind(state)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Moves the quota engine's window to the authorized period's bounds and
/// closes the old `subscription_periods` row as `boundary` — the renewal's
/// write on the legacy audit table, kept in step with the `plan_periods` row
/// the same transaction opens.
async fn roll_subscription_window_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    start: OffsetDateTime,
    end: OffsetDateTime,
    sold: &SoldPlan,
) -> Result<(), WalletError> {
    let sub = sqlx::query(
        "SELECT subscription_id, plan_id, consumed_minor \
         FROM oxsum.org_subscriptions \
         WHERE organization_id = $1 AND state = 'active' FOR UPDATE",
    )
    .bind(organization_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(sub) = sub else {
        return Ok(());
    };
    let subscription_id: Uuid = sub.try_get("subscription_id")?;
    let plan_id = mirror_sold_plan_in(tx, sold).await?;
    let consumed: i64 = sub.try_get("consumed_minor")?;
    sqlx::query(
        "UPDATE oxsum.subscription_periods \
         SET consumed_minor = $2, outcome = 'boundary' \
         WHERE subscription_id = $1 AND outcome = 'open'",
    )
    .bind(subscription_id)
    .bind(consumed)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE oxsum.org_subscriptions \
         SET current_period_start = $2, current_period_end = $3, consumed_minor = 0, \
             quota_minor = $4, plan_id = $5, allow_wallet_overflow = $6, updated_at = now() \
         WHERE subscription_id = $1",
    )
    .bind(subscription_id)
    .bind(start)
    .bind(end)
    .bind(sold.quota_minor)
    .bind(plan_id)
    .bind(sold.overage_enabled)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO oxsum.subscription_periods \
             (period_id, subscription_id, organization_id, plan_id, \
              period_start, period_end, quota_minor, outcome) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'open')",
    )
    .bind(Uuid::new_v4())
    .bind(subscription_id)
    .bind(organization_id)
    .bind(plan_id)
    .bind(start)
    .bind(end)
    .bind(sold.quota_minor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The live `customer_plans` row inside the caller's transaction.
async fn customer_plan_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    customer_plan_id: Uuid,
) -> Result<Option<CustomerPlan>, WalletError> {
    let row = sqlx::query(
        "SELECT customer_plan_id, organization_id, plan_version_id, subscription_id, \
                state, activated_at, cancel_at_period_end, scheduled_change, \
                created_at, updated_at \
         FROM oxsum.customer_plans WHERE customer_plan_id = $1",
    )
    .bind(customer_plan_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(customer_plan_from_row).transpose()
}

fn sold_plan_from_row(row: &sqlx::postgres::PgRow) -> Result<SoldPlan, WalletError> {
    Ok(SoldPlan {
        plan_version_id: row.try_get("plan_version_id")?,
        plan_id: row.try_get("plan_id")?,
        business_id: row.try_get("business_id")?,
        version: row.try_get("version")?,
        name: row.try_get("name")?,
        price_minor: row.try_get("price_minor")?,
        currency: row.try_get("currency")?,
        quota_minor: row.try_get("quota_minor")?,
        period_kind: PeriodKind::parse(row.try_get::<&str, _>("period_kind")?)
            .ok_or_else(|| invalid("unknown period kind"))?,
        overage_enabled: row.try_get("overage_enabled")?,
        sort_order: row.try_get("sort_order")?,
        published_at: row.try_get("published_at")?,
        retired_at: row.try_get("retired_at")?,
        created_at: row.try_get("created_at")?,
    })
}

fn customer_plan_from_row(row: &sqlx::postgres::PgRow) -> Result<CustomerPlan, WalletError> {
    Ok(CustomerPlan {
        customer_plan_id: row.try_get("customer_plan_id")?,
        organization_id: row.try_get("organization_id")?,
        plan_version_id: row.try_get("plan_version_id")?,
        subscription_id: row.try_get("subscription_id")?,
        state: CustomerPlanState::parse(&row.try_get::<String, _>("state")?)
            .ok_or_else(|| invalid("unknown customer plan state"))?,
        activated_at: row.try_get("activated_at")?,
        cancel_at_period_end: row.try_get("cancel_at_period_end")?,
        scheduled_change: row.try_get("scheduled_change")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn plan_period_from_row(row: &sqlx::postgres::PgRow) -> Result<PlanPeriod, WalletError> {
    Ok(PlanPeriod {
        plan_period_id: row.try_get("plan_period_id")?,
        customer_plan_id: row.try_get("customer_plan_id")?,
        period_index: row.try_get("period_index")?,
        period_start: row.try_get("period_start")?,
        period_end: row.try_get("period_end")?,
        quota_minor: row.try_get("quota_minor")?,
        consumed_minor: row.try_get("consumed_minor")?,
        authorization_kind: PlanAuthorizationKind::parse(
            &row.try_get::<String, _>("authorization_kind")?,
        )
        .ok_or_else(|| invalid("unknown authorization kind"))?,
        authorization_ref: row.try_get("authorization_ref")?,
        state: PlanPeriodState::parse(&row.try_get::<String, _>("state")?)
            .ok_or_else(|| invalid("unknown plan period state"))?,
        created_at: row.try_get("created_at")?,
    })
}
