//! `/api/v1/admin`: the platform's own eyes — holds in flight, settlement
//! anomalies, the margin and the reconciliation, plus the audit log every
//! admin write lands in.

use axum::extract::{Path, State};
use oxsum_core::{
    AnomalyFacts, AuditEntry, InFlightHold, Reconciliation, SettlementKind, anomaly_score,
    audit_action,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use crate::{
    AppState,
    admin::{decode_organization_cursor, encode_organization_cursor, record},
    today,
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

/// The deposits query string: an optional organization, and how many rows.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DepositsQuery {
    organization_id: Option<Uuid>,
    limit: Option<i64>,
}

/// One deposit row as the console lists it (issue #354).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DepositRes {
    deposit_id: Uuid,
    organization_id: Uuid,
    organization: String,
    rail: String,
    payment_ref: String,
    amount_minor: i64,
    received_minor: Option<i64>,
    status: String,
    entry_id: Option<Uuid>,
    reversal_entry_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    reversed_at: Option<OffsetDateTime>,
    /// The rail's refund standing, when it issued one (issue #360).
    refund_status: Option<String>,
    /// When a dispute opened on the payment, when the rail reported one.
    #[serde(with = "time::serde::rfc3339::option")]
    disputed_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

impl From<oxsum_core::AdminDeposit> for DepositRes {
    fn from(deposit: oxsum_core::AdminDeposit) -> Self {
        Self {
            deposit_id: deposit.deposit_id,
            organization_id: deposit.organization_id,
            organization: deposit.organization,
            rail: deposit.rail,
            payment_ref: deposit.payment_ref,
            amount_minor: deposit.amount_minor,
            received_minor: deposit.received_minor,
            status: deposit.status,
            entry_id: deposit.entry_id,
            reversal_entry_id: deposit.reversal_entry_id,
            reversed_at: deposit.reversed_at,
            refund_status: deposit.refund_status,
            disputed_at: deposit.disputed_at,
            created_at: deposit.created_at,
        }
    }
}

/// Every deposit, newest first, optionally narrowed to one organization
/// (issue #354). Read-only: the rails write these rows; the console only reads
/// them, because confirming or reversing money is a webhook's or an operator
/// action with its own audit, never a list view's side effect.
pub(crate) async fn deposits(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<DepositsQuery>,
) -> ApiResult<Vec<DepositRes>> {
    if let Some(organization) = query.organization_id {
        // An unknown organization is 404, not an empty list that reads like
        // "never paid".
        state.db.organization_by_id(organization).await?;
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let rows = state
        .db
        .admin_deposits(query.organization_id, limit)
        .await?;
    ok(rows.into_iter().map(DepositRes::from).collect())
}

/// The body of `POST …/deposits/{depositId}/reverse`: the reason the deposit is
/// reversed — it becomes the reversal entry's description, so it is part of
/// what the ledger proves.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReverseDepositReq {
    reason: String,
}

/// Reverses one credited deposit (issue #356, decision D12): the ledger half
/// of a refund. One entry posts — Wallet debited, Cash credited, the reason
/// as its description — and the row flips to `reversed` naming it.
///
/// One reversal per deposit ever: the ledger key is derived from the deposit
/// id, so a retry or a double-click answers the standing reversal rather than
/// posting twice. The wallet's own no-overdraft rule refuses what the
/// purchased pool cannot carry — `402 INSUFFICIENT_FUNDS` — and a deposit
/// that is not `credited` is `409`, an unknown one `404`. No payment provider
/// is called: returning the money through the rail is a separate issue.
pub(crate) async fn reverse_deposit(
    State(state): State<AppState>,
    Path(deposit_id): Path<Uuid>,
    ApiJson(request): ApiJson<ReverseDepositReq>,
) -> ApiResult<DepositRes> {
    // The deposit names the organization whose wallet takes the debit; an
    // unknown deposit is 404 before any ledger work — the domain's own
    // refusal under the row lock is the same answer, this lookup only finds
    // the tenant the wallet opens under.
    let deposit = state
        .db
        .admin_deposit(deposit_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let organization = state.db.organization_by_id(deposit.organization_id).await?;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let reversed = state
        .db
        .reverse_deposit(&wallet, deposit_id, &request.reason, today())
        .await?;
    // The payment-provider half (issue #360): a hyperswitch deposit also
    // returns the customer's money through the switch. The reversal already
    // committed — the books are right regardless — so a switch refusal is
    // recorded on the row as `refundStatus: failed`, never propagated into the
    // answer. A retry on a `failed` refund re-issues the call; anything else
    // (pending, succeeded, or a rail with no refund path) is left alone.
    let mut deposit = reversed;
    if deposit.rail == oxsum_core::RAIL_HYPERSWITCH
        && let Some(switch) = state.config.hyperswitch()
        && let Ok(Some(target)) = state.db.hyperswitch_refund_target(deposit_id).await
    {
        let issue =
            target.refund_status.is_none() || target.refund_status.as_deref() == Some("failed");
        if issue {
            match switch
                .create_refund(
                    &state.http,
                    &target.payment_id,
                    deposit_id,
                    target.amount_minor,
                    &request.reason,
                )
                .await
            {
                Ok(refund) => {
                    let _ = state
                        .db
                        .record_hyperswitch_refund(
                            deposit_id,
                            Some(&refund.refund_id),
                            &refund.status,
                        )
                        .await;
                }
                Err(error) => {
                    tracing::error!(%deposit_id, error = %error.0,
                        "the switch refused the refund");
                    let _ = state
                        .db
                        .record_hyperswitch_refund(deposit_id, None, "failed")
                        .await;
                }
            }
            // Re-read the row so the answer carries the refund standing the
            // call just wrote.
            if let Ok(Some(fresh)) = state.db.admin_deposit(deposit_id).await {
                deposit = fresh;
            }
        }
    }
    record(
        &state,
        audit_action::DEPOSIT_REVERSE,
        Some(deposit_id.to_string()),
        serde_json::json!({
            "organizationId": deposit.organization_id,
            "amountMinor": deposit.amount_minor,
            "reason": request.reason,
            "reversalEntryId": deposit.reversal_entry_id,
            "refundStatus": deposit.refund_status,
        }),
        // The reversal's own idempotency anchor is the deposit id: a retried
        // call is the same logical event, which the audit unique index absorbs.
        Some(&format!("deposit-reversal/{deposit_id}")),
    )
    .await?;
    ok(DepositRes::from(deposit))
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

/// The anomalies query string: a band to filter to, and which order to walk in.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AnomaliesQuery {
    /// `low`/`medium`/`high` keeps the rows of one band; absent lists them all.
    severity: Option<String>,
    /// `score` orders by score descending; absent (or `date`) stays newest first.
    sort: Option<String>,
}

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
    /// The turn's severity 0–100 and the band it falls into (issue #263) —
    /// derived at read time, never an input to what was charged.
    score: u8,
    severity: &'static str,
}

/// The settled turns that did not price cleanly, across all organizations — read
/// back from each ledger's own settlement records, so every row is exactly what
/// that turn's bill proves. Each carries a score and severity band derived at
/// read time (issue #263): the kind's base weighted by the charge ratio, the
/// upstream-cost divergence, the failover count and the request id's repeat
/// count — never an input to what was charged or written.
pub(crate) async fn anomalies(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<AnomaliesQuery>,
) -> ApiResult<Vec<AnomalyRes>> {
    // A severity filter that names no band is a validation error, not an empty list.
    let severity_filter = match query.severity.as_deref() {
        None => None,
        Some(s @ ("low" | "medium" | "high")) => Some(s),
        Some(other) => {
            return Err(ApiError::Validation(format!(
                "severity must be low, medium or high, not {other:?}"
            )));
        }
    };
    let sort_by_score = match query.sort.as_deref() {
        None | Some("date") => false,
        Some("score") => true,
        Some(other) => {
            return Err(ApiError::Validation(format!(
                "sort must be score or date, not {other:?}"
            )));
        }
    };

    let mut answer = Vec::new();
    for organization in state.db.organizations().await? {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        // The anomalous turns' records first, then one batched read of the usage
        // rows' cost and repeat counts to score against.
        let mut records = Vec::new();
        for turn in wallet.recent_settlements(100).await? {
            if ANOMALOUS.contains(&turn.record.kind) {
                records.push(turn);
            }
        }
        let request_ids: Vec<String> = records.iter().map(|t| t.record.request.clone()).collect();
        let inputs = state.db.anomaly_inputs(&request_ids).await?;
        for turn in records {
            let (upstream_cost_minor, prior) = inputs
                .get(&turn.record.request)
                .copied()
                .unwrap_or((None, 0));
            let (score, severity) = anomaly_score(&AnomalyFacts {
                kind: Some(turn.record.kind),
                charged_minor: turn.record.charged,
                freeze_minor: turn.record.freeze,
                upstream_cost_minor,
                upstream_attempts: turn.record.upstream_attempts,
                prior_anomalies: prior,
            });
            let band = severity.as_str();
            if severity_filter.is_some_and(|f| f != band) {
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
                score,
                severity: band,
            });
        }
    }
    if sort_by_score {
        answer.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| b.booked_on.cmp(&a.booked_on))
        });
    } else {
        // Booking dates are days, so within one day the per-ledger order stands;
        // across days newest first.
        answer.sort_by(|a, b| b.booked_on.cmp(&a.booked_on));
    }
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
    /// The channel's cumulative provider-billed total — channel-scoped, so the
    /// same figure repeats on each of that channel's model rows; null when the
    /// bill pass has never reconciled the channel (issue #283).
    provider_billed_minor: Option<i64>,
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
            provider_billed_minor: row.provider_billed_minor,
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
