//! Financial facts, cost coverage and reconciliation (issue #613, task T22).
//!
//! Financial facts separate service, recognition, billing and cash. A customer asks
//! "what did my turns cost" (`usage`); finance asks "what would those turns
//! have cost at catalog rate" (`rated`, so a discount's size is visible); the
//! plan asks "what did the subscription cover" (`entitlement`); the rail asks
//! "what cash arrived" (`funding`); the ledger asks "what did the business
//! earn" (`earned`); the receivable asks "what was collected" (`collected`)
//! and "what is still owed" (`receivable`); the refund log asks "what went
//! back" (`refund`). This module is the read model that answers them from
//! the same source rows, so the customer's filtered view
//! (`GET /api/v1/org/usage-report`) and the staff's
//! (`GET /api/v1/business/financial-facts`) are one projection, never two
//! totals that could disagree.
//!
//! The amounts are *separate* by design: `usage` and `funding` will not equal
//! each other inside a period — a customer spends from a topped-up wallet, so
//! the period's charges and the period's cash are different facts. What must
//! hold for a single source is `billed - collected = receivable`. Different
//! report clocks do not imply equality between differently filtered cohorts.
//! Usage, quota coverage and provider costs retain their separate operational
//! reports; financial reconciliation explicitly excludes them from actual totals.
//!
//! Reconciliation independently reconstructs official ledger entries on one
//! repeatable-read transaction and compares original accounting sources. Every
//! attempt retains immutable evidence and explicit coverage; unsupported fields
//! are excluded rather than copied into `actual_totals`. Runtime implementation
//! lives in `financial_reconciliation`; reads and explicit exception resolutions
//! remain here. A repair is audited and never silently performed by a scan.

use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::business_staff as authority;
use crate::business_staff::BusinessCapability;
use crate::db::Db;
use crate::error::{self, WalletError};
use crate::usage::UsageRecord;

// ---------------------------------------------------------------------------
// Financial categories
// ---------------------------------------------------------------------------

/// The figures a period's books answer, all integer minor units. The
/// serde names are the contract's camelCase (`usageMinor`, `ratedMinor`, …).
///
/// Every figure is a *sum over a filtered set of source rows*, never a
/// snapshot a writer could have updated independently — the read model
/// derives, it does not store, so a repair that corrects a source row moves
/// the next read's figure automatically.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinancialFacts {
    /// Σ `usage_records.charged_minor` over settled turns: what the customer
    /// was billed for usage in the period.
    pub usage_minor: i64,
    /// The same turns re-priced at their recorded `price_version`'s catalog
    /// rate — before any discount — so `rated − usage` is the discount the
    /// period's sale terms granted. A row whose price version no longer
    /// resolves contributes its `charged_minor` (a deleted price cannot
    /// re-rate, so the honest figure is what was charged).
    pub rated_minor: i64,
    /// What the plan covered: Σ `subscription_draws.settled_minor` plus the
    /// `plan_periods.consumed_minor` the period's quota absorbed. The part of
    /// `usage` the customer did not pay out of wallet.
    pub entitlement_minor: i64,
    /// Cash in: Σ `payment_orders.amount_minor` over `paid`/`fulfilled`
    /// orders, plus credited `deposits.amount_minor` — every rail that funds
    /// a wallet or a receivable.
    pub funding_minor: i64,
    /// Net recognized plan fees: legacy closing totals plus immutable v2
    /// recognition/correction effects, never the prepaid invoice total.
    pub earned_minor: i64,
    /// Net billed plan fees, separate from recognition timing.
    pub billed_minor: i64,
    /// Net v2 deferred fee movement under the selected source clock.
    pub deferred_minor: i64,
    /// Noncash discounts/promotions; confirmed cash refunds are separate.
    pub discount_minor: i64,
    /// What was collected: Σ `invoice_journals.settled_minor` over the same
    /// journals — the part of `billed` already paid.
    pub collected_minor: i64,
    /// Cash returned once, from exact allocations or proven historical entries.
    /// A matching journal discount is not a second cash return.
    pub refund_minor: i64,
    /// Still owed: Σ `(total_minor − paid_minor)` over `issued`/`past_due`
    /// plan statements plus `(total_minor − settled_minor)` over `posted`
    /// invoice journals with no live statement — the open receivable.
    pub receivable_minor: i64,
}

/// Which clock a facts read periodizes under — the contract's `timeBasis`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TimeBasis {
    /// `usage_records.settled_at` — when the charge booked.
    #[default]
    Booking,
    /// The settlement entry's `booking_date` — the service day.
    Service,
    /// `payment_orders.paid_at` (and the money tables' own paid stamps).
    Payment,
}

impl TimeBasis {
    /// Parse the contract spelling.
    ///
    /// # Errors
    /// [`WalletError::InvalidInput`] on an unknown basis.
    pub fn parse(value: &str) -> Result<Self, WalletError> {
        value.parse()
    }
}

impl FromStr for TimeBasis {
    type Err = WalletError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "booking" => Ok(Self::Booking),
            "service" => Ok(Self::Service),
            "payment" => Ok(Self::Payment),
            other => Err(WalletError::InvalidInput(format!(
                "timeBasis must be booking|service|payment, not {other:?}"
            ))),
        }
    }
}

/// The filter a facts read takes. `organization_id` scopes the business read
/// to one customer; the customer read always carries its own. `model`/`channel`
/// narrow the usage-derived categories — the money categories ignore them,
/// since a payment has no model.
#[derive(Debug, Default, Clone)]
pub struct FactFilter {
    /// The customer organization to scope to — `None` on the business read
    /// spans every customer.
    pub organization_id: Option<Uuid>,
    /// Narrow the usage-derived categories to one model.
    pub model: Option<String>,
    /// Narrow the usage-derived categories to one channel.
    pub channel: Option<String>,
    /// Period start, inclusive — under `time_basis`'s column.
    pub from: Option<Date>,
    /// Period end, inclusive — under `time_basis`'s column.
    pub to: Option<Date>,
    /// Which clock the period counts under.
    pub time_basis: TimeBasis,
}

// ---------------------------------------------------------------------------
// Cost coverage
// ---------------------------------------------------------------------------

/// One `cost_basis` bucket in the coverage report: how many settled turns and
/// how much upstream cost the bucket carries. The counts never blur "cost not
/// tracked" from "cost was zero" — `missing`/NULL basis rows count their
/// turns but contribute no cost figure, while a `real_attempt` row with
/// `upstream_cost_minor = 0` is a failed-but-billed attempt: the platform's
/// real zero, kept distinct from an untracked one.
#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostBasisCount {
    /// Settled turns in the bucket.
    pub turns: i64,
    /// Σ `upstream_cost_minor` over the bucket — the NULLs ignored as
    /// untracked, so a `missing` bucket's `turns` with no `costMinor` is the
    /// coverage gap, not a free lunch.
    pub cost_minor: i64,
    /// Turns whose cost is untracked (`upstream_cost_minor IS NULL`).
    pub untracked_turns: i64,
}

/// The failed-attempt count the coverage report separates: turns that
/// settled `errored` or `refused` — a refused turn paid nothing but still
/// names the attempt, an errored one may have burned an upstream call.
#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedAttempts {
    /// `errored` turns — upstream error, unreachable, swept.
    pub errored: i64,
    /// `refused` turns — admission refused before a hold.
    pub refused: i64,
}

/// One provider/day grain of the coverage report — the
/// `(service_date, channel, model)` triple the upstream-bill reconciliation
/// reads. `service_date` is `settled_at::date`, the day the provider's own
/// cost roll-up reports under.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostCoverageDay {
    /// The UTC day the turns settled.
    pub service_date: Date,
    /// The channel the turns ran on.
    pub channel: String,
    /// The model billed.
    pub model: String,
    /// Settled turns on the grain.
    pub turns: i64,
    /// Σ `upstream_cost_minor` on the grain.
    pub cost_minor: i64,
    /// Cache-reuse turns (`upstream_attempts = 0` — no upstream call ran).
    pub cache_reuse_turns: i64,
    /// Turns whose cost is untracked.
    pub untracked_turns: i64,
}

