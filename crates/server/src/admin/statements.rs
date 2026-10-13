//! `/api/v1/admin/statements` and `/closings`: the monthly close and the
//! per-organization statements it produces, through finalize, suspend and pay.

use axum::extract::{Path, State};
use axum::response::Response;
use oxsum_core::{Seal, audit_action};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use crate::{
    AppState,
    admin::{parse_month, record},
    today,
};

/// One organization's closing record for a sealed period, as the closings endpoint
/// answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClosingRes {
    organization: String,
    /// The sealed period, `YYYY-MM`.
    period: String,
    /// How many of the ledger's entries the period contains.
    entry_count: i64,
    /// The log's size at the moment of sealing.
    tree_size: i64,
    /// The log's tree head at sealing, hex — what the month committed to.
    tree_root: String,
    /// The Merkle root over the period's closing trial balance, hex.
    trial_balance_root: String,
    /// The seal's own hash, hex; it chains onto the seal before it.
    seal_hash: String,
}

impl ClosingRes {
    fn of(organization: &str, seal: &Seal) -> Self {
        ClosingRes {
            organization: organization.to_owned(),
            period: seal.period.to_string(),
            entry_count: seal.entry_count as i64,
            tree_size: seal.tree_head.size as i64,
            tree_root: seal.tree_head.root.to_string(),
            trial_balance_root: seal.trial_balance.root.to_string(),
            seal_hash: seal.seal_hash.to_string(),
        }
    }
}

/// Every sealed month across all organizations, newest first: the closing records.
pub(crate) async fn closings(State(state): State<AppState>) -> ApiResult<Vec<ClosingRes>> {
    let mut answer = Vec::new();
    for organization in state.db.organizations().await? {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        for seal in wallet.seals().await? {
            answer.push(ClosingRes::of(&organization.name, &seal));
        }
    }
    // Period ids sort as the months do (`YYYY-MM`), so newest first is a string sort.
    answer.sort_by(|a, b| b.period.cmp(&a.period));
    ok(answer)
}

/// The month to close, `YYYY-MM`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CloseReq {
    month: String,
}

/// Closes one month across every organization's ledger and answers each closing
/// record — the seal it appended.
///
/// The month must have fully ended, which `Wallet::close_month` enforces; closing
/// an already-sealed month answers the record it holds, so the call is idempotent.
pub(crate) async fn close_month(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CloseReq>,
) -> ApiResult<Vec<ClosingRes>> {
    let first = parse_month(&request.month)?;
    let mut answer = Vec::new();
    for organization in state.db.organizations().await? {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        let seal = wallet.close_month(first).await?;
        answer.push(ClosingRes::of(&organization.name, &seal));
    }
    record(
        &state,
        audit_action::CLOSING_CLOSE,
        Some(request.month.clone()),
        serde_json::json!({ "organizations": answer.len() }),
        None,
    )
    .await?;
    ok(answer)
}

/// The optional filters of `GET /admin/statements`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StatementsQuery {
    period: Option<String>,
    organization_id: Option<Uuid>,
}

/// Every statement the platform has generated, newest period first — drafts and
/// finalized alike, optionally narrowed by `period` or `organizationId`. The lazy
/// overdue flip runs first, so a pending statement past its due date answers
/// `overdue` already.
pub(crate) async fn statements(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<StatementsQuery>,
) -> ApiResult<Vec<oxsum_core::Statement>> {
    // The filter names a billing month; a malformed or still-running one is a
    // validation failure rather than an empty list, so a mistyped period does
    // not silently look like "no statements".
    if let Some(period) = &query.period {
        oxsum_core::statement_period(period)?;
    }
    state.db.flip_overdue(today()).await?;
    ok(state
        .db
        .statements(query.organization_id, query.period.as_deref())
        .await?)
}

/// The body of `POST /admin/statements`: the month to bill, and optionally the one
/// organization to bill it for.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GenerateStatementsReq {
    period: String,
    organization_id: Option<Uuid>,
}

