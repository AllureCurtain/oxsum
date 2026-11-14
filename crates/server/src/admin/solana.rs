//! `/api/v1/admin/solana`: the rail's operator exception queue and its
//! pull-payment refund lifecycle (issue #1142), plus the vault
//! reconciliation surface (issue #1149): a daily CSV export of every solana
//! deposit for line-by-line matching against the multisig vault, and a
//! read-only scan that walks `payTo`'s on-chain signature history and
//! reports the transfers no recorded deposit accounts for — the
//! "money arrived with no intent" report.
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
//! the row (`refund-complete`) — but only once the signature verifies
//! on-chain against the refund's frozen amount/address/mint (issue #1175).
//! No ledger entry ever posts on this path: an
//! `underpaid`/`late`/`flagged` deposit never credited, so the refund simply
//! returns what sits in `payTo`; an `overpaid` one's `entry_id` still proves
//! the opened credit while only the `delivered − expected` surplus goes back
//! the same way.

use axum::extract::{Path, State};
use axum::http::HeaderValue;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::AppState;
use crate::admin::record;
use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok, parse_day};

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

/// `POST /api/v1/admin/solana/{depositId}/refund-approve` (issues #1142,
/// #1184): approves an open refund — the review verdict that tells the
/// multisig signer the payout may be signed. The customer must have named
/// their refund address first: an approval with nowhere to pay is `409`, an
/// already-approved refund replays.
///
/// Before the approval is recorded the standing `refundAddress` is screened
/// through the same `SanctionsScreen` the payer check runs (issue #1184): a
/// `Flagged` hit refuses `409` — the refund cannot pay a sanctioned address,
/// so the operator's options are the deposit staying `refund_pending` or a
/// separately documented compliance exception, never an approve-and-hope.
/// A `TemporarilyUnavailable` screen refuses `403` — deny-until-cleared,
/// the same posture the pre-credit screen takes. When the deployment has no
/// `OXSUM_SOLANA_SANCTIONS_*` gate the screen is a no-op `Clear` and the
/// approve is exactly as before.
pub(crate) async fn approve_refund(
    State(state): State<AppState>,
    Path(deposit_id): Path<Uuid>,
) -> ApiResult<oxsum_core::SolanaExceptionDeposit> {
    // Read the standing refund address, screen it, then approve bound to
    // that same address — a customer edit landing between the screen and
    // the approval is a `Conflict` inside `solana_approve_refund`, never a
    // silently redirected payout. An already-approved refund replays without
    // a screen — the freeze already happened; re-screening the replay would
    // refuse what the first approval legitimately stands behind.
    let standing = state.db.solana_exception_deposit(deposit_id).await?;
    let already_approved = standing
        .refund
        .as_ref()
        .is_some_and(|r| r["approvedAt"].is_string());
    let screened = if !already_approved && let Some(address) = standing.refund_address.as_deref() {
        if let Some(verifier) = state.config.solana()
            && verifier.has_sanctions_screen()
        {
            match verifier.screen_address(address).await {
                oxsum_core::ScreenVerdict::Clear => Some(address.to_owned()),
                oxsum_core::ScreenVerdict::Flagged { source, reason } => {
                    return Err(ApiError::Conflict(format!(
                        "the refund address is flagged by {source} ({reason}); the refund cannot be approved"
                    )));
                }
                oxsum_core::ScreenVerdict::TemporarilyUnavailable => {
                    return Err(ApiError::Forbidden(
                        "the refund address could not be screened; the approval waits".into(),
                    ));
                }
            }
        } else {
            None
        }
    } else {
        None
    };
    let deposit = state
        .db
        .solana_approve_refund(deposit_id, OPERATOR, screened.as_deref())
        .await?;
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

/// The body of `POST …/solana/{depositId}/screening-clear`: why the operator
/// cleared the flagged deposit — the review note the audit entry and
/// `meta.screening.clearReason` carry. Required: a clear with no recorded
/// reasoning is not a review.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ScreeningClearReq {
    reason: String,
}