/// The cost-coverage report: the `cost_basis` breakdown at summary grain and
/// the provider/day detail the upstream-bill reconciliation reads.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostCoverage {
    /// Turns whose cost a real upstream attempt reported.
    pub real_attempt: CostBasisCount,
    /// Cache-reuse turns — no upstream call ran, cost already paid by the
    /// turn that produced the answer.
    pub cache_reuse: CostBasisCount,
    /// Locally-estimated cost (degraded policy, client-cancel estimate).
    pub estimate: CostBasisCount,
    /// Untracked cost: no `upstream` block, a swept turn, pre-column history.
    /// The coverage gap — `turns` names how much usage cannot be reconciled
    /// to a provider bill.
    pub missing: CostBasisCount,
    /// Turns reconciled against the provider's own bill (reserved).
    pub provider_reconciled: CostBasisCount,
    /// Failed attempts — errored and refused turns, separated so a
    /// failed-but-billed upstream charge (an `errored` turn's real cost) is
    /// never mistaken for a clean zero.
    pub failed_attempts: FailedAttempts,
    /// The provider/day detail.
    pub by_provider_day: Vec<CostCoverageDay>,
}

// ---------------------------------------------------------------------------
// Query plumbing
// ---------------------------------------------------------------------------

/// One bound parameter, in placeholder order. sqlx binds positionally, so the
/// WHERE builder appends `$N` and pushes the matching value here in lockstep.
enum Bind<'a> {
    Text(&'a str),
    Date(Date),
    Uuid(Uuid),
    Tenants(&'a [String]),
}

/// Placeholder counter: each `clause` rewrites its `?` to `$N` and bumps the
/// counter, so a WHERE built left-to-right binds in the order `bind_all`
/// replays.
struct Where {
    next: usize,
}

impl Where {
    fn new() -> Self {
        Self { next: 1 }
    }
    fn clause(&mut self, template: &str) -> String {
        let clause = template.replace('?', &format!("${}", self.next));
        self.next += 1;
        clause
    }
}

/// A usage row re-priced to catalog rate: the token dims and the
/// `(channel, model, price_version)` that priced it.
struct RatedRow {
    channel: String,
    model: String,
    price_version: i64,
    charged_minor: i64,
    usage: UsageRecord,
}

fn rated_row(row: &PgRow) -> Result<RatedRow, WalletError> {
    Ok(RatedRow {
        channel: row.try_get("channel")?,
        model: row.try_get("model")?,
        price_version: row.try_get("price_version")?,
        charged_minor: row.try_get("charged_minor")?,
        usage: UsageRecord {
            input_tokens: row.try_get("input_tokens")?,
            output_tokens: row.try_get("output_tokens")?,
            cached_tokens: row.try_get("cached_tokens")?,
            cache_write_5m_tokens: row.try_get("cache_write_5m_tokens")?,
            cache_write_1h_tokens: row.try_get("cache_write_1h_tokens")?,
            reasoning_tokens: row.try_get("reasoning_tokens")?,
            tool_calls: row.try_get("tool_calls")?,
            image_input_tokens: row.try_get("image_input_tokens")?,
            audio_input_tokens: row.try_get("audio_input_tokens")?,
            video_input_tokens: row.try_get("video_input_tokens")?,
            image_output_tokens: row.try_get("image_output_tokens")?,
            audio_output_tokens: row.try_get("audio_output_tokens")?,
            service_tier: row.try_get("service_tier")?,
            event_type: row.try_get("event_type")?,
            end_user: None,
            tags: Default::default(),
            usage_details: None,
        },
    })
}

/// The usage SELECT column list shared by the booking and service reads.
const RATED_COLS: &str = "u.channel, u.model, u.price_version, u.charged_minor, \
     u.input_tokens, u.output_tokens, u.cached_tokens, u.cache_write_5m_tokens, \
     u.cache_write_1h_tokens, u.reasoning_tokens, u.tool_calls, u.image_input_tokens, \
     u.audio_input_tokens, u.video_input_tokens, u.image_output_tokens, \
     u.audio_output_tokens, u.service_tier, u.event_type";

// The binds a built WHERE produced, replayed onto the query in order.
fn bind_all<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    binds: &[Bind<'q>],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    for bind in binds {
        query = match bind {
            Bind::Text(value) => query.bind(*value),
            Bind::Date(value) => query.bind(*value),
            Bind::Uuid(value) => query.bind(*value),
            Bind::Tenants(value) => query.bind(*value),
        };
    }
    query
}

// The same replay onto a scalar query — `QueryScalar` is its own type, so the
// one-bind-at-a-time replay is spelled separately.
fn bind_scalar<'q>(
    mut query: sqlx::query::QueryScalar<'q, sqlx::Postgres, i64, sqlx::postgres::PgArguments>,
    binds: &[Bind<'q>],
) -> sqlx::query::QueryScalar<'q, sqlx::Postgres, i64, sqlx::postgres::PgArguments> {
    for bind in binds {
        query = match bind {
            Bind::Text(value) => query.bind(*value),
            Bind::Date(value) => query.bind(*value),
            Bind::Uuid(value) => query.bind(*value),
            Bind::Tenants(value) => query.bind(*value),
        };
    }
    query
}

impl Db {
    /// The tenant ids an `organization_id` filter resolves to — `usage_records`
    /// keys on the ledger tenant id, not the organization uuid. `None` spans
    /// every tenant (the business read).
    async fn fact_tenants(
        &self,
        organization_id: Option<Uuid>,
    ) -> Result<Vec<String>, WalletError> {
        match organization_id {
            Some(id) => Ok(sqlx::query_scalar(
                "SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1",
            )
            .bind(id)
            .fetch_all(self.pool())
            .await?),
            None => Ok(
                sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations")
                    .fetch_all(self.pool())
                    .await?,
            ),
        }
    }