/// Builds the period's draft statements — for the named organization, or for every
/// organization the period's usage covers when `organizationId` is absent.
///
/// Generation is idempotent per organization and period: a draft is rebuilt from
/// the usage rows each call, so a usage row that landed after generation is picked
/// up by regenerating; a statement already finalized stands and is answered as it
/// is. Only a month that has fully ended generates.
pub(crate) async fn generate_statements(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<GenerateStatementsReq>,
) -> ApiResult<Vec<oxsum_core::Statement>> {
    let period = oxsum_core::statement_period(&request.period)?;
    let organizations = match request.organization_id {
        Some(id) => vec![state.db.organization_by_id(id).await?],
        None => state.db.billable_organizations().await?,
    };
    let mut answer = Vec::with_capacity(organizations.len());
    for organization in &organizations {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        if let Some(statement) = state
            .db
            .generate_statement(organization, &wallet, &period)
            .await?
        {
            answer.push(statement);
        }
    }
    record(
        &state,
        audit_action::STATEMENT_GENERATE,
        Some(request.period.clone()),
        serde_json::json!({
            "organizationId": request.organization_id,
            "statements": answer.len(),
        }),
        None,
    )
    .await?;
    ok(answer)
}

/// A statement and its itemized lines, as the detail endpoints answer it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatementDetailRes {
    statement: oxsum_core::Statement,
    lines: Vec<oxsum_core::StatementLine>,
    anchor: Option<crate::routes::billing::StatementAnchorRes>,
}

/// One statement with its lines, whatever standing it is in — the admin view of
/// the document.
pub(crate) async fn statement(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
) -> ApiResult<StatementDetailRes> {
    state.db.flip_overdue(today()).await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let organization = state
        .db
        .organization_by_id(statement.organization_id)
        .await?;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let anchor = crate::routes::billing::statement_anchor(&state, &wallet, &statement).await?;
    ok(StatementDetailRes {
        lines: state.db.statement_lines(statement_id).await?,
        statement,
        anchor,
    })
}

/// One finalized statement as a rendered PDF — the admin view of the export the
/// organization itself downloads (issue #205). A draft or a missing id answers
/// 404: an unissued document does not exist to export. The body is binary, not
/// the JSON envelope.
pub(crate) async fn statement_pdf(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    use axum::body::Body;
    state.db.flip_overdue(today()).await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if statement.status != oxsum_core::StatementStatus::Finalized {
        return Err(ApiError::not_found());
    }
    let lines = state.db.statement_lines(statement_id).await?;
    let organization = state
        .db
        .organization_by_id(statement.organization_id)
        .await?;
    let url = match state.config.public_url() {
        Some(base) => format!("{}/verify", base.trim_end_matches('/')),
        None => "/verify".to_owned(),
    };
    let bytes =
        oxsum_core::render_statement_pdf(&statement, &organization.name, &lines, Some(&url))?;
    let filename = format!(
        "statement-{}-{}.pdf",
        statement.period,
        &statement_id.simple().to_string()[..8]
    );
    Response::builder()
        .header("content-type", "application/pdf")
        .header(
            "content-disposition",
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(bytes))
        .map_err(|_| ApiError::Internal)
}

/// The body of the statement transitions: only the idempotency key, carried for
/// uniformity — a lifecycle transition is idempotent on its own.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TransitionReq {
    idempotency_key: String,
}

/// Issues a draft statement: locks its lines and totals, snapshots the
/// organization's payment terms into the due date, and pins the ledger window
/// the lines prove. Idempotent — an already-final statement answers itself.
pub(crate) async fn finalize_statement(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
    ApiJson(request): ApiJson<TransitionReq>,
) -> ApiResult<oxsum_core::Statement> {
    transition_key(&request)?;
    let existing = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let organization = state
        .db
        .organization_by_id(existing.organization_id)
        .await?;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let statement = state
        .db
        .finalize_statement(statement_id, &wallet, today())
        .await?;
    record(
        &state,
        audit_action::STATEMENT_FINALIZE,
        Some(statement_id.to_string()),
        serde_json::json!({ "paymentStatus": statement.payment_status }),
        Some(&request.idempotency_key),
    )
    .await?;
    ok(statement)
}

