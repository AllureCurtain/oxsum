//! Billing routes: holds, bills, top-ups and x402 payments.

use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use oxsum_core::{Principal, SignedHead, WalletError, signing_key};

use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::hyperswitch::WebhookEvent;
use crate::routes::{ApiResult, ok, verify_url, wallet};
use crate::{AppState, today};

/// The body of `POST /api/v1/redemptions`: the code as minted.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RedeemCodeReq {
    code: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BalanceRes {
    /// Everything the organization can still reserve: own funds plus the
    /// undrawn part of its credit line.
    available_minor: i64,
    /// The credit limit the operator granted; 0 for an organization without one.
    credit_limit_minor: i64,
    /// The drawn plus reserved part of the credit line.
    credit_used_minor: i64,
    /// What outstanding holds freeze right now — the dashboard's frozen total
    /// (issue #234).
    reserved_minor: i64,
    /// What this UTC calendar month has settled so far — the dashboard's
    /// month-spend figure.
    spent_this_month_minor: i64,
}

/// One hold the ledger still reserves, as the dashboard's in-flight table
/// lists it (issue #234).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenHoldRes {
    request_id: String,
    model: String,
    freeze_minor: i64,
    /// The display prefix of the key that took the hold; absent for
    /// unattributed holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    key_prefix: Option<String>,
}

/// `GET /api/v1/holds` answers `{holds: [...]}`.
#[derive(Serialize)]
pub(crate) struct HoldsRes {
    holds: Vec<OpenHoldRes>,
}

/// The holds the ledger still reserves for this organization — the sweeper's
/// watch list, newest first. Members see the organization's holds: the list
/// names the request and the reservation, not another member's keys.
pub(crate) async fn list_holds(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<HoldsRes> {
    let holds = state
        .db
        .open_holds_for_tenant(&principal.organization().tenant_id)
        .await?;
    let keys = state
        .db
        .list_keys(principal.organization().id, principal.key_scope())
        .await?;
    let scope = state.db.customer_record_scope(&principal).await?;
    ok(HoldsRes {
        holds: holds
            .into_iter()
            .filter(|hold| scope.allows(hold.key_id.map(|id| id.simple().to_string()).as_deref()))
            .map(|hold| OpenHoldRes {
                request_id: hold.request_id,
                model: hold.model,
                freeze_minor: hold.freeze_minor,
                key_prefix: hold.key_id.and_then(|id| {
                    keys.iter()
                        .find(|key| key.id == id)
                        .map(|key| key.prefix.clone())
                }),
            })
            .collect(),
    })
}

/// One transaction as the bills page lists it (issue #234): the entry that
/// moved money, with the hash its proof commits to.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BillRowRes {
    index: u64,
    entry_id: String,
    booked_on: String,
    kind: &'static str,
    amount_minor: i64,
    description: String,
    content_hash: String,
}

/// `GET /api/v1/bills` answers `{rows, nextCursor}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BillsRes {
    rows: Vec<BillRowRes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<u64>,
}

/// The transactions query: `before` pages the log backwards.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BillsQuery {
    before: Option<u64>,
}

/// The organization's transactions, newest first — top-ups, adjustments and
/// settled turns, the rows the server-rendered bills page draws and its CSV
/// and JSON exports carry. A member sees their own keys' rows plus the
/// organization's unattributed entries — the same scope rule the requests
/// list applies.
pub(crate) async fn bills(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(query): ApiQuery<BillsQuery>,
) -> ApiResult<BillsRes> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    let scoped = crate::routes::usage::scoped_key_ids(&state, &principal).await?;
    let page = w
        .scoped_transactions_page(query.before, 50, &scoped)
        .await?;
    ok(BillsRes {
        rows: page
            .rows
            .into_iter()
            .map(|row| BillRowRes {
                index: row.index,
                entry_id: row.id,
                booked_on: row.booked_on.to_string(),
                kind: match row.kind {
                    oxsum_core::TransactionKind::TopUp => "topUp",
                    oxsum_core::TransactionKind::Adjustment => "adjustment",
                    oxsum_core::TransactionKind::Settlement => "settlement",
                },
                amount_minor: row.amount_minor,
                description: row.description,
                content_hash: row.content_hash,
            })
            .collect(),
        next_cursor: page.next_cursor,
    })
}

