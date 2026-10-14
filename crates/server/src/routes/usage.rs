use axum::extract::{Extension, State};
use serde::{Deserialize, Serialize};

use oxsum_core::Principal;

use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok, parse_day, wallet};
use crate::{AppState, today};

const USAGE_WINDOW_DAYS: i64 = 92;

/// The page bounds `GET /api/v1/billing-records` accepts.
const RECORDS_DEFAULT_LIMIT: usize = 50;
const RECORDS_MAX_LIMIT: usize = 200;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageQuery {
    from: Option<String>,
    to: Option<String>,
}

/// The keys this principal may attribute usage to: an API key or an owner/admin
/// session sees every key of the organization, a member only their own — the
/// dashboard's `key_scope` rule applied to the REST surface.
async fn scoped_key_ids(
    state: &AppState,
    principal: &Principal,
) -> Result<Option<Vec<String>>, ApiError> {
    let scope = principal.key_scope();
    if matches!(
        scope,
        oxsum_core::KeyScope::Organization | oxsum_core::KeyScope::All
    ) {
        return Ok(None);
    }
    let keys = state
        .db
        .list_keys(principal.organization().id, scope)
        .await?;
    Ok(Some(
        keys.iter()
            .map(|key| key.id.as_simple().to_string())
            .collect(),
    ))
}

