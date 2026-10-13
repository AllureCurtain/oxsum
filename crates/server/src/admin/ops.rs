//! `/api/v1/admin`: the platform's own eyes — holds in flight, settlement
//! anomalies, the margin and the reconciliation, plus the audit log every
//! admin write lands in.

use axum::extract::State;
use oxsum_core::{AuditEntry, InFlightHold, Reconciliation, SettlementKind, audit_action};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiQuery};
use crate::routes::{ApiResult, ok};
use crate::{
    AppState,
    admin::{decode_organization_cursor, encode_organization_cursor},
};

/// The audit query string: the page size, where the walk resumes, and an
/// optional exact action to narrow to.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AuditQuery {
    limit: Option<usize>,
    cursor: Option<String>,
    action: Option<String>,
}

/// One page of audit rows, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditPageRes {
    entries: Vec<AuditEntry>,
    /// The cursor the next page asks with; absent at the log's end.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// The audit log, newest first — every mutating call on this surface, one row
/// each, written after the mutation committed. `action` narrows to one exact
/// action name; an unknown one is a 400, not an empty page masquerading as
/// quiet.
pub(crate) async fn audit(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<AuditQuery>,
) -> ApiResult<AuditPageRes> {
    if let Some(action) = &query.action
        && !audit_action::ALL.contains(&action.as_str())
    {
        return Err(ApiError::Validation(format!("unknown action: {action:?}")));
    }
    let after = query
        .cursor
        .as_deref()
        .map(decode_organization_cursor)
        .transpose()?;
    let page = state
        .db
        .audit_page(after, query.action.as_deref(), query.limit.unwrap_or(100))
        .await?;
    ok(AuditPageRes {
        entries: page.rows,
        next_cursor: page.next_cursor.map(encode_organization_cursor),
    })
}

/// Every unsettled hold across all organizations, newest first: the in-flight list.
///
/// The rows are the sweeper's watch table joined to the organizations they belong to;
/// what the ledger still reserves is what the page shows, because a watch row whose
/// hold already settled is deleted rather than listed.
pub(crate) async fn holds(State(state): State<AppState>) -> ApiResult<Vec<InFlightHold>> {
    ok(state.db.open_holds().await?)
}

/// The settlement kinds an admin reviews: the turns where usage was never reported, the
/// caller left mid-stream, the charge hit the freeze's ceiling, or the sweeper had to
/// release a hold nobody settled.
const ANOMALOUS: &[SettlementKind] = &[
    SettlementKind::Capped,
    SettlementKind::Estimated,
    SettlementKind::ClientCancelled,
    SettlementKind::Swept,
    SettlementKind::Unpriced,
];

/// One anomalous turn, as the anomalies endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnomalyRes {
    organization: String,
    request_id: String,
    model: String,
    channel: String,
    price_version: i64,
    kind: SettlementKind,
    /// How many upstream calls the turn made (issue #168) — above 1 means it
    /// failed over between channels.
    upstream_attempts: i64,
    charged_minor: i64,
    freeze_minor: i64,
    /// The settlement's booking date, `YYYY-MM-DD`.
    booked_on: String,
}

/// The settled turns that did not price cleanly, across all organizations, newest
/// first — read back from each ledger's own settlement records, so every row is
/// exactly what that turn's bill proves.
pub(crate) async fn anomalies(State(state): State<AppState>) -> ApiResult<Vec<AnomalyRes>> {
    let mut answer = Vec::new();
    for organization in state.db.organizations().await? {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        for turn in wallet.recent_settlements(100).await? {
            if !ANOMALOUS.contains(&turn.record.kind) {
                continue;
            }
            answer.push(AnomalyRes {
                organization: organization.name.clone(),
                request_id: turn.record.request,
                model: turn.record.model,
                channel: turn.record.channel,
                price_version: turn.record.price_version,
                kind: turn.record.kind,
                upstream_attempts: turn.record.upstream_attempts,
                charged_minor: turn.record.charged,
                freeze_minor: turn.record.freeze,
                booked_on: turn.booked_on.to_string(),
            });
        }
    }
    // Booking dates are days, so within one day the per-ledger order stands; across
    // days newest first.
    answer.sort_by(|a, b| b.booked_on.cmp(&a.booked_on));
    ok(answer)
}

/// One `(channel, model)` pair's margin, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MarginRes {
    channel: String,
    model: String,
    turns: i64,
    charged_minor: i64,
    upstream_cost_minor: i64,
    margin_minor: i64,
    untracked_turns: i64,
}

/// What the platform charged versus what upstream cost it, per channel and
/// model — summed over the usage rows every settled turn writes, so the sums
/// are exactly what the mutable record holds (issue #112).
pub(crate) async fn margin(State(state): State<AppState>) -> ApiResult<Vec<MarginRes>> {
    let mut answer = Vec::new();
    for row in state.db.margin().await? {
        answer.push(MarginRes {
            margin_minor: row.charged_minor - row.upstream_cost_minor,
            channel: row.channel,
            model: row.model,
            turns: row.turns,
            charged_minor: row.charged_minor,
            upstream_cost_minor: row.upstream_cost_minor,
            untracked_turns: row.untracked_turns,
        });
    }
    ok(answer)
}

/// The reconciliation report: the drift between the ledgers and the projections
/// that claim to describe them, one entry per class with a bounded sample —
/// read-only, for an operator to act on (issue #134).
pub(crate) async fn reconciliation(State(state): State<AppState>) -> ApiResult<Reconciliation> {
    ok(state.db.reconcile().await?)
}