/// `POST /api/v1/admin/solana/{depositId}/screening-clear` (issue #1184):
/// the review-then-credit path for a `flagged` deposit — the operator's
/// manual-review verdict that the parked payment may credit after all.
/// This is manual review, never a bypass: only a `flagged` row answers, the
/// credit books the already-verified transfer under the same
/// `solana:<signature>` idempotency key and the same `FOR UPDATE` row
/// discipline the ordinary credit path takes, and `meta.screening`
/// attributes the clear (`clearedBy`/`clearedAt`/`clearReason`) beside the
/// flag's own verdict. A replayed clear answers the standing credited row;
/// a non-`flagged` deposit is `409`, an unknown one `404`, and a flagged
/// row that never recorded its paying signature cannot credit — `409`, the
/// refund path is how that row unwinds. A deployment without
/// `OXSUM_SOLANA_*` answers 503: the rail's admin surface is the rail's.
pub(crate) async fn screening_clear(
    State(state): State<AppState>,
    Path(deposit_id): Path<Uuid>,
    ApiJson(request): ApiJson<ScreeningClearReq>,
) -> ApiResult<oxsum_core::SolanaExceptionDeposit> {
    state.config.solana().ok_or_else(|| {
        ApiError::ServiceUnavailable("the solana deposit rail is not configured".into())
    })?;
    let deposit = state
        .db
        .solana_screening_clear(
            &state.tenants,
            deposit_id,
            OPERATOR,
            &request.reason,
            crate::today(),
        )
        .await?;
    record(
        &state,
        oxsum_core::audit_action::SOLANA_SCREENING_CLEAR,
        Some(deposit_id.to_string()),
        serde_json::json!({
            "organizationId": deposit.organization_id,
            "asset": deposit.asset,
            "status": deposit.status,
            "signature": deposit.signature,
            "entryId": deposit.entry_id,
            "reason": request.reason,
        }),
        Some(&format!("solana-screening-clear/{deposit_id}")),
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
/// terminal — but only once the signature proves the payout on-chain (issue
/// #1175): the transaction must exist, have succeeded, and have delivered at
/// least the refund's frozen amount in the frozen asset to the recorded
/// refund address. A signature that proves nothing is `409` and the row
/// stays `refund_pending`. The payout already happened — the server holds no
/// spend key, so this is bookkeeping and audit, never a send. `400` on a
/// malformed signature, `409` before approval, on a signature that does not
/// verify, or with a contradicting signature on a `refunded` row; a replayed
/// complete with the standing signature answers the closed row.
pub(crate) async fn complete_refund(
    State(state): State<AppState>,
    Path(deposit_id): Path<Uuid>,
    ApiJson(request): ApiJson<RefundCompleteReq>,
) -> ApiResult<oxsum_core::SolanaExceptionDeposit> {
    let verifier = state.config.solana().ok_or_else(|| {
        ApiError::ServiceUnavailable("the solana deposit rail is not configured".into())
    })?;
    let deposit = state
        .db
        .solana_complete_refund(deposit_id, &request.signature, verifier)
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

// ---------------------------------------------------------------------------
// The reconciliation surface (issue #1149): the daily CSV export and the
// unrecorded-on-chain scan. Both are read-only — the CSV describes recorded
// deposits, the scan describes the chain; neither moves money.
// ---------------------------------------------------------------------------

/// The CSV export's row cap: a day's worth of deposits is far smaller in any
/// realistic deployment, and past the cap the export *refuses* — a `400`
/// telling the operator to narrow the window, the envelope this surface has —
/// rather than ship a truncated file a reconciler would read as complete.
/// The window can always be narrowed.
const MAX_CSV_ROWS: i64 = 50_000;

/// The scan's walk bound: how many `payTo` signatures one request reads at
/// most. Each costs one `getTransaction`, so the bound is the walk's
/// politeness budget — a vault with a longer history runs the scan again
/// after investigating, or waits for the next pass.
const MAX_SCAN_SIGNATURES: usize = 1_000;
const DEFAULT_SCAN_SIGNATURES: usize = 200;

/// `depositId,organizationId,paymentRef,signature,payer,asset,amountMinor,
/// delivered,expected,rateMicro,receivedMinor,status,createdAt,
/// refundSignature,refundStatus,refundAmount` — the column freeze
/// `openapi.yaml` records and this array is the single source of.
/// `refundAmount` rides last (issue #1175): the column a refund-reconciling
/// reader looks for, appended where an older reader's fixed-width slice stays
/// intact.
const CSV_FIELDS: &[&str] = &[
    "depositId",
    "organizationId",
    "paymentRef",
    "signature",
    "payer",
    "asset",
    "amountMinor",
    "delivered",
    "expected",
    "rateMicro",
    "receivedMinor",
    "status",
    "createdAt",
    "refundSignature",
    "refundStatus",
    "refundAmount",
];

/// The export's query string: `from`/`to` are ISO calendar dates (`YYYY-MM-DD`),
/// the window `[from, to]` inclusive — `to` is a *day*, not an instant, so the
/// SQL bound is `to + 1 day`. Omitted, the window is the last complete UTC day
/// (yesterday): the file a daily reconciliation job asks for. Both or neither:
/// `from` without `to` (or the reverse) is a `400`, not a silently different
/// window.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DepositsCsvQuery {
    from: Option<String>,
    to: Option<String>,
}

/// The reconciliation window `[from, to]` as instants (`from` 00:00 UTC to
/// `to + 1 day` 00:00 UTC). `Ok((from_instant, to_instant, from_day, to_day))`
/// — the day pair rides along for the filename.
fn csv_window(
    query: &DepositsCsvQuery,
) -> Result<(OffsetDateTime, OffsetDateTime, time::Date, time::Date), ApiError> {
    match (&query.from, &query.to) {
        (None, None) => {
            let yesterday = crate::today() - time::Duration::days(1);
            let from = yesterday.midnight().assume_utc();
            Ok((from, from + time::Duration::days(1), yesterday, yesterday))
        }
        (Some(from), Some(to)) => {
            let from_day = parse_day(from)?;
            let to_day = parse_day(to)?;
            if to_day < from_day {
                return Err(ApiError::Validation(
                    "the window's `to` is before its `from`".into(),
                ));
            }
            let from_instant = from_day.midnight().assume_utc();
            // `to` bounds inclusively: the SQL upper bound is its *next* day.
            // A `to` of Date::MAX has none — the same shape a `to < from`
            // window takes: refuse, never panic.
            let to_instant = to_day
                .next_day()
                .ok_or_else(|| ApiError::Validation("the window's `to` overflows".into()))?
                .midnight()
                .assume_utc();
            Ok((from_instant, to_instant, from_day, to_day))
        }
        _ => Err(ApiError::Validation(
            "`from` and `to` come as a pair, or neither".into(),
        )),
    }
}

/// `GET /api/v1/admin/solana/reconciliation/deposits.csv` (issue #1149): the
/// daily deposit export — every solana deposit created inside the window,
/// oldest first, as RFC-4180 CSV for line-by-line reconciliation against the
/// vault. `?from=&to=` bound the window (default: the last complete UTC day).
/// Over `MAX_CSV_ROWS` the answer is `400`, never a truncated file: a partial
/// CSV reconciles as "some deposits never existed", the worst lie an export
/// can tell — the operator narrows the window and retries.
pub(crate) async fn deposits_csv(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<DepositsCsvQuery>,
) -> Result<Response, ApiError> {
    // An unconfigured rail still has deposits of other rails, but none named
    // solana — the export would answer a header alone. The rail's own flag is
    // the honest refusal, like the rest of the solana surface.
    state.config.solana().ok_or_else(|| {
        ApiError::ServiceUnavailable("the solana deposit rail is not configured".into())
    })?;
    let (from, to, from_day, to_day) = csv_window(&query)?;
    // `limit + 1` detects truncation: one extra row means the window held
    // more than the cap and the export refuses rather than truncate.
    let rows = state
        .db
        .solana_deposits_window(from, to, MAX_CSV_ROWS + 1)
        .await?;
    if rows.len() as i64 > MAX_CSV_ROWS {
        return Err(ApiError::Validation(format!(
            "the window holds over {MAX_CSV_ROWS} solana deposits; narrow `from`/`to` and retry"
        )));
    }
    let mut body = CSV_FIELDS.join(",");
    body.push('\n');
    for row in &rows {
        body.push_str(&csv_row(row));
        body.push('\n');
    }
    let filename = format!("solana-deposits-{from_day}-{to_day}.csv");
    let mut response = body.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

/// One deposit as a CSV row. Column provenance:
///
/// - `signature` — `facilitator_tx_hash` (or `meta.signature`), the paying
///   transaction.
/// - `payer` — `meta.payerAddress`, which every terminal write merges from
///   the verify context (issue #1186), falling back to
///   `meta.screening.address` on a `flagged` row parked before the merge
///   landed; empty on a `pending` row that never recorded one.
/// - `delivered`/`expected` — the locked asset's own base units
///   (`meta.delivered`, `meta.expected`/`meta.solLamports`).
/// - `rateMicro` — the intent's locked `meta.rateMicro`.
/// - `refundAmount` — the payout's frozen figure in the locked asset's base
///   units (`meta.refund.amount`, issue #1175): `delivered − expected` on
///   `overpaid`, `delivered` on the others; empty while no refund is open.
/// - `refundStatus` — the pull-payment lifecycle derived from `status` +
///   `meta.refund`: `requested` (refund_pending), `approved` (refund_pending
///   with `approvedAt`), `completed` (refunded), empty otherwise.
fn csv_row(row: &oxsum_core::SolanaExceptionDeposit) -> String {
    let opt = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_default();
    let refund_status = match row.status.as_str() {
        "refunded" => "completed",
        "refund_pending" => {
            if row
                .refund
                .as_ref()
                .is_some_and(|r| r["approvedAt"].is_string())
            {
                "approved"
            } else {
                "requested"
            }
        }
        _ => "",
    };
    let created_at = row
        .created_at
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let cells = [
        row.deposit_id.to_string(),
        row.organization_id.to_string(),
        row.payment_ref.clone(),
        row.signature.clone().unwrap_or_default(),
        row.payer.clone().unwrap_or_default(),
        row.asset.clone().unwrap_or_default(),
        row.amount_minor.to_string(),
        opt(row.delivered),
        opt(row.expected),
        row.rate_micro.map(|n| n.to_string()).unwrap_or_default(),
        opt(row.received_minor),
        row.status.clone(),
        created_at,
        row.refund_signature.clone().unwrap_or_default(),
        refund_status.to_owned(),
        opt(row.refund_amount),
    ];
    cells.map(|cell| csv_field(&cell)).join(",")
}

/// One CSV field, RFC-4180: quoted only when a comma, a quote or a line break
/// would otherwise shift the row it sits in; its own quotes doubled.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// The scan request body: how many of `payTo`'s most recent signatures to
/// walk (bounded), the window the operator chose. Omitted, a conservative
/// page — the walk fetches one `getTransaction` per candidate, so the bound
/// is the RPC politeness budget.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ScanReq {
    limit: Option<usize>,
}

/// One on-chain transfer the scan could not match to a recorded deposit —
/// the "money arrived with no intent" row the operator investigates.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UnmatchedTransfer {
    signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    block_time: Option<i64>,
    asset: Option<String>,
    delivered: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    payer: Option<String>,
    /// Always `false` — the row exists precisely because no intent recorded
    /// the signature; the field names the report's semantics.
    matched_intent: bool,
}

/// The scan's answer (issue #1188): three distinct figures the old report
/// conflated — how much history was walked, how many paying transfers it
/// found, which of them recorded no deposit — plus the bound signal.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanReport {
    /// How many `payTo` signature-history entries the walk actually
    /// inspected — one `getTransaction` each, bounded at `limit`. NOT the
    /// credited count (that was the bug: `scanned` used to read
    /// `signatures.len()`, which was the filtered transfers found).
    scanned: usize,
    /// How many of the walked transactions actually delivered value into the
    /// vault in a whitelisted asset — one per (signature, asset) leg, so a
    /// transaction paying SOL + an SPL mint counts twice.
    credited: usize,
    /// The credited transfers that match no
    /// `deposits.facilitator_tx_hash` — each needs an operator's eyes;
    /// `unmatched.len()` is their count.
    unmatched: Vec<UnmatchedTransfer>,
    /// Whether the walk stopped at `limit` while older history may exist
    /// beyond the bound — `false` when the signature history ran out first.
    limit_reached: bool,
}