/// A row's attribution visible to the principal: unattributed rows are
/// organization history everyone reads, a key outside the scope drops the row.
fn in_scope(key_id: Option<&str>, scoped: &Option<Vec<String>>) -> bool {
    match (key_id, scoped) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(id), Some(keys)) => keys.iter().any(|key| key == id),
    }
}
pub(crate) async fn usage(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(query): ApiQuery<UsageQuery>,
) -> ApiResult<UsageRes> {
    let to = query
        .to
        .as_deref()
        .map(parse_day)
        .transpose()?
        .unwrap_or_else(today);
    let from = query
        .from
        .as_deref()
        .map(parse_day)
        .transpose()?
        .unwrap_or_else(|| to - time::Duration::days(USAGE_WINDOW_DAYS - 1));
    if from > to {
        return Err(ApiError::Validation("`from` must not be after `to`".into()));
    }
    if to - from >= time::Duration::days(USAGE_WINDOW_DAYS) {
        return Err(ApiError::Validation(format!(
            "the window is bounded to {USAGE_WINDOW_DAYS} days"
        )));
    }
    let organization = principal.organization();
    let days = state
        .db
        .usage_daily(&organization.tenant_id, from, to)
        .await?;
    let scoped = scoped_key_ids(&state, &principal).await?;
    let days = days
        .into_iter()
        .filter(|row| {
            in_scope(
                row.key_id.map(|id| id.as_simple().to_string()).as_deref(),
                &scoped,
            )
        })
        .collect();
    ok(UsageRes { days })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageRes {
    days: Vec<oxsum_core::UsageDay>,
}

/// `GET /api/v1/budget` — every budget scope the caller can read, with the
/// committed figure and the percentage consumed of its limit (issue #232).
/// `committed` is the figure the hold-side checks read: settled spend plus
/// outstanding holds, windowed to the key's `budgetDuration` when it has one.
pub(crate) async fn budget(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<BudgetRes> {
    let organization = principal.organization();
    let wallet = wallet(&state.tenants, organization).await?;
    let (available, reserved, credit_limit, credit_used) = tokio::try_join!(
        wallet.available(),
        wallet.reserved(),
        wallet.credit_limit(),
        wallet.credit_used()
    )?;
    // The member's own seat when it carries a limit — a key names no member.
    let member = match principal.user_id() {
        Some(user_id) => match state.db.membership_budget(organization.id, user_id).await? {
            Some(limit) => {
                let committed = wallet.member_committed(&organization.id, &user_id).await?;
                Some(MemberBudget {
                    budget_limit_minor: limit,
                    committed_minor: committed,
                    consumed_percent: consumed_percent(committed, limit),
                })
            }
            None => None,
        },
        None => None,
    };
    let today = today();
    let keys = state
        .db
        .list_keys(organization.id, principal.key_scope())
        .await?;
    let mut key_budgets = Vec::with_capacity(keys.len());
    for key in keys.iter().filter(|key| key.revoked_at.is_none()) {
        let window_start = key
            .budget_duration
            .map(|duration| duration.period_start(today));
        let committed = wallet.key_committed_window(&key.id, window_start).await?;
        key_budgets.push(KeyBudget {
            key_id: key.id,
            name: key.name.clone(),
            prefix: key.prefix.clone(),
            spend_limit_minor: key.spend_limit_minor,
            budget_duration: key
                .budget_duration
                .map(|duration| duration.as_str().to_owned()),
            window_start,
            committed_minor: committed,
            consumed_percent: key
                .spend_limit_minor
                .and_then(|limit| consumed_percent(committed, limit)),
        });
    }
    ok(BudgetRes {
        organization: OrgBudget {
            available_minor: available,
            reserved_minor: reserved,
            credit_limit_minor: (credit_limit > 0).then_some(credit_limit),
            credit_used_minor: (credit_limit > 0).then_some(credit_used),
            consumed_percent: consumed_percent(credit_used, credit_limit),
        },
        member,
        keys: key_budgets,
    })
}

/// The percentage `committed` is of `limit`, floored — `None` when there is no
/// numeric line to measure against.
fn consumed_percent(committed: i64, limit: i64) -> Option<i32> {
    (limit > 0).then(|| (i128::from(committed.max(0)) * 100 / i128::from(limit)) as i32)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BudgetRes {
    organization: OrgBudget,
    /// The acting member's own budget — `null` for a key and for a seat with
    /// no `budgetLimitMinor`.
    member: Option<MemberBudget>,
    keys: Vec<KeyBudget>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrgBudget {
    available_minor: i64,
    reserved_minor: i64,
    credit_limit_minor: Option<i64>,
    credit_used_minor: Option<i64>,
    consumed_percent: Option<i32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MemberBudget {
    budget_limit_minor: i64,
    committed_minor: i64,
    /// `null` when the limit is 0 — a line that admits nothing has no ratio.
    consumed_percent: Option<i32>,
}

/// `Option<Date>` as `YYYY-MM-DD` or `null` — the contract's `format: date`.
/// `time`'s default serde emits `[year, ordinal]`, which the API does not use.
mod date_opt {
    use serde::Serializer;

    time::serde::format_description!(day, Date, "[year]-[month]-[day]");

    pub(super) fn serialize<S: Serializer>(
        value: &Option<time::Date>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(date) => day::serialize(date, serializer),
            None => serializer.serialize_none(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyBudget {
    key_id: uuid::Uuid,
    name: Option<String>,
    prefix: String,
    spend_limit_minor: Option<i64>,
    budget_duration: Option<String>,
    /// The UTC date the key's current window began — `null` for a cumulative
    /// limit, whose consumption is all-time.
    #[serde(with = "date_opt")]
    window_start: Option<time::Date>,
    committed_minor: i64,
    consumed_percent: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordsQuery {
    before: Option<u64>,
    limit: Option<usize>,
}

pub(crate) async fn billing_records(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(query): ApiQuery<RecordsQuery>,
) -> ApiResult<RecordsRes> {
    let limit = query.limit.unwrap_or(RECORDS_DEFAULT_LIMIT);
    if !(1..=RECORDS_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Validation(format!(
            "limit must be 1 to {RECORDS_MAX_LIMIT}"
        )));
    }
    let w = wallet(&state.tenants, principal.organization()).await?;
    let page = w.requests_page(query.before, limit).await?;
    let scoped = scoped_key_ids(&state, &principal).await?;
    ok(RecordsRes {
        rows: page
            .rows
            .into_iter()
            .filter(|row| in_scope(row.key_id.as_deref(), &scoped))
            .collect(),
        next_cursor: page.next_cursor,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordsRes {
    rows: Vec<oxsum_core::RequestEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EstimateReq {
    model: String,
    /// Which billing mode's routes to price against; absent means `chat`
    /// (issue #170).
    mode: Option<oxsum_core::BillingMode>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    service_tier: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EstimateRes {
    estimate_minor: i64,
    model: String,
    version: i64,
}

/// The freeze a request of this shape would take — `Price::estimate_minor`, the
/// same arithmetic the gateway's hold path runs, so the number is the settle's
/// ceiling rather than a guess. Nothing is held and nothing is charged.
///
/// A model served by several channels estimates at the dearest route: the
/// gateway freezes the max across candidates, so the estimate is the bound the
/// hold would take (issue #168). `version` names the route that priced it.
pub(crate) async fn estimate_price(
    State(state): State<AppState>,
    Extension(_principal): Extension<Principal>,
    ApiJson(r): ApiJson<EstimateReq>,
) -> ApiResult<EstimateRes> {
    // Only the mode's own routes price the shape: an embeddings estimate reads
    // `embeddings` prices, a chat estimate `chat` ones (issue #170).
    let mode = r.mode.unwrap_or_default();
    let routes: Vec<_> = state
        .db
        .routes(&r.model)
        .await?
        .into_iter()
        .filter(|route| route.price.mode == mode)
        .collect();
    if routes.is_empty() {
        return Err(ApiError::Validation(format!(
            "no channel serves model {:?}",
            r.model
        )));
    }
    let mut estimate = i64::MIN;
    let mut version = routes[0].version;
    for route in &routes {
        let priced = route.price.estimate_minor(
            r.input_tokens.unwrap_or(0),
            r.output_tokens,
            r.service_tier.as_deref(),
        )?;
        if priced > estimate {
            estimate = priced;
            version = route.version;
        }
    }
    ok(EstimateRes {
        estimate_minor: estimate,
        model: r.model,
        version,
    })
}

/// The public catalog: every model's current price version. The query never
/// selects channel credentials, so there is nothing to withhold here.
pub(crate) async fn pricing(
    State(state): State<AppState>,
    Extension(_principal): Extension<Principal>,
) -> ApiResult<PricingRes> {
    ok(PricingRes {
        models: state.db.catalog().await?,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PricingRes {
    models: Vec<oxsum_core::CatalogModel>,
}