/// Customer credentials may request payment, never assert money received.
pub(crate) async fn top_up() -> ApiResult<oxsum_core::Receipt> {
    Err(ApiError::Forbidden(
        "manual top-ups require deployment authority; use checkout or a redemption code".into(),
    ))
}

/// The body of `POST /api/v1/topups/x402`: the signed payment authorization as
/// base64url, plus the caller-declared credit amount the rail must honor.
/// `amountMinor` is optional — the facilitator verifier builds
/// `paymentRequirements` around it, and without it the amount the
/// authorization itself commits is what credits; the stand-in ignores it
/// either way. The deposit's `payment_ref` anchors on the signed `nonce`, so
/// a replayed authorization credits once.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct X402TopupReq {
    payment: String,
    amount_minor: Option<i64>,
}

/// `POST /api/v1/topups/x402`: the x402-shaped deposit rail (issue #264). The
/// caller presents a signed payment authorization; the verifier proves it is a
/// payment the rail honors, and the deposit credits through the same
/// pending → confirmed → credited walk every rail shares. An unconfigured rail
/// answers 503, an unverified authorization 403 — the seam refuses, the
/// bookkeeping behind it is real. A replayed authorization answers the
/// standing receipt (`is_new: false`), like every credit.
pub(crate) async fn x402_topup(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<X402TopupReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let verifier = state.config.x402().ok_or_else(|| {
        ApiError::ServiceUnavailable("the x402 deposit rail is not configured".into())
    })?;
    let w = wallet(&state.tenants, principal.organization()).await?;
    let receipt = state
        .db
        .credit_x402_deposit(
            &w,
            principal.organization().id,
            verifier,
            r.payment.as_bytes(),
            r.amount_minor,
            today(),
        )
        .await?;
    // Whatever the deposit repaid counts against the statement book, like the
    // manual rail: open statements are repaid oldest first (issue #124).
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    ok(receipt)
}

/// The body of `POST /api/v1/org/subscription/checkout`: which plan to buy on
/// the Stripe rail.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SubscriptionCheckoutReq {
    // Accepted for contract compatibility; the versioned order flow supersedes it.
    #[expect(dead_code)]
    plan_id: Uuid,
}

/// Opens a Stripe subscription checkout for the plan: the org user's browser
/// is sent to the hosted page, and the subscription Stripe mints becomes the
/// rail's renewal source — `invoice.paid` rolls the period forward,
/// `customer.subscription.deleted` ends it (issue #496).
///
/// A replayed intent answers the same session's URL — Stripe's own
/// idempotency decides what one click means, and the row's
/// `stripe_subscription_id` is the webhook's anchor.
///
/// A deployment without `OXSUM_STRIPE_*` answers 503 — the rail is
/// configured or it is not, never silently manual.
pub(crate) async fn subscription_checkout(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<SubscriptionCheckoutReq>,
) -> ApiResult<serde_json::Value> {
    let _ = (state, principal, r);
    Err(ApiError::ServiceUnavailable(
        "paid plan checkout requires the versioned payment-order flow".into(),
    ))
}

/// The body of `POST /api/v1/topups/stripe`: the amount to charge and the
/// idempotency key that anchors the deposit.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StripeTopupReq {
    idempotency_key: String,
    amount_minor: i64,
}

/// The Stripe top-up intent: opens a deposit, asks Stripe for a checkout
/// session and answers the redirect URL the org user's browser follows.
///
/// A replayed intent (same `idempotencyKey`) answers the same checkout URL —
/// the deposit's `payment_ref` is the idempotency anchor.
///
/// A deployment without `OXSUM_STRIPE_*` answers 503 — the rail is configured
/// or it is not, never silently manual.
pub(crate) async fn stripe_topup(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<StripeTopupReq>,
) -> ApiResult<serde_json::Value> {
    ok(crate::merchant_payments::checkout(
        &state,
        principal.organization().id,
        oxsum_core::MerchantRail::Stripe,
        &r.idempotency_key,
        r.amount_minor,
    )
    .await?)
}

