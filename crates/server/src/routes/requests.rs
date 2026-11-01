//! Request analytics (issue #505): `GET /api/v1/requests`, the member's
//! org-scoped read of the per-request operational record, and the cursor
//! codec the keyset page echoes.

use axum::extract::{Extension, State};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use oxsum_core::Principal;

use crate::AppState;
use crate::error::{ApiError, ApiQuery};
use crate::routes::{ApiResult, ok};

/// The page bounds `GET /api/v1/requests` accepts.
const REQUESTS_DEFAULT_LIMIT: usize = 50;
const REQUESTS_MAX_LIMIT: usize = 200;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestsQuery {
    key_id: Option<Uuid>,
    model: Option<String>,
    channel: Option<String>,
    from: Option<String>,
    to: Option<String>,
    status: Option<String>,
    min_charge: Option<i64>,
    limit: Option<usize>,
    cursor: Option<String>,
    /// The admin endpoint's extra filter — `organizationId` narrows the
    /// all-orgs read to one. Ignored on the member endpoint: a member's scope
    /// is their own org and stays it, whatever they ask for.
    organization_id: Option<Uuid>,
}

/// The request-log cursor, encoded: `<unix nanos>:<request id>`. The pair is
/// the keyset `Db::request_log_page` orders by — `created_at` alone cannot
/// order two rows written in the same instant — and the string is opaque to
/// the caller: echoed, never constructed (the same rule the organizations
/// cursor keeps, issue #93).
fn encode_request_cursor((created_at, id): (OffsetDateTime, String)) -> String {
    format!("{}:{}", created_at.unix_timestamp_nanos(), id)
}

fn decode_request_cursor(text: &str) -> Result<(OffsetDateTime, String), ApiError> {
    let bad = || ApiError::Validation("a malformed requests cursor".into());
    let (nanos, id) = text.split_once(':').ok_or_else(bad)?;
    let nanos = nanos.parse::<i128>().map_err(|_| bad())?;
    let created_at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    if id.is_empty() {
        return Err(bad());
    }
    Ok((created_at, id.to_owned()))
}

/// `from`/`to` as instants: RFC 3339, the spelling `date-time` schemas and
/// every `createdAt` field already carry. A malformed bound is a validation
/// failure, not an empty result.
fn parse_instant(value: &str) -> Result<OffsetDateTime, ApiError> {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|_| ApiError::Validation(format!("{value:?} is not an RFC 3339 instant")))
}

fn parse_status(value: &str) -> Result<String, ApiError> {
    match value {
        "settled" | "errored" | "refused" => Ok(value.to_owned()),
        _ => Err(ApiError::Validation(
            "status must be settled, errored or refused".into(),
        )),
    }
}

/// One row as the API answers it — `organizationId` elided on the member's
/// own org (it can only ever name theirs), present on the admin read.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestLogRowRes {
    request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_id: Option<Uuid>,
    model_requested: String,
    model_served: String,
    channel: String,
    admission_outcome: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttft_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_ms: Option<i64>,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    charge_minor: i64,
    upstream_attempts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_status: Option<i64>,
    created_at: OffsetDateTime,
}

impl RequestLogRowRes {
    /// The member shape: the org is always the reader's own, so it stays off
    /// the wire.
    fn member(row: &oxsum_core::RequestLogRow) -> Self {
        Self::shape(row, false)
    }
    /// The admin shape: `organizationId` names which org the row billed.
    fn admin(row: &oxsum_core::RequestLogRow) -> Self {
        Self::shape(row, true)
    }
    fn shape(row: &oxsum_core::RequestLogRow, with_org: bool) -> Self {
        Self {
            request_id: row.request_id.clone(),
            entry_id: row.entry_id,
            organization_id: with_org.then_some(row.organization_id),
            key_id: row.key_id,
            model_requested: row.model_requested.clone(),
            model_served: row.model_served.clone(),
            channel: row.channel.clone(),
            admission_outcome: row.admission_outcome.clone(),
            status: row.status.clone(),
            ttft_ms: row.ttft_ms,
            total_ms: row.total_ms,
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cached_tokens: row.cached_tokens,
            charge_minor: row.charge_minor,
            upstream_attempts: row.upstream_attempts,
            error_kind: row.error_kind.clone(),
            http_status: row.http_status,
            created_at: row.created_at,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestsRes {
    rows: Vec<RequestLogRowRes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// Builds the filter the page reads from the query — shared by the member and
/// the admin endpoint so they narrow the same way.
fn build_filter(
    query: &RequestsQuery,
    scope: Option<Uuid>,
    filter_org: Option<Uuid>,
) -> Result<oxsum_core::RequestLogFilter, ApiError> {
    Ok(oxsum_core::RequestLogFilter {
        scope_organization_id: scope,
        filter_organization_id: filter_org,
        key_id: query.key_id,
        model_served: query.model.clone(),
        channel: query.channel.clone(),
        from: query.from.as_deref().map(parse_instant).transpose()?,
        to: query.to.as_deref().map(parse_instant).transpose()?,
        status: query.status.as_deref().map(parse_status).transpose()?,
        min_charge: query.min_charge,
    })
}

/// `GET /api/v1/requests` — the member's read, scoped to their organization.
pub(crate) async fn requests(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(query): ApiQuery<RequestsQuery>,
) -> ApiResult<RequestsRes> {
    let limit = query.limit.unwrap_or(REQUESTS_DEFAULT_LIMIT);
    if !(1..=REQUESTS_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Validation(format!(
            "limit must be 1 to {REQUESTS_MAX_LIMIT}"
        )));
    }
    let after = query
        .cursor
        .as_deref()
        .map(decode_request_cursor)
        .transpose()?;
    let filter = build_filter(&query, Some(principal.organization().id), None)?;
    let page = state.db.request_log_page(&filter, after, limit).await?;
    ok(RequestsRes {
        rows: page.rows.iter().map(RequestLogRowRes::member).collect(),
        next_cursor: page.next_cursor.map(encode_request_cursor),
    })
}

/// `GET /api/v1/admin/requests` — the operator's read across every
/// organization, narrowed by `organizationId` when the query names one.
pub(crate) async fn admin_requests(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<RequestsQuery>,
) -> ApiResult<RequestsRes> {
    let limit = query.limit.unwrap_or(REQUESTS_DEFAULT_LIMIT);
    if !(1..=REQUESTS_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Validation(format!(
            "limit must be 1 to {REQUESTS_MAX_LIMIT}"
        )));
    }
    let after = query
        .cursor
        .as_deref()
        .map(decode_request_cursor)
        .transpose()?;
    let filter = build_filter(&query, None, query.organization_id)?;
    let page = state.db.request_log_page(&filter, after, limit).await?;
    ok(RequestsRes {
        rows: page.rows.iter().map(RequestLogRowRes::admin).collect(),
        next_cursor: page.next_cursor.map(encode_request_cursor),
    })
}