    /// The usage WHERE fragment under the booking/payment clock — the period
    /// reads `u.settled_at::date` (for a metered charge, the settle instant is
    /// the payment instant; the money tables periodize on their own stamps).
    fn usage_clauses<'a>(
        filter: &'a FactFilter,
        tenants: &'a [String],
        w: &mut Where,
        binds: &mut Vec<Bind<'a>>,
    ) -> String {
        let mut sql = String::new();
        if filter.organization_id.is_some() {
            sql.push_str(&w.clause(" AND u.tenant_id = ANY(?)"));
            binds.push(Bind::Tenants(tenants));
        }
        if let Some(model) = &filter.model {
            sql.push_str(&w.clause(" AND u.model = ?"));
            binds.push(Bind::Text(model));
        }
        if let Some(channel) = &filter.channel {
            sql.push_str(&w.clause(" AND u.channel = ?"));
            binds.push(Bind::Text(channel));
        }
        if let Some(from) = filter.from {
            sql.push_str(&w.clause(" AND u.settled_at::date >= ?"));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql.push_str(&w.clause(" AND u.settled_at::date <= ?"));
            binds.push(Bind::Date(to));
        }
        sql
    }

    /// `usage`/`rated` under the booking or payment clock — one pass over
    /// `usage_records`, no ledger join.
    async fn usage_figures_booking(
        &self,
        filter: &FactFilter,
        tenants: &[String],
    ) -> Result<(i64, i64), WalletError> {
        let mut w = Where::new();
        let mut binds: Vec<Bind<'_>> = Vec::new();
        let clauses = Self::usage_clauses(filter, tenants, &mut w, &mut binds);
        let sql = format!(
            "SELECT {RATED_COLS} FROM oxsum.usage_records u \
             WHERE u.status = 'settled'{clauses}"
        );
        let query = bind_all(sqlx::query(&sql), &binds);
        let rows = query.fetch_all(self.pool()).await?;
        let mut usage_minor = 0_i64;
        let mut rated: Vec<RatedRow> = Vec::with_capacity(rows.len());
        for row in &rows {
            let r = rated_row(row)?;
            usage_minor += r.charged_minor;
            rated.push(r);
        }
        let rated_minor = self.rate_rows(&rated).await?;
        Ok((usage_minor, rated_minor))
    }

    /// `usage`/`rated` under the service clock — the period reads the
    /// settlement entry's `booking_date`, which lives in the per-tenant
    /// ledger schema, so each tenant's rows fetch under its own
    /// schema-qualified join and sum.
    async fn usage_figures_service(
        &self,
        filter: &FactFilter,
        tenants: &[String],
    ) -> Result<(i64, i64), WalletError> {
        let mut usage_minor = 0_i64;
        let mut all_rated: Vec<RatedRow> = Vec::new();
        for tenant in tenants {
            let schema = format!("ledger_{}", tenant.replace('"', "\"\""));
            let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                .bind(format!("\"{schema}\".entries"))
                .fetch_one(self.pool())
                .await?;
            if !exists {
                continue;
            }
            let mut w = Where::new();
            let mut binds: Vec<Bind<'_>> = Vec::new();
            // tenant_id is always bound first on this per-tenant read.
            let mut sql = format!(
                "SELECT {RATED_COLS} FROM oxsum.usage_records u \
                 JOIN \"{schema}\".entries e ON e.entry_id = u.entry_id \
                 WHERE u.tenant_id = {} AND u.status = 'settled'",
                w.clause("?")
            );
            binds.push(Bind::Text(tenant));
            if let Some(model) = &filter.model {
                sql.push_str(&w.clause(" AND u.model = ?"));
                binds.push(Bind::Text(model));
            }
            if let Some(channel) = &filter.channel {
                sql.push_str(&w.clause(" AND u.channel = ?"));
                binds.push(Bind::Text(channel));
            }
            if let Some(from) = filter.from {
                sql.push_str(&w.clause(" AND e.booking_date >= ?"));
                binds.push(Bind::Date(from));
            }
            if let Some(to) = filter.to {
                sql.push_str(&w.clause(" AND e.booking_date <= ?"));
                binds.push(Bind::Date(to));
            }
            let query = bind_all(sqlx::query(&sql), &binds);
            let rows = query.fetch_all(self.pool()).await?;
            for row in &rows {
                let r = rated_row(row)?;
                usage_minor += r.charged_minor;
                all_rated.push(r);
            }
        }
        let rated_minor = self.rate_rows(&all_rated).await?;
        Ok((usage_minor, rated_minor))
    }

    /// The period's usage figures: `usage` and `rated` read off the same
    /// settled rows so the two can never disagree on which turns count.
    async fn usage_figures(
        &self,
        filter: &FactFilter,
        tenants: &[String],
    ) -> Result<(i64, i64), WalletError> {
        match filter.time_basis {
            TimeBasis::Service => self.usage_figures_service(filter, tenants).await,
            _ => self.usage_figures_booking(filter, tenants).await,
        }
    }

    /// Re-prices the rows at their recorded catalog versions: one map load of
    /// the distinct `(channel, model, version)` prices, then `itemize` per
    /// row — the discount-free total is what `rated` sums.
    async fn rate_rows(&self, rows: &[RatedRow]) -> Result<i64, WalletError> {
        let mut wanted: BTreeSet<(String, String, i64)> = BTreeSet::new();
        for row in rows {
            wanted.insert((row.channel.clone(), row.model.clone(), row.price_version));
        }
        let mut prices: HashMap<(String, String, i64), crate::Price> = HashMap::new();
        for (channel, model, version) in wanted {
            if let Some(model_price) =
                crate::channels::price_row(self.pool(), &channel, &model, Some(version)).await?
            {
                prices.insert((channel, model, version), model_price.price);
            }
        }
        let mut rated = 0_i64;
        for row in rows {
            let key = (row.channel.clone(), row.model.clone(), row.price_version);
            rated += match prices.get(&key) {
                // `itemize` re-prices the recorded usage at the catalog rate —
                // no discount — under the version that billed it. A row whose
                // usage fails validation contributes its charged figure: the
                // bill already landed, so charged is the honest stand-in.
                Some(price) => price
                    .itemize(&row.usage, true)
                    .and_then(|charge| charge.total_minor())
                    .unwrap_or(row.charged_minor),
                None => row.charged_minor,
            };
        }
        Ok(rated)
    }

    /// Σ `subscription_draws.settled_minor` plus `plan_periods.consumed_minor`
    /// for the period — the entitlement figure: what the plan covered.
    async fn entitlement_minor(&self, filter: &FactFilter) -> Result<i64, WalletError> {
        let mut total = 0_i64;
        let mut w = Where::new();
        let mut binds: Vec<Bind<'_>> = Vec::new();
        let mut sql = String::from(
            "SELECT COALESCE(sum(d.settled_minor),0)::bigint \
             FROM oxsum.subscription_draws d \
             JOIN oxsum.org_subscriptions s ON s.subscription_id = d.subscription_id \
             WHERE d.settled_minor IS NOT NULL",
        );
        if let Some(org) = filter.organization_id {
            sql.push_str(&w.clause(" AND s.organization_id = ?"));
            binds.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql.push_str(&w.clause(" AND d.settled_at::date >= ?"));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql.push_str(&w.clause(" AND d.settled_at::date <= ?"));
            binds.push(Bind::Date(to));
        }
        let query = bind_scalar(sqlx::query_scalar::<_, i64>(&sql), &binds);
        total += query.fetch_one(self.pool()).await?;

        let mut w2 = Where::new();
        let mut binds2: Vec<Bind<'_>> = Vec::new();
        let mut sql2 = String::from(
            "SELECT COALESCE(sum(pp.consumed_minor),0)::bigint \
             FROM oxsum.plan_periods pp \
             JOIN oxsum.customer_plans cp ON cp.customer_plan_id = pp.customer_plan_id \
             WHERE true",
        );
        if let Some(org) = filter.organization_id {
            sql2.push_str(&w2.clause(" AND cp.organization_id = ?"));
            binds2.push(Bind::Uuid(org));
        }
        // A plan period overlapping the window contributes its consumed share —
        // the period boundaries are the entitlement's own grain.
        if let Some(from) = filter.from {
            sql2.push_str(&w2.clause(" AND pp.period_end::date >= ?"));
            binds2.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql2.push_str(&w2.clause(" AND pp.period_start::date <= ?"));
            binds2.push(Bind::Date(to));
        }
        let query2 = bind_scalar(sqlx::query_scalar::<_, i64>(&sql2), &binds2);
        total += query2.fetch_one(self.pool()).await?;
        Ok(total)
    }

    /// Cash in over the period: paid/fulfilled `payment_orders` plus credited
    /// `deposits`. `payment` basis reads `paid_at`/`updated_at`; the other
    /// bases read `created_at` — the cash's clock is its own.
    async fn funding_minor(&self, filter: &FactFilter) -> Result<i64, WalletError> {
        let order_col = match filter.time_basis {
            TimeBasis::Payment => "COALESCE(o.paid_at, o.updated_at)::date",
            _ => "o.created_at::date",
        };
        let mut w = Where::new();
        let mut binds: Vec<Bind<'_>> = Vec::new();
        let mut sql = "SELECT COALESCE(sum(o.amount_minor),0)::bigint \
             FROM oxsum.payment_orders o WHERE o.status IN ('paid','fulfilled')"
            .to_string();
        if let Some(org) = filter.organization_id {
            sql.push_str(&w.clause(" AND o.organization_id = ?"));
            binds.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql.push_str(&w.clause(&format!(" AND {order_col} >= ?")));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql.push_str(&w.clause(&format!(" AND {order_col} <= ?")));
            binds.push(Bind::Date(to));
        }
        let query = bind_scalar(sqlx::query_scalar::<_, i64>(&sql), &binds);
        let mut total = query.fetch_one(self.pool()).await?;

        let dep_col = match filter.time_basis {
            TimeBasis::Payment => "d.updated_at::date",
            _ => "d.created_at::date",
        };
        let mut w2 = Where::new();
        let mut binds2: Vec<Bind<'_>> = Vec::new();
        let mut sql2 = "SELECT COALESCE(sum(d.amount_minor),0)::bigint \
             FROM oxsum.deposits d WHERE d.status = 'credited'"
            .to_string();
        if let Some(org) = filter.organization_id {
            sql2.push_str(&w2.clause(" AND d.organization_id = ?"));
            binds2.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql2.push_str(&w2.clause(&format!(" AND {dep_col} >= ?")));
            binds2.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql2.push_str(&w2.clause(&format!(" AND {dep_col} <= ?")));
            binds2.push(Bind::Date(to));
        }
        let query2 = bind_scalar(sqlx::query_scalar::<_, i64>(&sql2), &binds2);
        total += query2.fetch_one(self.pool()).await?;
        Ok(total)
    }

    /// Legacy v1 billed/earned and collected: the journal figure and the
    /// part of it already settled, over the period the journal posted in.
    async fn journal_figures(&self, filter: &FactFilter) -> Result<(i64, i64), WalletError> {
        let mut w = Where::new();
        let mut binds: Vec<Bind<'_>> = Vec::new();
        let mut sql = String::from(
            "SELECT COALESCE(sum(j.total_minor),0)::bigint AS earned, \
                    COALESCE(sum(j.settled_minor),0)::bigint AS collected \
             FROM oxsum.invoice_journals j WHERE j.state IN ('posted','paid') AND j.accounting_policy_version=1",
        );
        if let Some(org) = filter.organization_id {
            sql.push_str(&w.clause(" AND j.organization_id = ?"));
            binds.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql.push_str(&w.clause(" AND j.posted_at::date >= ?"));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql.push_str(&w.clause(" AND j.posted_at::date <= ?"));
            binds.push(Bind::Date(to));
        }
        let query = bind_all(sqlx::query(&sql), &binds);
        let row = query.fetch_one(self.pool()).await?;
        Ok((row.try_get("earned")?, row.try_get("collected")?))
    }

    /// Exact cash returns include late external facts even when their old
    /// terminal state remains failed. Historical recovery retains the original
    /// Cash date; its noncash correction cannot create another cash return.
    /// Other historical rows need a real sequenced Cash credit.
    async fn refund_minor(&self, filter: &FactFilter) -> Result<i64, WalletError> {
        let mut w = Where::new();
        let mut binds = Vec::new();
        let mut sql = String::from(
            "SELECT COALESCE(sum(e.cash_minor),0)::bigint FROM (SELECT organization_id,cash_minor,booked_at::date AS cash_on FROM oxsum.plan_refund_effects UNION ALL SELECT organization_id,cash_minor,cash_booking_date AS cash_on FROM oxsum.nonplan_refund_effects UNION ALL SELECT organization_id,cash_minor,booked_on AS cash_on FROM oxsum.statement_payment_effects WHERE kind='refund') e WHERE true",
        );
        if let Some(org) = filter.organization_id {
            sql.push_str(&w.clause(" AND e.organization_id=?"));
            binds.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql.push_str(&w.clause(" AND e.cash_on>=?"));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql.push_str(&w.clause(" AND e.cash_on<=?"));
            binds.push(Bind::Date(to));
        }
        let mut total: i64 = bind_scalar(sqlx::query_scalar(&sql), &binds)
            .fetch_one(self.pool())
            .await?;
        for tenant in self.fact_tenants(filter.organization_id).await? {
            let schema = format!("ledger_{}", tenant.replace('"', "\"\""));
            let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                .bind(format!("\"{schema}\".entries"))
                .fetch_one(self.pool())
                .await?;
            if !exists {
                continue;
            }
            let mut w = Where::new();
            let mut binds = Vec::new();
            let mut sql = format!(
                "SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint FROM \"{schema}\".entries e JOIN \"{schema}\".postings p USING(entry_id) JOIN \"{schema}\".accounts a USING(account_index) WHERE e.log_index IS NOT NULL AND a.path='Assets:Cash' AND p.layer='settled' AND e.entry_id IN (SELECT n.entry_id FROM oxsum.credit_notes n JOIN oxsum.refunds r USING(refund_id) JOIN oxsum.organizations o ON o.organization_id=r.organization_id WHERE o.tenant_id=$1 AND n.entry_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM oxsum.plan_refund_effects f WHERE f.refund_id=r.refund_id) AND NOT EXISTS(SELECT 1 FROM oxsum.nonplan_refund_effects f WHERE f.refund_id=r.refund_id) AND NOT EXISTS(SELECT 1 FROM oxsum.statement_payment_effects f WHERE f.kind='refund' AND f.source_ref=r.refund_id))"
            );
            binds.push(Bind::Text(&tenant));
            w.next = 2;
            if let Some(from) = filter.from {
                sql.push_str(&w.clause(" AND e.booking_date>=?"));
                binds.push(Bind::Date(from));
            }
            if let Some(to) = filter.to {
                sql.push_str(&w.clause(" AND e.booking_date<=?"));
                binds.push(Bind::Date(to));
            }
            let cash: i64 = bind_scalar(sqlx::query_scalar(&sql), &binds)
                .fetch_one(self.pool())
                .await?;
            total = total
                .checked_add(cash)
                .ok_or_else(|| error::invalid("refund total overflows"))?;
        }
        Ok(total)
    }

    async fn discount_minor(&self, filter: &FactFilter) -> Result<i64, WalletError> {
        let mut w = Where::new();
        let mut binds = Vec::new();
        let mut sql = String::from(
            "SELECT -COALESCE(SUM(l.amount_minor),0)::bigint FROM oxsum.invoice_journal_lines l JOIN oxsum.invoice_journals j USING(journal_id) WHERE j.state<>'void' AND l.kind IN ('discount','promo_credit') AND NOT EXISTS(SELECT 1 FROM oxsum.credit_notes n WHERE n.credit_note_id=l.source_ref AND n.refund_id IS NOT NULL)",
        );
        if let Some(org) = filter.organization_id {
            sql.push_str(&w.clause(" AND j.organization_id=?"));
            binds.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql.push_str(&w.clause(" AND l.created_at::date>=?"));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql.push_str(&w.clause(" AND l.created_at::date<=?"));
            binds.push(Bind::Date(to));
        }
        Ok(bind_scalar(sqlx::query_scalar(&sql), &binds)
            .fetch_one(self.pool())
            .await?)
    }

    async fn daily_plan_figures(
        &self,
        filter: &FactFilter,
    ) -> Result<crate::plan_recognition::Effect, WalletError> {
        let clock = match filter.time_basis {
            TimeBasis::Booking => "e.booked_on",
            TimeBasis::Service => "e.service_day",
            TimeBasis::Payment => "e.payment_on",
        };
        let mut w = Where::new();
        let mut binds = Vec::new();
        let mut sql = String::from(
            "SELECT COALESCE(SUM(e.billed_minor),0)::bigint AS billed,COALESCE(SUM(e.earned_minor),0)::bigint AS earned,COALESCE(SUM(e.deferred_minor),0)::bigint AS deferred,COALESCE(SUM(e.collected_minor),0)::bigint AS collected,COALESCE(SUM(e.receivable_minor),0)::bigint AS receivable FROM oxsum.plan_accounting_effects e JOIN oxsum.plan_accounting_periods a USING(plan_period_id) WHERE true",
        );
        if let Some(org) = filter.organization_id {
            sql.push_str(&w.clause(" AND a.organization_id=?"));
            binds.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql.push_str(&w.clause(&format!(" AND {clock}>=?")));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql.push_str(&w.clause(&format!(" AND {clock}<=?")));
            binds.push(Bind::Date(to));
        }
        let row = bind_all(sqlx::query(&sql), &binds)
            .fetch_one(self.pool())
            .await?;
        Ok(crate::plan_recognition::Effect {
            billed: row.try_get("billed")?,
            earned: row.try_get("earned")?,
            deferred: row.try_get("deferred")?,
            collected: row.try_get("collected")?,
            receivable: row.try_get("receivable")?,
            ..crate::plan_recognition::Effect::default()
        })
    }

    /// The open receivable: `issued`/`past_due` plan statements'
    /// `total − paid` plus `posted` journals' `total − settled` where no live
    /// statement already surfaces the same receivable. Mixed-policy dispute
    /// lines follow their original journal source, bounded by remaining source
    /// debt and the receiving statement's actual outstanding amount.
    async fn receivable_minor(&self, filter: &FactFilter) -> Result<i64, WalletError> {
        let mut w = Where::new();
        let mut binds: Vec<Bind<'_>> = Vec::new();
        let mut sql = String::from(
            "SELECT COALESCE(SUM(CASE WHEN COALESCE(owner.accounting_policy_version,1)=1 THEN \
                GREATEST(s.total_minor-s.paid_minor,0)-LEAST(GREATEST(s.total_minor-s.paid_minor,0),COALESCE(daily.amount,0)) \
              ELSE LEAST(GREATEST(s.total_minor-s.paid_minor,0),COALESCE(legacy.amount,0)) END),0)::bigint \
             FROM oxsum.plan_statements s LEFT JOIN oxsum.invoice_journals owner ON owner.journal_id=s.journal_id \
             LEFT JOIN LATERAL (SELECT SUM(amount)::bigint AS amount FROM ( \
                SELECT LEAST(SUM(l.amount_minor),SUM(e.receivable_minor),MAX(GREATEST(j.total_minor-j.settled_minor,0))) AS amount \
                FROM oxsum.plan_statement_lines l JOIN oxsum.plan_accounting_effects e ON e.source_ref=l.source_ref AND e.kind='dispute' \
                JOIN oxsum.plan_accounting_periods a ON a.plan_period_id=e.plan_period_id \
                JOIN oxsum.invoice_journals j ON j.journal_id=a.journal_id \
                WHERE l.statement_id=s.statement_id AND l.kind='dispute_chargeback' AND j.state='posted' \
                GROUP BY j.journal_id) sources) daily ON true \
             LEFT JOIN LATERAL (SELECT SUM(amount)::bigint AS amount FROM ( \
                SELECT LEAST(SUM(l.amount_minor),MAX(GREATEST(j.total_minor-j.settled_minor,0))) AS amount \
                FROM oxsum.plan_statement_lines l JOIN oxsum.disputes d ON d.dispute_id=l.source_ref \
                JOIN oxsum.plan_periods p ON p.authorization_kind='payment_order' AND p.authorization_ref=d.order_id \
                JOIN oxsum.invoice_journals j ON j.plan_period_id=p.plan_period_id AND j.accounting_policy_version=1 AND j.state='posted' \
                WHERE l.statement_id=s.statement_id AND l.kind='dispute_chargeback' GROUP BY j.journal_id) sources) legacy ON true \
             WHERE s.state IN ('issued','past_due')",
        );
        if let Some(org) = filter.organization_id {
            sql.push_str(&w.clause(" AND s.organization_id = ?"));
            binds.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql.push_str(&w.clause(" AND s.issued_at::date >= ?"));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql.push_str(&w.clause(" AND s.issued_at::date <= ?"));
            binds.push(Bind::Date(to));
        }
        let query = bind_scalar(sqlx::query_scalar::<_, i64>(&sql), &binds);
        let mut total = query.fetch_one(self.pool()).await?;

        let mut w2 = Where::new();
        let mut binds2: Vec<Bind<'_>> = Vec::new();
        let mut sql2 = String::from(
            "SELECT COALESCE(sum(j.total_minor - j.settled_minor),0)::bigint \
             FROM oxsum.invoice_journals j \
             WHERE j.state = 'posted' AND j.accounting_policy_version=1 \
               AND NOT EXISTS (SELECT 1 FROM oxsum.plan_statements s \
                               WHERE s.journal_id = j.journal_id AND s.state <> 'void')",
        );
        if let Some(org) = filter.organization_id {
            sql2.push_str(&w2.clause(" AND j.organization_id = ?"));
            binds2.push(Bind::Uuid(org));
        }
        if let Some(from) = filter.from {
            sql2.push_str(&w2.clause(" AND j.posted_at::date >= ?"));
            binds2.push(Bind::Date(from));
        }
        if let Some(to) = filter.to {
            sql2.push_str(&w2.clause(" AND j.posted_at::date <= ?"));
            binds2.push(Bind::Date(to));
        }
        let query2 = bind_scalar(sqlx::query_scalar::<_, i64>(&sql2), &binds2);
        total += query2.fetch_one(self.pool()).await?;
        Ok(total)
    }

    /// The financial figures for one filter — the read model both the
    /// staff `GET /api/v1/business/financial-facts` and the customer's
    /// `GET /api/v1/org/usage-report` project from. The customer's read is the
    /// same call with its own `organization_id` — a projection, never a
    /// second total.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn financial_facts(
        &self,
        filter: &FactFilter,
    ) -> Result<FinancialFacts, WalletError> {
        let tenants = self.fact_tenants(filter.organization_id).await?;
        let (usage_minor, rated_minor) = self.usage_figures(filter, &tenants).await?;
        let (legacy_billed, legacy_collected) = self.journal_figures(filter).await?;
        let daily = self.daily_plan_figures(filter).await?;
        let earned_minor = legacy_billed
            .checked_add(daily.earned)
            .ok_or_else(|| error::invalid("earned total overflows"))?;
        let billed_minor = legacy_billed
            .checked_add(daily.billed)
            .ok_or_else(|| error::invalid("billed total overflows"))?;
        let collected_minor = legacy_collected
            .checked_add(daily.collected)
            .ok_or_else(|| error::invalid("collected total overflows"))?;
        let deferred_minor = daily.deferred;
        let discount_minor = self.discount_minor(filter).await?;
        let (entitlement_minor, funding_minor, refund_minor, receivable_minor) = tokio::try_join!(
            self.entitlement_minor(filter),
            self.funding_minor(filter),
            self.refund_minor(filter),
            self.receivable_minor(filter)
        )?;
        let receivable_minor = receivable_minor
            .checked_add(daily.receivable)
            .ok_or_else(|| error::invalid("receivable total overflows"))?;
        Ok(FinancialFacts {
            usage_minor,
            rated_minor,
            entitlement_minor,
            funding_minor,
            earned_minor,
            billed_minor,
            deferred_minor,
            discount_minor,
            collected_minor,
            refund_minor,
            receivable_minor,
        })
    }

    /// The customer-scoped read: `GET /api/v1/org/usage-report` projects the
    /// same `financial_facts` under the caller's own organization, so the
    /// customer's total and the business's sum for that organization are the
    /// same figure by construction.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn org_usage_report(
        &self,
        organization_id: Uuid,
        from: Option<Date>,
        to: Option<Date>,
        time_basis: TimeBasis,
    ) -> Result<FinancialFacts, WalletError> {
        self.financial_facts(&FactFilter {
            organization_id: Some(organization_id),
            model: None,
            channel: None,
            from,
            to,
            time_basis,
        })
        .await
    }

    /// The business-scoped read: `GET /api/v1/business/financial-facts`,
    /// `ReportsRead` — a personal session, never a machine credential.
    ///
    /// # Errors
    ///
    /// Storage and authorization failures surface as [`WalletError`].
    pub async fn business_financial_facts(
        &self,
        token: &str,
        filter: &FactFilter,
    ) -> Result<FinancialFacts, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        tx.commit().await?;
        self.financial_facts(filter).await
    }

    /// The cost-coverage report: `cost_basis` counts and cost at summary
    /// grain plus the provider/day detail, scoped like the facts read.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn cost_coverage(
        &self,
        organization_id: Option<Uuid>,
        from: Option<Date>,
        to: Option<Date>,
    ) -> Result<CostCoverage, WalletError> {
        let tenants = self.fact_tenants(organization_id).await?;
        let mut coverage = CostCoverage::default();
        // Summary grain: one row per (cost_basis, status) bucket. NULL
        // `cost_basis` folds into `missing` — the same untracked the column's
        // NULL means.
        let mut w = Where::new();
        let mut binds: Vec<Bind<'_>> = Vec::new();
        let mut sql = String::from(
            "SELECT COALESCE(u.cost_basis,'missing') AS basis, u.status, \
                    count(*) AS turns, \
                    COALESCE(sum(u.upstream_cost_minor),0)::bigint AS cost_minor, \
                    count(*) FILTER (WHERE u.upstream_cost_minor IS NULL) AS untracked \
             FROM oxsum.usage_records u WHERE true",
        );
        if organization_id.is_some() {
            sql.push_str(&w.clause(" AND u.tenant_id = ANY(?)"));
            binds.push(Bind::Tenants(&tenants));
        }
        if let Some(from) = from {
            sql.push_str(&w.clause(" AND u.settled_at::date >= ?"));
            binds.push(Bind::Date(from));
        }
        if let Some(to) = to {
            sql.push_str(&w.clause(" AND u.settled_at::date <= ?"));
            binds.push(Bind::Date(to));
        }
        sql.push_str(" GROUP BY basis, u.status");
        let query = bind_all(sqlx::query(&sql), &binds);
        let rows = query.fetch_all(self.pool()).await?;
        for row in &rows {
            let basis: String = row.try_get("basis")?;
            let status: String = row.try_get("status")?;
            let turns: i64 = row.try_get("turns")?;
            let cost: i64 = row.try_get("cost_minor")?;
            let untracked: i64 = row.try_get("untracked")?;
            if status == "errored" {
                coverage.failed_attempts.errored += turns;
            } else if status == "refused" {
                coverage.failed_attempts.refused += turns;
            }
            let bucket = match basis.as_str() {
                "real_attempt" => &mut coverage.real_attempt,
                "cache_reuse" => &mut coverage.cache_reuse,
                "estimate" => &mut coverage.estimate,
                "provider_reconciled" => &mut coverage.provider_reconciled,
                _ => &mut coverage.missing,
            };
            bucket.turns += turns;
            bucket.cost_minor += cost;
            bucket.untracked_turns += untracked;
        }
        // Provider/day detail: (settled_at::date, channel, model).
        let mut w2 = Where::new();
        let mut binds2: Vec<Bind<'_>> = Vec::new();
        let mut sql2 = String::from(
            "SELECT u.settled_at::date AS service_date, u.channel, u.model, \
                    count(*) AS turns, \
                    COALESCE(sum(u.upstream_cost_minor),0)::bigint AS cost_minor, \
                    count(*) FILTER (WHERE u.upstream_attempts = 0) AS cache_reuse, \
                    count(*) FILTER (WHERE u.upstream_cost_minor IS NULL) AS untracked \
             FROM oxsum.usage_records u WHERE u.status = 'settled'",
        );
        if organization_id.is_some() {
            sql2.push_str(&w2.clause(" AND u.tenant_id = ANY(?)"));
            binds2.push(Bind::Tenants(&tenants));
        }
        if let Some(from) = from {
            sql2.push_str(&w2.clause(" AND u.settled_at::date >= ?"));
            binds2.push(Bind::Date(from));
        }
        if let Some(to) = to {
            sql2.push_str(&w2.clause(" AND u.settled_at::date <= ?"));
            binds2.push(Bind::Date(to));
        }
        sql2.push_str(
            " GROUP BY service_date, u.channel, u.model ORDER BY service_date, u.channel, u.model",
        );
        let query2 = bind_all(sqlx::query(&sql2), &binds2);
        let days = query2.fetch_all(self.pool()).await?;
        for row in &days {
            coverage.by_provider_day.push(CostCoverageDay {
                service_date: row.try_get("service_date")?,
                channel: row.try_get("channel")?,
                model: row.try_get("model")?,
                turns: row.try_get("turns")?,
                cost_minor: row.try_get("cost_minor")?,
                cache_reuse_turns: row.try_get("cache_reuse")?,
                untracked_turns: row.try_get("untracked")?,
            });
        }
        Ok(coverage)
    }

    /// The staff cost-coverage read — `ReportsRead`.
    ///
    /// # Errors
    ///
    /// Storage and authorization failures surface as [`WalletError`].
    pub async fn business_cost_coverage(
        &self,
        token: &str,
        organization_id: Option<Uuid>,
        from: Option<Date>,
        to: Option<Date>,
    ) -> Result<CostCoverage, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        tx.commit().await?;
        self.cost_coverage(organization_id, from, to).await
    }
}