/// The body of `POST …/statements/{id}/payments`: the amount the organization paid
/// and the idempotency key naming the ledger repayment entry.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecordPaymentReq {
    amount_minor: i64,
    idempotency_key: String,
}

/// A recorded payment, as the endpoint answers it: the repayment's ledger entry
/// and the statement's standing after the money applied.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PaymentRes {
    statement: oxsum_core::Statement,
    entry_id: String,
    amount_minor: i64,
}

/// Records a payment against a statement: books the amount into the
/// organization's ledger — `debit Cash`, repaying the drawn credit line first —
/// then reconciles the statement book, which applies repayments oldest-first.
///
/// The payment may not exceed the credit-line debt outstanding through the
/// statement's period (`400` if it does), so money recorded here always lands
/// on the line. The idempotency key names the ledger entry, which can never
/// book twice: a retried payment while the statement is still open replays the
/// receipt, and one that arrives after the statement paid is refused as
/// `400` — there is nothing left to take.
pub(crate) async fn record_payment(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
    ApiJson(request): ApiJson<RecordPaymentReq>,
) -> ApiResult<PaymentRes> {
    let existing = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let organization = state
        .db
        .organization_by_id(existing.organization_id)
        .await?;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    // Fresh figures first: a payment raced with a top-up could have repaid the
    // statement since it was last read, and the caps below validate against the
    // standing the ledger now shows.
    state
        .db
        .reconcile_statements(organization.id, &wallet, today())
        .await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if statement.status == oxsum_core::StatementStatus::Draft {
        return Err(ApiError::Validation(
            "a draft statement is not issued yet; finalize it first".into(),
        ));
    }
    if statement.payment_status == oxsum_core::PaymentStatus::Paid {
        return Err(ApiError::Validation(
            "the statement is paid; nothing is owed".into(),
        ));
    }
    let owed = state
        .db
        .statement_debt_through(&wallet, &statement.period)
        .await?;
    if request.amount_minor > owed {
        return Err(ApiError::Validation(
            "the payment exceeds the billed debt outstanding through this statement".into(),
        ));
    }
    let receipt = wallet
        .top_up(&request.idempotency_key, request.amount_minor, today())
        .await?;
    state
        .db
        .reconcile_statements(organization.id, &wallet, today())
        .await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    record(
        &state,
        audit_action::STATEMENT_PAYMENT,
        Some(statement_id.to_string()),
        serde_json::json!({
            "amountMinor": request.amount_minor,
            "entryId": receipt.entry_id,
        }),
        Some(&request.idempotency_key),
    )
    .await?;
    ok(PaymentRes {
        statement,
        entry_id: receipt.entry_id.to_string(),
        amount_minor: request.amount_minor,
    })
}

/// Marks an unpaid finalized statement `suspended` — the standing for a bill that
/// has gone unpaid past its grace. A later payment still settles it.
pub(crate) async fn suspend_statement(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
    ApiJson(request): ApiJson<TransitionReq>,
) -> ApiResult<oxsum_core::Statement> {
    transition_key(&request)?;
    // A pending statement whose due date has passed is overdue first — suspending
    // it lands from the standing it actually holds.
    state.db.flip_overdue(today()).await?;
    let statement = state.db.suspend_statement(statement_id).await?;
    record(
        &state,
        audit_action::STATEMENT_SUSPEND,
        Some(statement_id.to_string()),
        serde_json::json!({}),
        Some(&request.idempotency_key),
    )
    .await?;
    ok(statement)
}

/// The key is carried for uniformity, so it is held to the same shape every write
/// key answers to: present, and at most 200 bytes.
fn transition_key(request: &TransitionReq) -> Result<(), ApiError> {
    if request.idempotency_key.is_empty() || request.idempotency_key.len() > 200 {
        return Err(ApiError::Validation("a malformed idempotency key".into()));
    }
    Ok(())
}
