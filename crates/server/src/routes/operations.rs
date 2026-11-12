//! The operations read surface (issue #601): `GET /api/v1/operations`, the
//! member's org-scoped list of the durable turn records, and
//! `GET /api/v1/operations/{operationId}`, one operation with its attempts.
//!
//! The surface is deliberately read-only: the operation is the turn's
//! recovery record, not a thing a caller mutates — the cancel belongs to
//! T12's coordinated settlement. A session reads any operation in its
//! organization; a consumption key reads only the operations its own `key_id`
//! admitted, the same scoping `task_for_key` keeps.

use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use oxsum_core::{Principal, RequestOperation};

use crate::AppState;
use crate::error::{ApiError, ApiQuery};
use crate::routes::{ApiResult, ok};

/// The page bounds `GET /api/v1/operations` accepts.
const OPERATIONS_DEFAULT_LIMIT: i64 = 50;
const OPERATIONS_MAX_LIMIT: i64 = 200;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OperationsQuery {
    limit: Option<i64>,
    cursor: Option<String>,
    /// The admin endpoint's extra filter — `organizationId` narrows the
    /// all-orgs read to one. Ignored on the member endpoint: a member's scope
    /// is their own org and stays it, whatever they ask for.
    organization_id: Option<Uuid>,
    /// Only operations in this lifecycle state — `running`, `succeeded`,
    /// `timed_out`, any of the stored spellings.
    state: Option<String>,
}

/// The operations cursor, encoded: `<unix nanos>:<operation id>` — the keyset
/// `Db::list_operations` orders by, opaque to the caller like the
/// request-log cursor (issue #93's rule).
fn encode_operation_cursor((created_at, id): (OffsetDateTime, Uuid)) -> String {
    format!("{}:{}", created_at.unix_timestamp_nanos(), id)
}

fn decode_operation_cursor(text: &str) -> Result<(OffsetDateTime, Uuid), ApiError> {
    let bad = || ApiError::Validation("a malformed operations cursor".into());
    let (nanos, id) = text.split_once(':').ok_or_else(bad)?;
    let nanos = nanos.parse::<i128>().map_err(|_| bad())?;
    let created_at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    let id = id.parse::<Uuid>().map_err(|_| bad())?;
    Ok((created_at, id))
}

/// One operation as the API answers it — camelCase, `operationId` first.
/// `organizationId` rides only on the admin read: a member's org is always
/// their own, so naming it on the wire would repeat the credential.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationRes {
    operation_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_id: Option<Uuid>,
    surface: String,
    bound_minor: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    hold_key: Option<String>,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    charged_minor: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome_kind: Option<String>,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    completed_at: Option<OffsetDateTime>,
}

impl OperationRes {
    /// The member shape: the org is always the reader's own.
    fn member(operation: &RequestOperation) -> Self {
        Self::shape(operation, false)
    }
    /// The admin shape: `organizationId` names which org the operation billed.
    fn admin(operation: &RequestOperation) -> Self {
        Self::shape(operation, true)
    }
    fn shape(operation: &RequestOperation, with_org: bool) -> Self {
        Self {
            operation_id: operation.operation_id,
            organization_id: with_org.then_some(operation.organization_id),
            key_id: operation.key_id,
            surface: operation.surface.clone(),
            bound_minor: operation.bound_minor,
            hold_key: operation.hold_key.clone(),
            state: operation.state.as_str().to_owned(),
            charged_minor: operation.charged_minor,
            outcome_kind: operation.outcome_kind.clone(),
            model: operation.model.clone(),
            channel: operation.channel.clone(),
            created_at: operation.created_at,
            completed_at: operation.completed_at,
        }
    }
}

/// One attempt as the detail read serves it — the failover audit.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationAttemptRes {
    attempt_id: Uuid,
    channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    external_id: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage_json: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    started_at: OffsetDateTime,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    completed_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationsRes {
    operations: Vec<OperationRes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// The operation plus its attempts — the detail read's body.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationDetailRes {
    #[serde(flatten)]
    operation: OperationRes,
    attempts: Vec<OperationAttemptRes>,
}