// ---------------------------------------------------------------------------
// Reconciliation: runs, exceptions, repairs
// ---------------------------------------------------------------------------

/// One reconciliation run, as `GET /api/v1/business/reconciliation-runs`
/// lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationRun {
    /// Local run identifier.
    pub run_id: Uuid,
    /// `daily` | `period_close` | `manual`.
    pub kind: String,
    /// First day covered by the run, inclusive.
    #[serde(with = "reconciliation_day")]
    pub period_start: Date,
    /// Last day covered by the run, inclusive.
    #[serde(with = "reconciliation_day")]
    pub period_end: Date,
    /// `running` | `matched` | `exceptions` | `failed`.
    pub state: String,
    /// The requested original-source clock.
    pub time_basis: TimeBasis,
    /// Latest immutable capture; older attempts remain independently readable.
    pub latest_snapshot_id: Option<Uuid>,
    /// Explicit comparison coverage; version zero marks old projection-only reports.
    pub coverage: crate::ReconciliationCoverage,
    /// Source assertions for explicitly compared fields, frozen at run time.
    pub expected_totals: serde_json::Value,
    /// The figures the run actually observed, frozen beside `expected_totals`.
    pub actual_totals: serde_json::Value,
    /// Number of exceptions the run opened.
    pub exception_count: i64,
    /// Staff user who requested a manual run; `None` for scheduled runs.
    pub requested_by: Option<Uuid>,
    /// When the run started.
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    /// When the run finished; `None` while still running.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// Failure detail when the run ended `failed`.
    pub last_error: Option<String>,
}