/// Stripe's inbound webhook: verifies `Stripe-Signature`, then walks the
/// deposit the event names. Org-agnostic — the signature is the credential,
/// so this route sits on the `open` surface.
///
/// `checkout.session.completed` confirms the deposit and credits the wallet
/// under `stripe:<session_id>`; `checkout.session.expired` closes it. Every
/// other event type, every replay and every miss is a 200 — Stripe retries
/// until it hears one, and a row that does not exist is not this endpoint's
/// business.
pub(crate) async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    if let Some(secret) = state.config.secret()
        && state
            .db
            .environment_merchant(oxsum_core::MerchantRail::Stripe, secret)
            .await?
            .is_some()
    {
        return super::merchant_events::environment(
            &state,
            oxsum_core::MerchantRail::Stripe,
            &headers,
            &body,
        )
        .await;
    }
    stripe_legacy_webhook(State(state), headers, body).await
}

/// The body of `POST /api/v1/topups/airwallex`: the amount to charge and the
/// idempotency key that anchors the deposit.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AirwallexTopupReq {
    idempotency_key: String,
    amount_minor: i64,
}

/// The Airwallex top-up intent (issue #285): opens a deposit, asks Airwallex
/// for a PaymentIntent and answers the hosted-page URL plus the `client_secret`
/// the payer's browser confirms against.
///
/// A replayed intent (same `idempotencyKey`) answers the same URL and secret —
/// the deposit's `payment_ref` is the idempotency anchor.
///
/// A deployment without `OXSUM_AIRWALLEX_*` answers 503 — the rail is
/// configured or it is not, never silently manual.
pub(crate) async fn airwallex_topup(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<AirwallexTopupReq>,
) -> ApiResult<serde_json::Value> {
    ok(crate::merchant_payments::checkout(
        &state,
        principal.organization().id,
        oxsum_core::MerchantRail::Airwallex,
        &r.idempotency_key,
        r.amount_minor,
    )
    .await?)
}

/// Airwallex's inbound webhook: verifies `x-timestamp` + `x-signature`, then
/// walks the deposit the event names. Org-agnostic — the signature is the
/// credential, so this route sits on the `open` surface.
///
/// `payment_intent.succeeded` confirms the deposit and credits the wallet
/// under `airwallex:<intent_id>`; `payment_intent.cancelled` and
/// `payment_intent.expired` close it. Every other event type, every replay
/// and every miss is a 200 — Airwallex retries until it hears one, and a row
/// that does not exist is not this endpoint's business.
pub(crate) async fn airwallex_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    if let Some(secret) = state.config.secret()
        && state
            .db
            .environment_merchant(oxsum_core::MerchantRail::Airwallex, secret)
            .await?
            .is_some()
    {
        return super::merchant_events::environment(
            &state,
            oxsum_core::MerchantRail::Airwallex,
            &headers,
            &body,
        )
        .await;
    }
    airwallex_legacy_webhook(State(state), headers, body).await
}

/// The body of `POST /api/v1/topups/hyperswitch`: the amount and the
/// idempotency key that anchors the deposit.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HyperswitchTopupReq {
    idempotency_key: String,
    amount_minor: i64,
}

/// The Hyperswitch top-up intent (issue #319): opens a deposit, creates a
/// switch payment and answers the payment id, the `clientSecret` the browser
/// SDK confirms against, and the publishable key it initializes with.
///
/// A replayed intent (same `idempotencyKey`) answers the same handles — the
/// deposit's `payment_ref` is the idempotency anchor.
///
/// A deployment without `OXSUM_HYPERSWITCH_*` answers 503 — the rail is
/// configured or it is not, never silently manual.
pub(crate) async fn hyperswitch_topup(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<HyperswitchTopupReq>,
) -> ApiResult<serde_json::Value> {
    ok(crate::merchant_payments::checkout(
        &state,
        principal.organization().id,
        oxsum_core::MerchantRail::Hyperswitch,
        &r.idempotency_key,
        r.amount_minor,
    )
    .await?)
}

