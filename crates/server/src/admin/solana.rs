//! `/api/v1/admin/solana`: the rail's operator exception queue and its
//! pull-payment refund lifecycle (issue #1142).
//!
//! The queue lists the deposits the verify path marked — `underpaid`,
//! `overpaid`, `late`, `flagged` — plus the refund lifecycle's own states
//! (`refund_pending`, `refunded`), filterable by status. Each row names its
//! `asset` — the whitelist key the intent locked — so an operator reads which
//! denomination `delivered`/`expected` count in and which one the refund
//! repays. A refund is a pull payment, never an auto-send: the operator opens
//! it (`refund`), the customer names the refund address on their own surface,
//! the operator approves (`refund-approve`), the multisig pays out-of-band —
//! the server holds no spend key — and the recorded payout signature closes
//! the row (`refund-complete`). No ledger entry ever posts on this path: an
//! `underpaid`/`late`/`flagged` deposit never credited, so the refund simply
//! returns what sits in `payTo`; an `overpaid` one's `entry_id` still proves
//! the opened credit while the surplus goes back the same way.

use axum::extract::{Path, State};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::AppState;
use crate::admin::record;
use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};

/// The operator identity an admin write records — the token has no name, so
/// `meta.refund` attributes the lifecycle to the role, and the audit row
/// carries the detail.
const OPERATOR: &str = "operator";

/// The exception queue's query string: one status to narrow to, how many
/// rows, and where the keyset walk resumes.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExceptionsQuery {
    status: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

/// The cursor the queue pages by, encoded `<unix nanos>:<deposit id>` — the
/// `(created_at, deposit_id)` pair the keyset orders on, opaque to the
/// caller: echoed, never constructed.
fn encode_cursor((created_at, id): (OffsetDateTime, Uuid)) -> String {
    format!("{}:{}", created_at.unix_timestamp_nanos(), id)
}

fn decode_cursor(text: &str) -> Result<(OffsetDateTime, Uuid), ApiError> {
    let bad = || ApiError::Validation("a malformed solana exceptions cursor".into());
    let (nanos, id) = text.split_once(':').ok_or_else(bad)?;
    let nanos: i128 = nanos.parse().map_err(|_| bad())?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    let id: Uuid = id.parse().map_err(|_| bad())?;
    Ok((at, id))
}

/// One page of queue rows, as the endpoint answers it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExceptionsPage {
    entries: Vec<oxsum_core::SolanaExceptionDeposit>,
    /// The cursor the next page asks with; absent at the queue's end.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// `GET /api/v1/admin/solana/exceptions` (issue #1142): the rail's exception
/// queue — deposits in `underpaid`/`overpaid`/`late`/`flagged` plus the
/// refund lifecycle's `refund_pending`/`refunded`, newest first. `status`
/// narrows to one; an unknown status is a `400`, not an empty page
/// masquerading as quiet. Read-only: nothing here moves money.
pub(crate) async fn exceptions(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<ExceptionsQuery>,
) -> ApiResult<ExceptionsPage> {
    if let Some(status) = &query.status
        && !oxsum_core::solana_status::EXCEPTIONS.contains(&status.as_str())
    {
        return Err(ApiError::Validation(format!(
            "unknown exception status: {status:?}"
        )));
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let after = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let rows = state
        .db
        .solana_exception_deposits(query.status.as_deref(), limit + 1, after)
        .await?;
    let next_cursor = if rows.len() as i64 > limit {
        rows.get(limit as usize - 1)
            .map(|row| encode_cursor((row.created_at, row.deposit_id)))
    } else {
        None
    };
    ok(ExceptionsPage {
        entries: rows.into_iter().take(limit as usize).collect(),
        next_cursor,
    })
}

/// The body of `POST …/solana/{depositId}/refund`: why the refund is opened —
/// the reason the queue row and the audit entry carry.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RefundReq {
    reason: String,
}

/// `POST /api/v1/admin/solana/{depositId}/refund` (issue #1142): opens a
/// pull-payment refund on a `underpaid`/`overpaid`/`late`/`flagged` deposit
/// — status `refund_pending`, `meta.refund` recording the operator, the time
/// and the reason. Re-requesting an open refund replays the standing row; a
/// `refunded` or non-refundable deposit is `409`, an unknown one `404`.
/// Nothing moves on-chain: the multisig pays out-of-band by design.
pub(crate) async fn request_refund(
    State(state): State<AppState>,
    Path(deposit_id): Path<Uuid>,
    ApiJson(request): ApiJson<RefundReq>,
) -> ApiResult<oxsum_core::SolanaExceptionDeposit> {
    let deposit = state
        .db
        .solana_request_refund(deposit_id, OPERATOR, &request.reason)
        .await?;
    record(
        &state,
        oxsum_core::audit_action::SOLANA_REFUND_REQUEST,
        Some(deposit_id.to_string()),
        serde_json::json!({
            "organizationId": deposit.organization_id,
            "status": deposit.status,
            "asset": deposit.asset,
            "reason": request.reason,
        }),
        // The refund's anchor is the deposit id: a retried request replays
        // the standing row, and the audit unique index absorbs it.
        Some(&format!("solana-refund/{deposit_id}")),
    )
    .await?;
    ok(deposit)
}

/// `POST /api/v1/admin/solana/{depositId}/refund-approve` (issue #1142):
/// approves an open refund — the review verdict that tells the multisig
/// signer the payout may be signed. The customer must have named their
/// refund address first: an approval with nowhere to pay is `409`, an
/// already-approved refund replays.
pub(crate) async fn approve_refund(
    State(state): State<AppState>,
    Path(deposit_id): Path<Uuid>,
) -> ApiResult<oxsum_core::SolanaExceptionDeposit> {
    let deposit = state.db.solana_approve_refund(deposit_id, OPERATOR).await?;
    record(
        &state,
        oxsum_core::audit_action::SOLANA_REFUND_APPROVE,
        Some(deposit_id.to_string()),
        serde_json::json!({
            "organizationId": deposit.organization_id,
            "asset": deposit.asset,
            "refundAddress": deposit.refund_address,
        }),
        Some(&format!("solana-refund-approve/{deposit_id}")),
    )
    .await?;
    ok(deposit)
}

/// The body of `POST …/solana/{depositId}/refund-complete`: the multisig
/// payout's on-chain signature — the refund's proof.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RefundCompleteReq {
    signature: String,
}

/// `POST /api/v1/admin/solana/{depositId}/refund-complete` (issue #1142):
/// records the payout's signature and closes the deposit `refunded`,
/// terminal. The payout already happened — the server holds no spend key, so
/// this is bookkeeping and audit, never a send. `400` on a malformed
/// signature, `409` before approval or with a contradicting signature on a
/// `refunded` row; a replayed complete with the standing signature answers
/// the closed row.
pub(crate) async fn complete_refund(
    State(state): State<AppState>,
    Path(deposit_id): Path<Uuid>,
    ApiJson(request): ApiJson<RefundCompleteReq>,
) -> ApiResult<oxsum_core::SolanaExceptionDeposit> {
    let deposit = state
        .db
        .solana_complete_refund(deposit_id, &request.signature)
        .await?;
    record(
        &state,
        oxsum_core::audit_action::SOLANA_REFUND_COMPLETE,
        Some(deposit_id.to_string()),
        serde_json::json!({
            "organizationId": deposit.organization_id,
            "asset": deposit.asset,
            "refundSignature": request.signature,
        }),
        Some(&format!("solana-refund-complete/{deposit_id}")),
    )
    .await?;
    ok(deposit)
}