time::serde::format_description!(reconciliation_day, Date, "[year]-[month]-[day]");

/// One exception a run opened — a disagreed fact, never silent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationException {
    /// Local exception identifier.
    pub exception_id: Uuid,
    /// Run that opened the exception.
    pub run_id: Uuid,
    /// Customer organization the exception blames, when scoped to one.
    pub organization_id: Option<Uuid>,
    /// `unmatched_usage` | `unmatched_journal` | `unmatched_payment` |
    /// `amount_drift` | `missing_cost` | `double_entry`.
    pub kind: String,
    /// The figure the projections expected, in minor units.
    pub expected_minor: Option<i64>,
    /// The figure actually observed, in minor units.
    pub actual_minor: Option<i64>,
    /// Table holding the blamed source row.
    pub source_table: Option<String>,
    /// Identifier of the blamed source row.
    pub source_ref: Option<String>,
    /// Stable compared field or source failure, distinct from other facts on this source.
    pub fact: String,
    /// Human-readable explanation of the disagreement.
    pub detail: Option<String>,
    /// `open` | `resolved` | `waived`.
    pub state: String,
    /// Staff user who closed the exception, if any.
    pub resolved_by: Option<Uuid>,
    /// The resolution or waiver explanation.
    pub resolution: Option<String>,
    /// When the exception was closed, if it has been.
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
    /// When the exception was opened.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One corrective action against an exception — an explicit financial write,