/// The switch's outgoing webhook (issue #319): verifies
/// `X-Webhook-Signature-512`, records the event under its `event_id`, then
/// walks the deposit it names. Org-agnostic — the HMAC is the credential, so
/// this route sits on the `open` surface.
///
/// `payment_succeeded` confirms the deposit and credits the wallet under
/// `hyperswitch:<payment_id>`; `payment_cancelled` and `payment_expired` close
/// it. The refund and dispute events (issue #360) walk the `meta` the reversal
/// wrote: `refund_succeeded`/`refund_failed` settle the refund's standing, and
/// `dispute_opened` records the dispute and enqueues `deposit.disputed`. Every
/// other event type is recorded and acknowledged: the switch retries until it
/// hears a 2xx.
pub(crate) async fn hyperswitch_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    if let Some(secret) = state.config.secret()
        && state
            .db
            .environment_merchant(oxsum_core::MerchantRail::Hyperswitch, secret)
            .await?
            .is_some()
    {
        return super::merchant_events::environment(
            &state,
            oxsum_core::MerchantRail::Hyperswitch,
            &headers,
            &body,
        )
        .await;
    }
    hyperswitch_legacy_webhook(State(state), headers, body).await
}

pub(crate) async fn redeem(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<RedeemCodeReq>,
) -> ApiResult<oxsum_core::Redemption> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    let redemption = state
        .db
        .redeem_code(&w, principal.organization().id, &r.code, today())
        .await?;
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    ok(redemption)
}

/// Customer principals can observe holds but cannot create or settle them.
/// Deliberately no body extractor: denial precedes parsing and ledger access.
pub(crate) async fn hold() -> ApiResult<oxsum_core::Receipt> {
    Err(WalletError::Forbidden("raw holds require trusted billing authority".into()).into())
}

pub(crate) async fn settle() -> ApiResult<oxsum_core::Receipt> {
    Err(WalletError::Forbidden("raw settlements require trusted billing authority".into()).into())
}

/// A statement and its itemized lines, as the detail endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatementDetailRes {
    statement: oxsum_core::Statement,
    lines: Vec<oxsum_core::StatementLine>,
    anchor: Option<StatementAnchorRes>,
}

/// The statement's self-contained proof bundle (issue #479): the detail plus
/// the two inputs `verify_statement_anchor` cannot read out of the document —
/// the covered entries' content hashes and the head right after the anchor.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatementProofRes {
    statement: oxsum_core::Statement,
    lines: Vec<oxsum_core::StatementLine>,
    anchor: Option<StatementAnchorRes>,
    /// The covered entries' content hashes in log order — refold under
    /// `oxsum/statement-window/v1` rather than trusting `windowHash`.
    window_hashes: Vec<String>,
    /// The signed head at `anchor.logIndex + 1`, `None` on a no-anchor or a
    /// deployment that signs no heads — the same honesty `anchor.note` keeps.
    expected_head: Option<oxsum_core::TreeHead>,
    /// The tenant ledger origin, `oxsum/ledgers/<tenant>`.
    expected_origin: String,
}

/// The statement's log anchor as the detail endpoints answer it (issue #221):
/// the marker's identity, the two digests it commits, and — when the deployment
/// signs heads — the signed note over the head right after it, which is what
/// proves the marker was committed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatementAnchorRes {
    entry_id: Uuid,
    log_index: i64,
    window_hash: String,
    rollup_hash: String,
    origin: String,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
}