/// `POST /api/v1/admin/solana/reconciliation/scan` (issue #1149): walks
/// `payTo`'s on-chain signature history (bounded at `limit`, default 200,
/// max 1000), filters to transactions that actually delivered value into the
/// vault in a whitelisted asset, and reports each whose signature no
/// `deposits.facilitator_tx_hash` records — the deposits the rail never
/// opened an intent for. A synchronous bounded call, not a job: the endpoint
/// pool the walk runs over already fails over, and a POST + audit row is how
/// the operator's action is recorded.
///
/// Read-only by construction: the scan diffs the chain against the table and
/// *reports* — it never credits, never marks a deposit, never moves money.
/// An unmatched row is the manual-investigation trigger: the operator
/// confirms the transfer on the vault and walks the refund/intent path by
/// hand.
pub(crate) async fn scan(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ScanReq>,
) -> ApiResult<ScanReport> {
    let verifier = state.config.solana().ok_or_else(|| {
        ApiError::ServiceUnavailable("the solana deposit rail is not configured".into())
    })?;
    let limit = request
        .limit
        .unwrap_or(DEFAULT_SCAN_SIGNATURES)
        .clamp(1, MAX_SCAN_SIGNATURES);
    // The chain walk — `payTo`'s inbound transfers, bounded. `walked` is the
    // signatures inspected, `credited` the paying transfers among them
    // (one row per asset leg), `history_bounded` the truncation signal.
    let walk = verifier.scan_received(limit).await;
    let signatures: Vec<String> = walk.credited.iter().map(|t| t.signature.clone()).collect();
    // Diff against the recorded deposits: an on-chain signature that paid
    // the vault but appears nowhere is money that arrived with no intent.
    let recorded = state.db.solana_recorded_signatures(&signatures).await?;
    let unmatched = walk
        .credited
        .iter()
        .filter(|t| !recorded.contains(&t.signature))
        .map(|t| UnmatchedTransfer {
            signature: t.signature.clone(),
            block_time: t.block_time,
            asset: t.asset.clone(),
            delivered: t.delivered,
            payer: t.payer.clone(),
            matched_intent: false,
        })
        .collect::<Vec<_>>();
    record(
        &state,
        oxsum_core::audit_action::SOLANA_RECONCILE_SCAN,
        None,
        serde_json::json!({
            "scanned": walk.walked,
            "credited": walk.credited.len(),
            "unmatched": unmatched.len(),
            "limit": limit,
            "limitReached": walk.history_bounded,
        }),
        Some(&format!("solana-reconcile-scan/{}", Uuid::new_v4())),
    )
    .await?;
    ok(ScanReport {
        scanned: walk.walked,
        credited: walk.credited.len(),
        unmatched,
        limit_reached: walk.history_bounded,
    })
}

/// Only the tests below reach in.
#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    /// RFC-4180: a field with a comma, a quote or a line break is quoted and
    /// its own quotes doubled; every other field passes through verbatim.
    /// A CSV whose fields could shift columns would lie to the reconciler
    /// reading it line by line.
    #[test]
    fn a_csv_field_quotes_only_when_it_must() {
        assert_eq!(super::csv_field("plain"), "plain");
        assert_eq!(super::csv_field("has,comma"), "\"has,comma\"");
        assert_eq!(
            super::csv_field("has\"quote"),
            "\"has\"\"quote\"",
            "a quote doubles inside the quoting"
        );
        assert_eq!(super::csv_field("has\nnewline"), "\"has\nnewline\"");
        assert_eq!(super::csv_field("has\rcr"), "\"has\rcr\"");
    }
}