/// never a balance overwrite.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationRepair {
    /// Local repair identifier.
    pub repair_id: Uuid,
    /// Exception the repair was applied to.
    pub exception_id: Uuid,
    /// Staff user who ran the repair.
    pub actor_user_id: Uuid,
    /// `reclassify_cost` | `adjust_journal` | `link_payment` | `mark_duplicate`.
    pub action: String,
    /// Snapshot of the affected fact before the write.
    pub before: serde_json::Value,
    /// Snapshot of the affected fact after the write.
    pub after: serde_json::Value,
    /// Finance-audit row that authorized the repair.
    pub audit_ref: Option<Uuid>,
    /// Operator's stated reason for the corrective action.
    pub reason: String,
    /// When the repair was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The run's outcome, for the job's log line.
#[derive(Debug)]
pub struct ReconciliationReport {
    /// The run row's id.
    pub run_id: Uuid,
    /// Exceptions the run opened.
    pub exceptions: i64,
    /// Whether every check held within tolerance.
    pub matched: bool,
}

pub(crate) fn run_from_row(row: &PgRow) -> Result<ReconciliationRun, WalletError> {
    Ok(ReconciliationRun {
        run_id: row.try_get("run_id")?,
        kind: row.try_get("kind")?,
        period_start: row.try_get("period_start")?,
        period_end: row.try_get("period_end")?,
        state: row.try_get("state")?,
        time_basis: row.try_get::<String, _>("time_basis")?.parse()?,
        latest_snapshot_id: row.try_get("latest_snapshot_id")?,
        coverage: serde_json::from_value(row.try_get("coverage")?).map_err(error::invalid)?,
        expected_totals: row.try_get("expected_totals")?,
        actual_totals: row.try_get("actual_totals")?,
        exception_count: row.try_get("exception_count")?,
        requested_by: row.try_get("requested_by")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        last_error: row.try_get("last_error")?,
    })
}