/// Builds the anchor a statement detail serves from the row's columns, signing
/// the head that follows the marker when the deployment holds a key — `None`
/// key leaves `note` and the key fields absent, as on the proof endpoint.
/// `None` when the row carries no anchor (drafts, pre-anchor statements).
pub(crate) async fn statement_anchor(
    state: &AppState,
    w: &oxsum_core::Wallet,
    statement: &oxsum_core::Statement,
) -> Result<Option<StatementAnchorRes>, ApiError> {
    let (Some(entry_id), Some(log_index), Some(window_hash), Some(rollup_hash)) = (
        statement.anchor_entry_id,
        statement.anchor_log_index,
        statement.window_hash.as_deref(),
        statement.rollup_hash.as_deref(),
    ) else {
        return Ok(None);
    };
    let key = state
        .config
        .head_signing_seed()
        .map(signing_key)
        .transpose()
        .map_err(|_| ApiError::Internal)?;
    let signed: Option<SignedHead> = match key {
        Some(key) => Some(w.signed_head_at(&key, log_index as u64 + 1).await?),
        None => None,
    };
    Ok(Some(StatementAnchorRes {
        entry_id,
        log_index,
        window_hash: window_hash.to_owned(),
        rollup_hash: rollup_hash.to_owned(),
        origin: signed
            .as_ref()
            .map(|s| s.origin.clone())
            .unwrap_or_else(|| format!("oxsum/ledgers/{}", w.tenant_id())),
        note: signed.as_ref().map(|s| s.note.clone()),
        key_name: signed.as_ref().map(|s| s.key.name.clone()),
        public_key: signed.as_ref().map(|s| {
            s.key
                .public_key
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }),
        key_hash: signed
            .as_ref()
            .map(|s| s.key.key_hash.iter().map(|b| format!("{b:02x}")).collect()),
    }))
}

/// The organization's finalized statements, newest period first — the monthly
/// billing documents "borrow first, settle monthly" issues. Drafts are the
/// operator's working copy and never appear here. The read reconciles first, so
/// a repayment that landed since the last look is already counted, and a pending
/// statement past its due date answers `overdue`.
pub(crate) async fn statements(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<oxsum_core::Statement>> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    ok(state
        .db
        .organization_statements(principal.organization().id)
        .await?)
}

/// One of the organization's finalized statements, with its lines. A draft,
/// another organization's statement and a missing id all answer 404 — an
/// unissued document does not exist for the organization.
pub(crate) async fn statement_detail(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(statement_id): Path<Uuid>,
) -> ApiResult<StatementDetailRes> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if statement.organization_id != principal.organization().id
        || statement.status != oxsum_core::StatementStatus::Finalized
    {
        return Err(ApiError::not_found());
    }
    let anchor = statement_anchor(&state, &w, &statement).await?;
    ok(StatementDetailRes {
        lines: state.db.statement_lines(statement_id).await?,
        statement,
        anchor,
    })
}

/// The statement's self-contained proof bundle (issue #479): the detail's
/// `{statement, lines, anchor}` plus the two inputs `verify_statement_anchor`
/// cannot read out of the document itself — the covered entries' content
/// hashes in log order and the head right after the anchor. A finalized
/// statement with no anchor answers `anchor: null`, the same honesty the
/// detail endpoint keeps; a draft, a foreign or a missing id is 404.
pub(crate) async fn statement_proof(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(statement_id): Path<Uuid>,
) -> ApiResult<StatementProofRes> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if statement.organization_id != principal.organization().id
        || statement.status != oxsum_core::StatementStatus::Finalized
    {
        return Err(ApiError::not_found());
    }
    let anchor = statement_anchor(&state, &w, &statement).await?;
    let lines = state.db.statement_lines(statement_id).await?;

    // The covered entries' content hashes in log order — the same period join
    // the draft folded when it anchored, re-run so the verifier can refold
    // `oxsum/statement-window/v1` itself rather than trust `windowHash`.
    let period = oxsum_core::statement_period(&statement.period).map_err(|_| ApiError::Internal)?;
    let window_hashes: Vec<String> = state
        .db
        .statement_window_hashes(&principal.organization().tenant_id, &period)
        .await?
        .iter()
        .map(oxsum_core::Hash::to_hex)
        .collect();

    // The head right after the anchor — the head whose log already contains
    // it, as `verify_statement_anchor`'s `expected_head` names it. It is only
    // meaningful where `anchor.note` can attest it, so a deployment that signs
    // no heads serves none — matching the anchor's own `note`-absent honesty.
    let expected_head = match (anchor.as_ref(), state.config.head_signing_seed()) {
        (Some(a), Some(seed)) => {
            let key = signing_key(seed).map_err(|_| ApiError::Internal)?;
            Some(w.signed_head_at(&key, a.log_index as u64 + 1).await?.head)
        }
        _ => None,
    };
    let expected_origin = format!("oxsum/ledgers/{}", w.tenant_id());
    ok(StatementProofRes {
        statement,
        lines,
        anchor,
        window_hashes,
        expected_head,
        expected_origin,
    })
}