fn attempt_res(attempt: &oxsum_core::OperationAttempt) -> OperationAttemptRes {
    OperationAttemptRes {
        attempt_id: attempt.attempt_id,
        channel: attempt.channel.clone(),
        credential_id: attempt.credential_id,
        external_id: attempt.external_id.clone(),
        status: attempt.status.as_str().to_owned(),
        usage_json: attempt.usage_json.clone(),
        error: attempt.error.clone(),
        started_at: attempt.started_at,
        completed_at: attempt.completed_at,
    }
}

/// `GET /api/v1/operations` — the member's read, scoped to their
/// organization: a session lists all of the org's operations, a consumption
/// key lists only the ones its own credential admitted.
pub(crate) async fn operations(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(query): ApiQuery<OperationsQuery>,
) -> ApiResult<OperationsRes> {
    let limit = query.limit.unwrap_or(OPERATIONS_DEFAULT_LIMIT);
    if !(1..=OPERATIONS_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Validation(format!(
            "limit must be 1 to {OPERATIONS_MAX_LIMIT}"
        )));
    }
    let after = query
        .cursor
        .as_deref()
        .map(decode_operation_cursor)
        .transpose()?;
    let organization_id = principal.organization().id;
    // The state filter is validated against the stored spellings before it
    // reaches SQL — an unknown state answers 400, not an empty page.
    if let Some(state) = &query.state {
        oxsum_core::OperationState::parse(state)
            .map_err(|error| ApiError::Validation(error.to_string()))?;
    }
    // A consumption key's own `key_id` is the read's scope — it lists only
    // the operations it admitted, the same rule `task_for_key` keeps.
    let key_id = principal.acting_key().map(|key| key.key_id);
    let mut page = state
        .db
        .list_operations(
            Some(organization_id),
            key_id,
            query.state.as_deref(),
            limit + 1,
            after,
        )
        .await?;
    let more = page.len() as i64 > limit;
    page.truncate(limit as usize);
    let next_cursor = more
        .then(|| page.last())
        .flatten()
        .map(|o| encode_operation_cursor((o.created_at, o.operation_id)));
    ok(OperationsRes {
        operations: page.iter().map(OperationRes::member).collect(),
        next_cursor,
    })
}

/// `GET /api/v1/operations/{operationId}` — one operation with its attempt
/// rows. A session reads any operation in its organization; a consumption key
/// reads only the operations its own `key_id` admitted — a foreign id answers
/// 404, because an operation's existence is not an oracle for another key.
pub(crate) async fn operation(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(operation_id): Path<Uuid>,
) -> ApiResult<OperationDetailRes> {
    let organization_id = principal.organization().id;
    let operation = match principal.acting_key() {
        Some(key) => {
            state
                .db
                .operation_for_key(organization_id, key.key_id, operation_id)
                .await?
        }
        None => state.db.operation(organization_id, operation_id).await?,
    };
    let Some(operation) = operation else {
        return Err(ApiError::NotFound("operation not found".into()));
    };
    let attempts = state.db.operation_attempts(operation_id).await?;
    ok(OperationDetailRes {
        operation: OperationRes::member(&operation),
        attempts: attempts.iter().map(attempt_res).collect(),
    })
}

/// `GET /api/v1/admin/operations` — the operator's read across every
/// organization, narrowed by `organizationId` when the query names one.
pub(crate) async fn admin_operations(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<OperationsQuery>,
) -> ApiResult<OperationsRes> {
    let limit = query.limit.unwrap_or(OPERATIONS_DEFAULT_LIMIT);
    if !(1..=OPERATIONS_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Validation(format!(
            "limit must be 1 to {OPERATIONS_MAX_LIMIT}"
        )));
    }
    let after = query
        .cursor
        .as_deref()
        .map(decode_operation_cursor)
        .transpose()?;
    if let Some(state) = &query.state {
        oxsum_core::OperationState::parse(state)
            .map_err(|error| ApiError::Validation(error.to_string()))?;
    }
    let mut page = state
        .db
        .list_operations(
            query.organization_id,
            None,
            query.state.as_deref(),
            limit + 1,
            after,
        )
        .await?;
    let more = page.len() as i64 > limit;
    page.truncate(limit as usize);
    let next_cursor = more
        .then(|| page.last())
        .flatten()
        .map(|o| encode_operation_cursor((o.created_at, o.operation_id)));
    ok(OperationsRes {
        operations: page.iter().map(OperationRes::admin).collect(),
        next_cursor,
    })
}