const EXCEPTION_COLS: &str = "exception_id, run_id, organization_id, kind, expected_minor, \
     actual_minor, source_table, source_ref, fact, detail, state, resolved_by, resolution, \
     resolved_at, created_at";

fn exception_from_row(row: &PgRow) -> Result<ReconciliationException, WalletError> {
    Ok(ReconciliationException {
        exception_id: row.try_get("exception_id")?,
        run_id: row.try_get("run_id")?,
        organization_id: row.try_get("organization_id")?,
        kind: row.try_get("kind")?,
        expected_minor: row.try_get("expected_minor")?,
        actual_minor: row.try_get("actual_minor")?,
        source_table: row.try_get("source_table")?,
        source_ref: row.try_get("source_ref")?,
        fact: row.try_get("fact")?,
        detail: row.try_get("detail")?,
        state: row.try_get("state")?,
        resolved_by: row.try_get("resolved_by")?,
        resolution: row.try_get("resolution")?,
        resolved_at: row.try_get("resolved_at")?,
        created_at: row.try_get("created_at")?,
    })
}

const RUN_COLS: &str = "run_id, kind, period_start, period_end, state, time_basis, latest_snapshot_id, coverage, expected_totals, \
     actual_totals, exception_count, requested_by, started_at, finished_at, last_error";

impl Db {
    /// Run an independently verified booking-basis comparison and retain its snapshot.
    /// Previous completed evidence remains intact if capture fails.
    pub async fn run_reconciliation(
        &self,
        kind: &str,
        period_start: Date,
        period_end: Date,
        requested_by: Option<Uuid>,
    ) -> Result<ReconciliationReport, WalletError> {
        self.run_reconciliation_with_basis(
            kind,
            period_start,
            period_end,
            TimeBasis::Booking,
            requested_by,
        )
        .await
    }

    /// The run list — `GET /api/v1/business/reconciliation-runs`, `ReportsRead`.
    ///
    /// # Errors
    ///
    /// Storage and authorization failures surface as [`WalletError`].
    pub async fn reconciliation_runs(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<ReconciliationRun>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        tx.commit().await?;
        let rows = sqlx::query(&format!(
            "SELECT {RUN_COLS} FROM oxsum.reconciliation_runs \
             WHERE ($1::uuid IS NULL OR run_id < $1) \
             ORDER BY run_id DESC LIMIT $2"
        ))
        .bind(after)
        .bind(limit.clamp(1, 200))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(run_from_row).collect()
    }

    /// One run — `GET /api/v1/business/reconciliation-runs/{id}`, `ReportsRead`.
    ///
    /// # Errors
    ///
    /// Storage and authorization failures surface as [`WalletError`].
    pub async fn reconciliation_run(
        &self,
        token: &str,
        run_id: Uuid,
    ) -> Result<Option<ReconciliationRun>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        tx.commit().await?;
        let row = sqlx::query(&format!(
            "SELECT {RUN_COLS} FROM oxsum.reconciliation_runs WHERE run_id = $1"
        ))
        .bind(run_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(run_from_row).transpose()
    }