/// The `/verify` URL the PDF's footer and QR point at: `OXSUM_PUBLIC_URL`
/// plus the path when the deployment names one, else the relative path — the
/// same rule `verification_uri` uses for the device page.
pub(crate) async fn statement_pdf(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(statement_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if statement.organization_id != principal.organization().id
        || statement.status != oxsum_core::StatementStatus::Finalized
    {
        return Err(ApiError::not_found());
    }
    let lines = state.db.statement_lines(statement_id).await?;
    let url = verify_url(&state);
    let display = state.config.display_for(principal.organization());
    let bytes = oxsum_core::render_statement_pdf(
        &statement,
        &principal.organization().name,
        &lines,
        Some(&url),
        display.as_ref(),
    )?;
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

pub(crate) async fn balance(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<BalanceRes> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    ok(BalanceRes {
        available_minor: w.available().await?,
        credit_limit_minor: w.credit_limit().await?,
        credit_used_minor: w.credit_used().await?,
        reserved_minor: w.reserved().await?,
        spent_this_month_minor: w.month_spend().await?,
    })
}

pub(crate) async fn stripe_legacy_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let stripe = state.config.stripe().ok_or_else(ApiError::not_found)?;
    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !stripe.verify_signature(signature, &body) {
        return Err(ApiError::Unauthorized);
    }
    #[derive(Deserialize)]
    struct Event {
        #[serde(rename = "type")]
        kind: String,
        data: EventData,
    }
    #[derive(Deserialize)]
    struct EventData {
        object: serde_json::Value,
    }
    let event: Event = serde_json::from_slice(&body)
        .map_err(|e| ApiError::Validation(format!("unreadable event: {e}")))?;
    match event.kind.as_str() {
        "checkout.session.completed" => {
            let session_id = event.data.object["id"]
                .as_str()
                .ok_or_else(|| ApiError::Validation("no session id".into()))?;
            let amount = event.data.object["amount_total"].as_i64();
            // A subscription-mode session's object carries `subscription` — a
            // top-up's does not, so the field decides which flow opened it.
            if let Some(subscription_id) = event.data.object["subscription"].as_str()
                && !subscription_id.is_empty()
            {
                // The subscription's opening: bind the organization the
                // session named to the plan the session's line item priced.
                // `metadata` is what the org user cannot fake — the session
                // is Stripe's own record of what was sold.
                let org = event.data.object["client_reference_id"]
                    .as_str()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .ok_or_else(|| ApiError::Validation("no client reference".into()))?;
                let price_id = event.data.object["metadata"]["oxsum_plan_id"]
                    .as_str()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .ok_or_else(|| ApiError::Validation("no plan id".into()))?;
                state
                    .db
                    .bind_subscription(org, price_id, "stripe", Some(subscription_id))
                    .await?;
            } else {
                // Find the deposit this session opened, then confirm + credit it.
                // Only a `pending` deposit moves, so a replayed or raced webhook
                // does nothing twice.
                if let Some((deposit_id, organization_id, tenant_id)) =
                    state.db.find_stripe_by_session(session_id).await?
                    && state.db.confirm_stripe_deposit(deposit_id).await?
                {
                    let w = state
                        .tenants
                        .get(&tenant_id)
                        .await
                        .map_err(ApiError::from)?;
                    state
                        .db
                        .credit_stripe_deposit(&w, deposit_id, session_id, amount, today())
                        .await?;
                    // Whatever the deposit repaid counts against the statement book.
                    state
                        .db
                        .reconcile_statements(organization_id, &w, today())
                        .await?;
                }
            }
        }
        "checkout.session.expired" => {
            let session_id = event.data.object["id"]
                .as_str()
                .ok_or_else(|| ApiError::Validation("no session id".into()))?;
            if let Some((deposit_id, _, _)) = state.db.find_stripe_by_session(session_id).await? {
                state.db.expire_stripe_deposit(deposit_id).await?;
            }
        }
        // A subscription renewal: the invoice Stripe settled rolls the
        // subscription's period forward to the boundary the invoice opened.
        "invoice.paid" => {
            let subscription_id = event.data.object["subscription"]
                .as_str()
                .ok_or_else(|| ApiError::Validation("no subscription id".into()))?;
            let period_end = event.data.object["lines"]["data"][0]["period"]["end"]
                .as_i64()
                .map(time::OffsetDateTime::from_unix_timestamp)
                .transpose()
                .map_err(|_| ApiError::Validation("bad period end".into()))?
                .ok_or_else(|| ApiError::Validation("no period end".into()))?;
            state
                .db
                .renew_stripe_subscription(subscription_id, period_end)
                .await?;
        }
        // The rail's cancel: Stripe ended the subscription — the period
        // closes, and the wallet takes over from the next hold.
        "customer.subscription.deleted" => {
            let subscription_id = event.data.object["id"]
                .as_str()
                .ok_or_else(|| ApiError::Validation("no subscription id".into()))?;
            state.db.cancel_stripe_subscription(subscription_id).await?;
        }
        _ => {}
    }
    Ok(StatusCode::OK)
}

pub(crate) async fn airwallex_legacy_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let airwallex = state.config.airwallex().ok_or_else(ApiError::not_found)?;
    let timestamp = headers
        .get("x-timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let signature = headers
        .get("x-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !airwallex.verify_signature(timestamp, signature, &body) {
        return Err(ApiError::Unauthorized);
    }
    #[derive(Deserialize)]
    struct Event {
        name: Option<String>,
        data: EventData,
    }
    #[derive(Deserialize)]
    struct EventData {
        object: serde_json::Value,
    }
    let event: Event = serde_json::from_slice(&body)
        .map_err(|e| ApiError::Validation(format!("unreadable event: {e}")))?;
    match event.name.as_deref() {
        Some("payment_intent.succeeded") => {
            let intent_id = event.data.object["id"]
                .as_str()
                .ok_or_else(|| ApiError::Validation("no intent id".into()))?;
            // The intent reports `amount` in decimal major units; the deposit
            // records `received_minor` in cents, like every rail.
            let amount = event.data.object["amount"]
                .as_f64()
                .map(|major| (major * 100.0).round() as i64);
            // Find the deposit this intent opened, then confirm + credit it.
            // Only a `pending` deposit moves, so a replayed or raced webhook
            // does nothing twice.
            if let Some((deposit_id, organization_id, tenant_id)) =
                state.db.find_airwallex_by_intent(intent_id).await?
                && state.db.confirm_airwallex_deposit(deposit_id).await?
            {
                let w = state
                    .tenants
                    .get(&tenant_id)
                    .await
                    .map_err(ApiError::from)?;
                state
                    .db
                    .credit_airwallex_deposit(&w, deposit_id, intent_id, amount, today())
                    .await?;
                // Whatever the deposit repaid counts against the statement book.
                state
                    .db
                    .reconcile_statements(organization_id, &w, today())
                    .await?;
            }
        }
        Some("payment_intent.cancelled") | Some("payment_intent.expired") => {
            let intent_id = event.data.object["id"]
                .as_str()
                .ok_or_else(|| ApiError::Validation("no intent id".into()))?;
            if let Some((deposit_id, _, _)) = state.db.find_airwallex_by_intent(intent_id).await? {
                state.db.expire_airwallex_deposit(deposit_id).await?;
            }
        }
        _ => {}
    }
    Ok(StatusCode::OK)
}

pub(crate) async fn hyperswitch_legacy_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let switch = state.config.hyperswitch().ok_or_else(ApiError::not_found)?;
    let signature = headers
        .get("x-webhook-signature-512")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !switch.verify_signature(signature, &body) {
        return Err(ApiError::Unauthorized);
    }
    let event: WebhookEvent = serde_json::from_slice(&body)
        .map_err(|e| ApiError::Validation(format!("unreadable event: {e}")))?;
    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::Validation(format!("unreadable event: {e}")))?;
    // The event id is the idempotency anchor: a delivery that was already
    // processed is acknowledged without acting again.
    if !state
        .db
        .record_payment_event(
            &event.event_id,
            oxsum_core::RAIL_HYPERSWITCH,
            &event.event_type,
            &payload,
        )
        .await?
    {
        return Ok(StatusCode::OK);
    }
    let payment_id = event.content.object["payment_id"].as_str();
    match event.event_type.as_str() {
        "payment_succeeded" => {
            let payment_id =
                payment_id.ok_or_else(|| ApiError::Validation("no payment id".into()))?;
            // The switch reports the amount in minor units, like the deposit.
            let received = event.content.object["amount"].as_i64();
            // Find the deposit this payment opened, then confirm + credit it.
            // Only a `pending` deposit moves, so a replayed or raced webhook
            // does nothing twice.
            if let Some((deposit_id, organization_id, tenant_id)) =
                state.db.find_hyperswitch_by_payment(payment_id).await?
                && state.db.confirm_hyperswitch_deposit(deposit_id).await?
            {
                let w = state
                    .tenants
                    .get(&tenant_id)
                    .await
                    .map_err(ApiError::from)?;
                state
                    .db
                    .credit_hyperswitch_deposit(&w, deposit_id, payment_id, received, today())
                    .await?;
                // Whatever the deposit repaid counts against the statement book.
                state
                    .db
                    .reconcile_statements(organization_id, &w, today())
                    .await?;
            }
        }
        "payment_cancelled" | "payment_expired" => {
            if let Some(payment_id) = payment_id
                && let Some((deposit_id, _, _)) =
                    state.db.find_hyperswitch_by_payment(payment_id).await?
            {
                state.db.expire_hyperswitch_deposit(deposit_id).await?;
            }
        }
        // Refund and dispute events (issue #360): the content object is the
        // refund or dispute resource — the refund names its own id plus the
        // payment it refunds, the dispute names only the payment. The deposit
        // is found by the refund id the reversal recorded, or by the payment
        // id as a fallback for an event that outran the record.
        "refund_succeeded" | "refund_failed" => {
            let object = &event.content.object;
            let refund_id = object["refund_id"].as_str().unwrap_or_default();
            let refund_status = object["status"].as_str().unwrap_or_default();
            let status = if event.event_type == "refund_succeeded" {
                "succeeded"
            } else {
                "failed"
            };
            // The webhook's own status wins when it names one; the event type
            // is the fallback for a payload that leaves it ambiguous.
            let status = if refund_status.is_empty() {
                status
            } else {
                refund_status
            };
            if let Some(payment_id) = payment_id
                && let Some((deposit_id, _, _)) = state
                    .db
                    .find_hyperswitch_by_refund_or_payment(
                        if refund_id.is_empty() {
                            None
                        } else {
                            Some(refund_id)
                        },
                        payment_id,
                    )
                    .await?
            {
                state
                    .db
                    .settle_hyperswitch_refund(deposit_id, refund_id, status)
                    .await?;
            }
        }
        "dispute_opened" => {
            if let Some(payment_id) = payment_id
                && let Some((deposit_id, organization_id, _)) = state
                    .db
                    .find_hyperswitch_by_refund_or_payment(None, payment_id)
                    .await?
            {
                state
                    .db
                    .record_dispute(deposit_id, organization_id, &event.content.object)
                    .await?;
            }
        }
        _ => {}
    }
    Ok(StatusCode::OK)
}