    /// A run's exceptions — `GET …/reconciliation-runs/{id}/exceptions`,
    /// `ReportsRead`. `None` run_id lists every open exception (the work
    /// list); `only_open` narrows a run's list to what still needs a human.
    ///
    /// # Errors
    ///
    /// Storage and authorization failures surface as [`WalletError`].
    pub async fn reconciliation_exceptions(
        &self,
        token: &str,
        run_id: Option<Uuid>,
        only_open: bool,
    ) -> Result<Vec<ReconciliationException>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        tx.commit().await?;
        let rows = sqlx::query(&format!(
            "SELECT {EXCEPTION_COLS} FROM oxsum.reconciliation_exceptions \
             WHERE ($1::uuid IS NULL OR run_id = $1) \
               AND ($2::bool = false OR state = 'open') \
             ORDER BY created_at DESC LIMIT 500"
        ))
        .bind(run_id)
        .bind(only_open)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(exception_from_row).collect()
    }

    /// A staff-requested manual run — `POST /api/v1/business/reconciliation-runs`,
    /// `FundsWrite` + finance_audit. `requested_by` records the staff actor.
    ///
    /// # Errors
    ///
    /// Validation, storage and authorization failures surface as
    /// [`WalletError`].
    pub async fn request_reconciliation_run(
        &self,
        token: &str,
        period_start: Date,
        period_end: Date,
        idempotency_key: &str,
    ) -> Result<ReconciliationRun, WalletError> {
        self.request_reconciliation_run_with_basis(
            token,
            period_start,
            period_end,
            TimeBasis::Booking,
            idempotency_key,
        )
        .await
    }

    /// Closes an exception — `resolve` names the corrective outcome, `waive`
    /// why the drift is not a defect. `FundsWrite` + finance_audit.
    ///
    /// # Errors
    ///
    /// Validation, storage and authorization failures surface as
    /// [`WalletError`]; a non-open exception is `NotFound`.
    pub async fn close_reconciliation_exception(
        &self,
        token: &str,
        exception_id: Uuid,
        action: &str,
        resolution: &str,
        idempotency_key: &str,
    ) -> Result<ReconciliationException, WalletError> {
        if resolution.trim().is_empty() || resolution.chars().count() > 500 {
            return Err(WalletError::InvalidInput(
                "resolution must contain 1 to 500 characters".into(),
            ));
        }
        let (state, audit_action) = match action {
            "resolve" => ("resolved", "reconciliation.resolve"),
            "waive" => ("waived", "reconciliation.waive"),
            other => {
                return Err(WalletError::InvalidInput(format!(
                    "action must be resolve|waive, not {other:?}"
                )));
            }
        };
        authority::validate(idempotency_key, Some(resolution))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let principal =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        let changed = sqlx::query(
            "UPDATE oxsum.reconciliation_exceptions \
             SET state=$2, resolved_by=$3, resolution=$4, resolved_at=now() \
             WHERE exception_id=$1 AND state='open'",
        )
        .bind(exception_id)
        .bind(state)
        .bind(principal.user_id)
        .bind(resolution)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() == 0 {
            tx.rollback().await?;
            return Err(WalletError::NotFound(
                "no open exception under that id".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO oxsum.finance_audit \
             (audit_id, business_id, actor_user_id, action, target, detail, idempotency_key) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (business_id, action, idempotency_key) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(business)
        .bind(principal.user_id)
        .bind(audit_action)
        .bind(exception_id.to_string())
        .bind(serde_json::json!({"resolution":resolution,"state":state}))
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let row = sqlx::query(&format!(
            "SELECT {EXCEPTION_COLS} FROM oxsum.reconciliation_exceptions WHERE exception_id = $1"
        ))
        .bind(exception_id)
        .fetch_one(self.pool())
        .await?;
        exception_from_row(&row)
    }

    /// A corrective action against an open exception — `FundsWrite` +
    /// finance_audit + a `reconciliation_repairs` row. The action writes the
    /// correcting row (a reclassified `cost_basis`, a journal note, a payment
    /// link, a duplicate mark), never mutates a money column in place, and
    /// re-checks the exception's condition: a `reclassify_cost` on a
    /// `missing_cost` exception re-reads the row's basis — the exception
    /// stays open until the check agrees, so a repair cannot close by
    /// declaration alone.
    ///
    /// # Errors
    ///
    /// Validation, storage and authorization failures surface as
    /// [`WalletError`].
    pub async fn repair_reconciliation_exception(
        &self,
        token: &str,
        exception_id: Uuid,
        action: &str,
        reason: &str,
        patch: serde_json::Value,
        idempotency_key: &str,
    ) -> Result<ReconciliationRepair, WalletError> {
        if !matches!(
            action,
            "reclassify_cost" | "adjust_journal" | "link_payment" | "mark_duplicate"
        ) {
            return Err(WalletError::InvalidInput(format!(
                "action must be reclassify_cost|adjust_journal|link_payment|mark_duplicate, not {action:?}"
            )));
        }
        if reason.trim().is_empty() || reason.chars().count() > 500 {
            return Err(WalletError::InvalidInput(
                "reason must contain 1 to 500 characters".into(),
            ));
        }
        authority::validate(idempotency_key, Some(reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let principal =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        // The exception must be open and its source row fetchable.
        let exception = sqlx::query(&format!(
            "SELECT {EXCEPTION_COLS} FROM oxsum.reconciliation_exceptions \
             WHERE exception_id=$1 AND state='open' FOR UPDATE"
        ))
        .bind(exception_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("no open exception under that id".into()))?;
        let source_table: Option<String> = exception.try_get("source_table")?;
        let source_ref: Option<String> = exception.try_get("source_ref")?;
        let kind: String = exception.try_get("kind")?;

        // Apply the corrective write inside the transaction — each action
        // rewrites a provenance/basis field or records the instruction, and
        // snapshots before/after so the audit reads the change without
        // re-deriving.
        let (before, after) = match action {
            "reclassify_cost" => {
                if kind != "missing_cost" || source_table.as_deref() != Some("usage_records") {
                    return Err(WalletError::InvalidInput(
                        "reclassify_cost applies to a missing_cost usage_records exception".into(),
                    ));
                }
                let request_id = source_ref.as_deref().ok_or_else(|| {
                    WalletError::InvalidInput("the exception names no usage row".into())
                })?;
                let new_basis = patch
                    .get("costBasis")
                    .and_then(|v| v.as_str())
                    .unwrap_or("provider_reconciled");
                if !matches!(
                    new_basis,
                    "real_attempt" | "cache_reuse" | "estimate" | "provider_reconciled"
                ) {
                    return Err(WalletError::InvalidInput(
                        "costBasis must be a non-missing basis".into(),
                    ));
                }
                let new_cost = patch.get("upstreamCostMinor").and_then(|v| v.as_i64());
                let before_row = sqlx::query(
                    "SELECT cost_basis, upstream_cost_minor FROM oxsum.usage_records \
                     WHERE request_id=$1 FOR UPDATE",
                )
                .bind(request_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| WalletError::NotFound("the usage row is gone".into()))?;
                let before = serde_json::json!({
                    "costBasis": before_row.try_get::<Option<String>,_>("cost_basis")?,
                    "upstreamCostMinor": before_row.try_get::<Option<i64>,_>("upstream_cost_minor")?,
                });
                sqlx::query(
                    "UPDATE oxsum.usage_records \
                     SET cost_basis=$2, upstream_cost_minor=COALESCE($3, upstream_cost_minor) \
                     WHERE request_id=$1",
                )
                .bind(request_id)
                .bind(new_basis)
                .bind(new_cost)
                .execute(&mut *tx)
                .await?;
                let after = serde_json::json!({
                    "costBasis": new_basis,
                    "upstreamCostMinor": new_cost,
                });
                (before, after)
            }
            "mark_duplicate" => {
                // The mark says a second writer already covered the fact —
                // the row stays, and the exception's re-check is the close.
                let before =
                    serde_json::json!({"sourceTable": source_table, "sourceRef": source_ref});
                let after = serde_json::json!({"markedDuplicate": true});
                (before, after)
            }
            // `adjust_journal`/`link_payment` record the corrective
            // instruction — the journal append and the payment bind land
            // through their own write paths (a journal line is immutable, a
            // payment links under its own key), so the repair row is the
            // audited instruction and the re-run check is the close.
            _ => {
                let before = serde_json::json!({
                    "sourceTable": source_table, "sourceRef": source_ref, "patch": patch
                });
                let after = serde_json::json!({"applied": patch});
                (before, after)
            }
        };
        // The finance-audit row authorizing the write.
        let audit_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.finance_audit \
             (audit_id, business_id, actor_user_id, action, target, detail, idempotency_key) \
             VALUES ($1,$2,$3,'reconciliation.repair',$4,$5,$6)",
        )
        .bind(audit_id)
        .bind(business)
        .bind(principal.user_id)
        .bind(exception_id.to_string())
        .bind(serde_json::json!({"action":action,"reason":reason,"before":before,"after":after}))
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await?;
        // The repair row — the audited corrective fact.
        let repair_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.reconciliation_repairs \
             (repair_id, exception_id, actor_user_id, action, before, after, audit_ref, reason) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(repair_id)
        .bind(exception_id)
        .bind(principal.user_id)
        .bind(action)
        .bind(&before)
        .bind(&after)
        .bind(audit_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ReconciliationRepair {
            repair_id,
            exception_id,
            actor_user_id: principal.user_id,
            action: action.to_owned(),
            before,
            after,
            audit_ref: Some(audit_id),
            reason: reason.to_owned(),
            created_at: OffsetDateTime::now_utc(),
        })
    }
}
