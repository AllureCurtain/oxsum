//! Corrections, partial refunds and disputes on payment orders (issue #608,
//! task T17).
//!
//! Three durable facts, deliberately separate like T15's paid/fulfilled split:
//!
//! - A **refund request** is the customer's ask for review — it can never
//!   grant itself money. `requested` is the only state a customer writes.
//! - A **resolution** is a staff decision under `RefundsWrite`: approve moves
//!   the refund to `provider_pending` — the durable reservation against the
//!   order's refundable bound, committed *before* any PSP call — and issues a
//!   credit note naming the order it corrects; decline lands `declined`.
//! - The **provider's answer** arrives through `merchant_event_inbox`'s
//!   `refund_succeeded`/`refund_failed`/`dispute` kinds: the signed event is
//!   the only source of terminal truth. `refund_succeeded` books the wallet
//!   take-back (`debit Wallet, credit Cash` — `FundedReservations` refuses it
//!   when the customer already spent the credit, and the compensation is then
//!   recorded on the audit trail, never silently negative); `refund_failed`
//!   releases the reservation. A PSP timeout leaves `provider_pending` for
//!   the `refund-retries` job to re-issue inside a bounded attempt budget.
//!
//! A **dispute** is not a refund: the `dispute` event kind opens a contested
//! record of its own (`opened → evidence_due → won | lost | closed`). Each
//! resolution commits authority, ledger/documents, terminal state and audit
//! together. Historical interrupted effects require source review. New non-plan
//! losses retain complete Cash credit against proven original Wallet funding or
//! explicit review suspense. Historical wrong-direction entries remain #842.
//!
//! The promo leg of a `bundle` order is never refundable cash: the bound is
//! `amount_minor` alone, and cancelling the promo lot is T18's seam — a
//! refund never converts grant into money.

mod dispute_resolution;

use crate::{
    BusinessCapability, Db, MerchantEventKind, MerchantEventReceipt, WalletError,
    business_staff as authority,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// How many provider attempts a `provider_pending` refund gets before the
/// reservation releases — the definite-failure exit, never an infinite loop.
pub const REFUND_MAX_ATTEMPTS: i32 = 5;
/// The retry backoff in seconds by attempt number (1-based, clamped at the
/// table's end): short enough for a transient PSP timeout, long enough that a
/// rail outage isn't hammered.
const REFUND_RETRY_DELAYS_SECS: [i64; 5] = [60, 120, 300, 900, 3600];

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

/// A refund's decision, as the resolve request spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefundDecision {
    /// Approve the refund: reserve the bound and issue a credit note.
    Approve,
    /// Decline the refund: no money moves.
    Decline,
}

/// One refund, as the customer and the business surfaces read it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderRefund {
    /// Refund identifier.
    pub refund_id: Uuid,
    /// Payment order the refund corrects.
    pub order_id: Uuid,
    /// Customer organization that owns the order.
    pub organization_id: Uuid,
    /// The credit amount asked back, in ledger minor units.
    pub amount_minor: i64,
    /// The fiat figure the PSP is asked to return.
    pub display_amount_minor: i64,
    /// Currency of the display amount.
    pub display_currency: String,
    /// Customer's reason for the refund request.
    pub reason: String,
    /// `requested → provider_pending → succeeded | failed | released`; the
    /// request's other exit is `declined`.
    pub state: String,
    /// New Hyperswitch requests use their immutable local refund UUID in the body.
    /// Historical requests must only retrieve their already-known handle.
    #[serde(skip_serializing)]
    pub hyperswitch_body_id: bool,
    /// PSP-side refund reference, once the provider assigns one.
    pub provider_refund_id: Option<String>,
    /// Provider attempts made so far for a `provider_pending` refund.
    pub retry_count: i32,
    /// The business the refund's audit rows belong to — internal scoping,
    /// never serialized to the customer.
    #[serde(skip_serializing)]
    pub business_id: Uuid,
    /// The staff user who resolved it; `None` while requested.
    #[serde(skip_serializing)]
    pub resolved_by: Option<Uuid>,
    /// When the next provider attempt is due; `None` when not retrying.
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_retry_at: Option<OffsetDateTime>,
    /// The credit note the approval issued, once it exists.
    pub note_id: Option<Uuid>,
    /// When the request was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the refund last changed.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// One page of refunds.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderRefundPage {
    /// Refunds in this page.
    pub refunds: Vec<OrderRefund>,
    /// Cursor for the next page; `None` at the end.
    pub next_cursor: Option<Uuid>,
}

/// A contested charge as the dispute record tracks it — deliberately not a
/// refund: a dispute is a claim the provider arbitrates. A new `lost` outcome
/// retains the complete known cash loss against its source or review suspense.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderDispute {
    /// Dispute identifier.
    pub dispute_id: Uuid,
    /// Order whose charge is contested.
    pub order_id: Uuid,
    /// Customer organization that owns the order.
    pub organization_id: Uuid,
    /// PSP-side dispute reference.
    pub provider_dispute_id: String,
    /// Contested ledger credit minor units; non-plan fiat converts exactly.
    pub amount_minor: i64,
    /// Original collection fiat currency; `amount_minor` uses ledger credit units.
    pub currency: String,
    /// `opened → evidence_due → won | lost | closed`.
    pub state: String,
    /// Ledger entry retaining the lost chargeback cash; `None` before.
    pub recharge_entry_id: Option<Uuid>,
    /// When the dispute record opened.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the dispute last changed.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// One page of the business's disputes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderDisputePage {
    /// Disputes in this page.
    pub disputes: Vec<OrderDispute>,
    /// Cursor for the next page; `None` at the end.
    pub next_cursor: Option<Uuid>,
}

/// One finance-resolution audit row: who decided, on what, under which
/// reason, naming the customer-visible note it produced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinanceAudit {
    /// Audit row identifier.
    pub audit_id: Uuid,
    /// Staff user who made the decision.
    pub actor_user_id: Uuid,
    /// Dotted action name, such as `refund.approve`.
    pub action: String,
    /// Identifier of the affected resource, usually the refund or dispute.
    pub target: String,
    /// Action-specific detail, including the reason and amounts.
    pub detail: serde_json::Value,
    /// Credit note the action produced, when it produced one.
    pub note_id: Option<Uuid>,
    /// When the audit row was written.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The credit-note projection T17 adds: an order-linked correction walks
/// `issued → applied` as its ledger entry lands, `reversed` when the refund
/// releases without applying.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderCreditNote {
    /// Credit note identifier.
    pub credit_note_id: Uuid,
    /// Customer organization the note corrects.
    pub organization_id: Uuid,
    /// Order the note corrects.
    pub order_id: Uuid,
    /// Refund whose approval issued the note.
    pub refund_id: Uuid,
    /// The take-back entry once `applied`; `None` while issued-unapplied.
    pub entry_id: Option<Uuid>,
    /// Credit amount being corrected, in ledger minor units.
    pub amount_minor: i64,
    /// Human-readable reason shown to the customer.
    pub reason: String,
    /// `issued`, `applied` or `reversed`.
    pub state: String,
    /// When the note was issued.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One page of the business's order-linked credit notes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderCreditNotePage {
    /// Credit notes in this page.
    pub notes: Vec<OrderCreditNote>,
    /// Cursor for the next page; `None` at the end.
    pub next_cursor: Option<Uuid>,
}

// The select list joins the refund's credit note (at most one — a resolution
// issues exactly one) so every read carries `note_id` without a second query.
const REFUND_SELECT: &str = "SELECT r.refund_id,r.order_id,r.organization_id,r.business_id,\
     r.amount_minor,r.display_amount_minor,r.display_currency,r.reason,r.state,\
     r.provider_refund_id,r.provider_payment_id,r.retry_count,r.next_retry_at,r.hyperswitch_body_id,\
     r.requested_by,r.resolved_by,r.audit_ref,r.request_key,r.created_at,r.updated_at,\
     n.credit_note_id AS note_id \
     FROM oxsum.refunds r \
     LEFT JOIN oxsum.credit_notes n ON n.refund_id = r.refund_id";

fn refund_from_row(row: &PgRow) -> Result<OrderRefund, WalletError> {
    Ok(OrderRefund {
        refund_id: row.try_get("refund_id")?,
        order_id: row.try_get("order_id")?,
        organization_id: row.try_get("organization_id")?,
        amount_minor: row.try_get("amount_minor")?,
        display_amount_minor: row.try_get("display_amount_minor")?,
        display_currency: row.try_get("display_currency")?,
        reason: row.try_get("reason")?,
        state: row.try_get("state")?,
        provider_refund_id: row.try_get("provider_refund_id")?,
        hyperswitch_body_id: row.try_get("hyperswitch_body_id")?,
        retry_count: row.try_get("retry_count")?,
        business_id: row.try_get("business_id")?,
        resolved_by: row.try_get("resolved_by")?,
        next_retry_at: row.try_get("next_retry_at")?,
        note_id: row.try_get("note_id")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// The order columns a refund decision reads: the frozen terms, the
/// lifecycle, the business it audits under, and the merchant coordinates the
/// PSP call needs.
struct RefundOrder {
    business_id: Uuid,
    organization_id: Uuid,
    amount_minor: i64,
    display_amount_minor: i64,
    display_currency: String,
    provider_payment_id: Option<String>,
    status: String,
    purpose: String,
}

fn refund_order(row: &PgRow) -> Result<RefundOrder, WalletError> {
    Ok(RefundOrder {
        business_id: row.try_get("business_id")?,
        organization_id: row.try_get("organization_id")?,
        amount_minor: row.try_get("amount_minor")?,
        display_amount_minor: row.try_get("display_amount_minor")?,
        display_currency: row.try_get("display_currency")?,
        provider_payment_id: row.try_get("provider_payment_id")?,
        status: row.try_get("status")?,
        purpose: row.try_get("purpose")?,
    })
}

const REFUND_ORDER_COLS: &str = "business_id,organization_id,amount_minor,\
     display_amount_minor,display_currency,provider_payment_id,status,purpose";

async fn refund_order_in(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<Option<RefundOrder>, WalletError> {
    let row = sqlx::query(&format!(
        "SELECT {REFUND_ORDER_COLS} FROM oxsum.payment_orders WHERE order_id=$1 FOR UPDATE"
    ))
    .bind(order_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(refund_order).transpose()
}

async fn refund_in(
    tx: &mut Transaction<'_, Postgres>,
    refund_id: Uuid,
) -> Result<Option<OrderRefund>, WalletError> {
    let row = sqlx::query(&format!(
        "{REFUND_SELECT} WHERE r.refund_id=$1 FOR UPDATE OF r"
    ))
    .bind(refund_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(refund_from_row).transpose()
}

/// What one order has already refunded or reserved: succeeded refunds plus
/// the requested/provider_pending reservations still holding a bound.
/// Declined, failed and released rows have freed theirs.
async fn committed_minor_in(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<i64, WalletError> {
    let committed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.refunds \
         WHERE order_id=$1 AND (state IN ('requested','provider_pending','succeeded') OR EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=refunds.refund_id))",
    )
    .bind(order_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(committed)
}

/// The display figure already committed by live refunds on the order — the
/// fiat counterpart of [`committed_minor_in`].
async fn committed_display_minor_in(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<i64, WalletError> {
    let committed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(display_amount_minor),0)::bigint FROM oxsum.refunds \
         WHERE order_id=$1 AND (state IN ('requested','provider_pending','succeeded') OR EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=refunds.refund_id))",
    )
    .bind(order_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(committed)
}

/// The credit-minor a refund of `credit_minor` carries back in display terms:
/// proportional at the order's frozen rate, except the refund that completes
/// the order's refundable total takes the display leftover so the last
/// partial refund never strands a rounding cent.
fn display_refund_minor(
    order: &RefundOrder,
    credit_minor: i64,
    committed_credit: i64,
    committed_display: i64,
) -> i64 {
    if committed_credit + credit_minor == order.amount_minor {
        (order.display_amount_minor - committed_display).max(0)
    } else {
        // Integer division toward zero: a partial refund may under-claim its
        // share by a fraction of a cent, which the final refund picks up.
        order
            .display_amount_minor
            .saturating_mul(credit_minor)
            .checked_div(order.amount_minor)
            .unwrap_or(0)
    }
}

/// What a refund-request create carries: the credit amount asked back (the
/// full refundable remainder when `None`) and the customer's reason, anchored
/// by `request_key` — the idempotency key the replay returns the standing
/// refund under.
#[derive(Debug, Clone)]
pub struct NewRefundRequest {
    /// Credit amount to refund in minor units; `None` refunds the full
    /// refundable remainder.
    pub amount_minor: Option<i64>,
    /// Customer's reason, up to 500 bytes.
    pub reason: String,
    /// Idempotency key scoped to the organization.
    pub request_key: String,
    /// The session user asking — audit, never authorization: the row records
    /// who asked; only staff resolves.
    pub requested_by: Option<Uuid>,
}

impl Db {
    /// Creates a refund request on one of the caller's own orders. The
    /// request is the review's durable record — `requested`, no PSP call, no
    /// ledger write — and a replay of `(organization, request_key)` returns
    /// the standing refund rather than opening a second review.
    ///
    /// The bound holds at request time *and* again inside the resolution's
    /// locked transaction: the amount may not exceed the order's funded
    /// `amount_minor` minus what live refunds already reserve or returned —
    /// the promo leg never counts, grant is never refundable cash — so no
    /// wallet can be driven negative by refunding.
    pub async fn request_order_refund(
        &self,
        organization: Uuid,
        order_id: Uuid,
        request: &NewRefundRequest,
    ) -> Result<OrderRefund, WalletError> {
        authority::validate(&request.request_key, None)?;
        if request.reason.trim().is_empty()
            || request.reason.len() > 500
            || !identifier(&request.request_key)
        {
            return Err(invalid("invalid refund request terms"));
        }
        if let Some(amount) = request.amount_minor
            && amount <= 0
        {
            return Err(invalid("a refund amount must be positive"));
        }
        let mut tx = self.pool().begin().await?;
        // The order row serializes every refund decision on it: a request and
        // a resolution, or two racing requests, compute the bound against the
        // same committed figure.
        // Historical correction holds the business before its source snapshot.
        // A new request must serialize before that correction or see its fence.
        authority::lock_business(&mut tx).await?;
        crate::nonplan_returns::lock_source_in(&mut tx, order_id).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("order-refund:{order_id}"))
            .execute(&mut *tx)
            .await?;
        // A replayed request answers its own refund before anything else.
        if let Some(row) = sqlx::query(&format!(
            "{REFUND_SELECT} WHERE r.organization_id=$1 AND r.request_key=$2"
        ))
        .bind(organization)
        .bind(&request.request_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            let standing = refund_from_row(&row)?;
            if standing.order_id != order_id
                || request
                    .amount_minor
                    .is_some_and(|a| a != standing.amount_minor)
            {
                return Err(WalletError::Conflict(
                    "refund request terms changed for the same idempotency key".into(),
                ));
            }
            tx.commit().await?;
            return Ok(standing);
        }
        let Some(order) = refund_order_in(&mut tx, order_id).await? else {
            return Err(WalletError::NotFound("unknown payment order".into()));
        };
        if order.organization_id != organization {
            // Another organization's order is a not-found, the same scoping
            // the order read uses.
            return Err(WalletError::NotFound("unknown payment order".into()));
        }
        if !crate::plan_usage_corrections::corrected_order_journals_in(&mut tx, order_id)
            .await?
            .is_empty()
        {
            return Err(WalletError::Conflict("this order funds a historical usage correction; review its refund payable before authorizing a payout".into()));
        }
        crate::nonplan_returns::guard_lost_in(&mut tx, order_id).await?;
        // Only money that actually arrived may refund: `paid` or `fulfilled`
        // — a `provider_pending` order has nothing to give back yet.
        if !matches!(order.status.as_str(), "paid" | "fulfilled") {
            return Err(WalletError::Conflict(format!(
                "a {} payment order has nothing refundable",
                order.status
            )));
        }
        if order.purpose == "bundle" && order.status != "fulfilled" {
            return Err(WalletError::Conflict(
                "bundle fulfillment must complete before a refund can be requested".into(),
            ));
        }
        let committed = committed_minor_in(&mut tx, order_id).await?;
        let refundable = order.amount_minor - committed;
        let amount = request.amount_minor.unwrap_or(refundable);
        if amount <= 0 || amount > refundable {
            return Err(WalletError::InvalidInput(format!(
                "the order's refundable remainder is {refundable} minor"
            )));
        }
        let display = display_refund_minor(
            &order,
            amount,
            committed,
            committed_display_minor_in(&mut tx, order_id).await?,
        );
        if display <= 0 {
            return Err(invalid("the refund rates to no display amount"));
        }
        let refund_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.refunds\
             (refund_id,order_id,organization_id,business_id,amount_minor,\
              display_amount_minor,display_currency,reason,state,provider_payment_id,\
              requested_by,request_key,hyperswitch_body_id) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,'requested',$9,$10,$11,true)",
        )
        .bind(refund_id)
        .bind(order_id)
        .bind(organization)
        .bind(order.business_id)
        .bind(amount)
        .bind(display)
        .bind(&order.display_currency)
        .bind(request.reason.trim())
        .bind(&order.provider_payment_id)
        .bind(request.requested_by)
        .bind(&request.request_key)
        .execute(&mut *tx)
        .await?;
        let standing = refund_in(&mut tx, refund_id)
            .await?
            .ok_or_else(|| invalid("the refund request did not land"))?;
        tx.commit().await?;
        Ok(standing)
    }

    /// The order's refundable remainder in credit-minor — `amount_minor`
    /// minus what live refunds reserve or returned, the promo leg never
    /// counted. The customer surface reads it before naming an amount.
    pub async fn order_refundable_minor(
        &self,
        organization: Uuid,
        order_id: Uuid,
    ) -> Result<Option<i64>, WalletError> {
        let row = sqlx::query(
            "SELECT amount_minor FROM oxsum.payment_orders \
             WHERE order_id=$1 AND organization_id=$2 AND status IN ('paid','fulfilled') \
               AND (purpose <> 'bundle' OR status='fulfilled') \
               AND (purpose='plan_period' OR NOT EXISTS(SELECT 1 FROM oxsum.disputes d WHERE d.order_id=payment_orders.order_id AND d.state='lost'))",
        )
        .bind(order_id)
        .bind(organization)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let total: i64 = row.try_get("amount_minor")?;
        let committed: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.refunds \
             WHERE order_id=$1 AND (state IN ('requested','provider_pending','succeeded') OR EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=refunds.refund_id))",
        )
        .bind(order_id)
        .fetch_one(self.pool())
        .await?;
        Ok(Some(total - committed))
    }

    /// One order's refunds — the customer read, scoped to the caller's own
    /// organization like every order read.
    pub async fn order_refunds(
        &self,
        organization: Uuid,
        order_id: Uuid,
    ) -> Result<Vec<OrderRefund>, WalletError> {
        let rows = sqlx::query(&format!(
            "{REFUND_SELECT} WHERE r.order_id=$1 AND r.organization_id=$2 ORDER BY r.created_at"
        ))
        .bind(order_id)
        .bind(organization)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(refund_from_row).collect()
    }
}

impl Db {
    /// The staff resolution a `requested` refund waits for — `RefundsWrite`,
    /// a personal business-context action. Approve writes the durable
    /// reservation (`provider_pending`, `next_retry_at` due now) *and* the
    /// `issued` credit note naming the order it corrects, in one transaction
    /// with the `finance_audit` row — the PSP is called only after this
    /// commits, so a crash mid-call leaves a retryable reservation, never a
    /// lost refund. Decline lands `declined` with the same audit.
    ///
    /// Returns `(refund, needs_provider_call)`: the caller issues the PSP
    /// refund only for an approved row this call moved — a replayed resolve
    /// answers the standing refund without asking for a second provider call.
    pub async fn resolve_order_refund(
        &self,
        token: &str,
        refund_id: Uuid,
        decision: RefundDecision,
        reason: Option<&str>,
        idempotency_key: &str,
    ) -> Result<(OrderRefund, bool), WalletError> {
        authority::validate(idempotency_key, reason)?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::RefundsWrite,
        )
        .await?;
        let order_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT order_id FROM oxsum.refunds WHERE refund_id=$1 AND business_id=$2",
        )
        .bind(refund_id)
        .bind(business)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(order_id) = order_id {
            crate::nonplan_returns::lock_source_in(&mut tx, order_id).await?;
        }
        let Some(existing) = refund_in(&mut tx, refund_id).await? else {
            return Err(WalletError::NotFound("unknown refund".into()));
        };
        if existing.business_id != business {
            return Err(WalletError::NotFound("unknown refund".into()));
        }
        if existing.state != "requested" {
            // A terminal or in-flight row is the resolve's own replay —
            // answer it without re-deciding. `provider_pending` also means a
            // prior approve committed; the provider call is not re-requested
            // (its retry owns re-issuing).
            tx.commit().await?;
            return Ok((existing, false));
        }
        let reason = reason.map(str::trim).filter(|r| !r.is_empty());
        let audit_id = Uuid::new_v4();
        match decision {
            RefundDecision::Decline => {
                sqlx::query(
                    "UPDATE oxsum.refunds SET state='declined',resolved_by=$2,\
                     audit_ref=$3,updated_at=now() WHERE refund_id=$1",
                )
                .bind(refund_id)
                .bind(actor.user_id)
                .bind(audit_id)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO oxsum.finance_audit\
                     (audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) \
                     VALUES($1,$2,$3,'refund.decline',$4,$5,$6)",
                )
                .bind(audit_id)
                .bind(business)
                .bind(actor.user_id)
                .bind(refund_id.to_string())
                .bind(json!({
                    "orderId": existing.order_id,
                    "amountMinor": existing.amount_minor,
                    "reason": reason,
                }))
                .bind(idempotency_key)
                .execute(&mut *tx)
                .await?;
                let standing = refund_in(&mut tx, refund_id)
                    .await?
                    .ok_or_else(|| invalid("the refund did not land"))?;
                tx.commit().await?;
                Ok((standing, false))
            }
            RefundDecision::Approve => {
                crate::nonplan_returns::guard_lost_in(&mut tx, existing.order_id).await?;
                if !crate::plan_usage_corrections::corrected_order_journals_in(
                    &mut tx,
                    existing.order_id,
                )
                .await?
                .is_empty()
                {
                    return Err(WalletError::Conflict("this order funds a historical usage correction; review its refund payable before provider authorization".into()));
                }
                // Re-check the bound inside the locked transaction: refunds
                // resolved since the request was written may have consumed
                // the remainder this one was sized against.
                let order = refund_order_in(&mut tx, existing.order_id)
                    .await?
                    .ok_or_else(|| invalid("the refund's order is gone"))?;
                if order.purpose == "bundle" && order.status != "fulfilled" {
                    return Err(WalletError::Conflict(
                        "bundle fulfillment must complete before refund approval".into(),
                    ));
                }
                let committed =
                    committed_minor_in(&mut tx, existing.order_id).await? - existing.amount_minor; // the request's own reservation counts too
                if committed + existing.amount_minor > order.amount_minor {
                    return Err(WalletError::Conflict(
                        "the order's refundable remainder no longer covers this refund".into(),
                    ));
                }
                // The reservation and the note land together: the note's
                // `issued` state says its ledger entry is not booked yet —
                // the wallet credit lands when the signed refund event does.
                let note_id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO oxsum.credit_notes\
                     (credit_note_id,organization_id,order_id,refund_id,amount_minor,reason,state,issued_by,audit_ref) \
                     VALUES($1,$2,$3,$4,$5,$6,'issued',$7,$8)",
                )
                .bind(note_id)
                .bind(existing.organization_id)
                .bind(existing.order_id)
                .bind(refund_id)
                .bind(existing.amount_minor)
                .bind(format!(
                    "refund of order {} — {}",
                    existing.order_id,
                    reason.unwrap_or(existing.reason.as_str())
                ))
                .bind(actor.user_id)
                .bind(audit_id)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE oxsum.refunds SET state='provider_pending',resolved_by=$2,\
                     audit_ref=$3,next_retry_at=now(),updated_at=now() WHERE refund_id=$1",
                )
                .bind(refund_id)
                .bind(actor.user_id)
                .bind(audit_id)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO oxsum.finance_audit\
                     (audit_id,business_id,actor_user_id,action,target,detail,note_id,idempotency_key) \
                     VALUES($1,$2,$3,'refund.approve',$4,$5,$6,$7)",
                )
                .bind(audit_id)
                .bind(business)
                .bind(actor.user_id)
                .bind(refund_id.to_string())
                .bind(json!({
                    "orderId": existing.order_id,
                    "amountMinor": existing.amount_minor,
                    "displayAmountMinor": existing.display_amount_minor,
                    "displayCurrency": existing.display_currency,
                    "reason": reason,
                }))
                .bind(note_id)
                .bind(idempotency_key)
                .execute(&mut *tx)
                .await?;
                let standing = refund_in(&mut tx, refund_id)
                    .await?
                    .ok_or_else(|| invalid("the refund did not land"))?;
                tx.commit().await?;
                Ok((standing, true))
            }
        }
    }

    /// Records the PSP handle a refund's provider call returned. The refund
    /// stays `provider_pending` — the signed webhook is the only terminal
    /// truth — but the provider's own id is what the `refund_*` events name,
    /// and a PSP that already answers `succeeded` lets the success event's
    /// dedup settle it. New Hyperswitch requests pin the refund UUID in the
    /// provider body; known historical handles are retrieval-only.
    pub async fn note_refund_provider(
        &self,
        refund_id: Uuid,
        provider_refund_id: &str,
    ) -> Result<(), WalletError> {
        if !identifier(provider_refund_id) {
            return Err(invalid("invalid provider refund reference"));
        }
        sqlx::query(
            "UPDATE oxsum.refunds SET provider_refund_id=$2,updated_at=now() \
             WHERE refund_id=$1 AND state='provider_pending' \
               AND (provider_refund_id IS NULL OR provider_refund_id=$2)",
        )
        .bind(refund_id)
        .bind(provider_refund_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Reschedules an ambiguous or acknowledged provider outcome within a
    /// bounded recovery budget. Exhaustion stops automatic calls but keeps the
    /// reservation and issued note intact until an authenticated terminal event.
    pub async fn retry_order_refund(&self, refund_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.refunds SET retry_count=LEAST(retry_count+1,$3),\
             next_retry_at=CASE WHEN retry_count+1 < $3 THEN now() + \
               (($2::bigint[])[LEAST(retry_count+1,$3)] * interval '1 second') ELSE NULL END,\
             updated_at=now() WHERE refund_id=$1 AND state='provider_pending'",
        )
        .bind(refund_id)
        .bind(REFUND_RETRY_DELAYS_SECS.as_slice())
        .bind(REFUND_MAX_ATTEMPTS)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Records a failed compatibility caller's attempt conservatively. A
    /// transport error or exhausted budget cannot prove that no refund exists,
    /// so this never releases money. New callers claim before I/O and use
    /// [`Self::refuse_claimed_refund`] only for a proven first-attempt refusal.
    pub async fn fail_order_refund(
        &self,
        refund_id: Uuid,
        _error: &str,
    ) -> Result<OrderRefund, WalletError> {
        self.retry_order_refund(refund_id).await?;
        self.refund_retry_target(refund_id)
            .await?
            .map(|target| target.refund)
            .ok_or_else(|| WalletError::NotFound("unknown refund".into()))
    }

    /// Releases a refund's reservation inside the caller's transaction:
    /// `provider_pending → released`, the issued note → `reversed`, and the
    /// audit detail records the reason the bound freed.
    async fn release_refund_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        refund: &OrderRefund,
        reason: Option<&str>,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.refunds SET state='released',next_retry_at=NULL,updated_at=now() \
             WHERE refund_id=$1",
        )
        .bind(refund.refund_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.credit_notes SET state='reversed',updated_at=now() \
             WHERE refund_id=$1 AND state='issued'",
        )
        .bind(refund.refund_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT INTO oxsum.finance_audit\
             (audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) \
             VALUES($1,$2,$3,'refund.release',$4,$5,$6)",
        )
        .bind(Uuid::new_v4())
        .bind(refund.business_id)
        .bind(refund.resolved_by.unwrap_or_else(Uuid::nil))
        .bind(refund.refund_id.to_string())
        .bind(json!({
            "orderId": refund.order_id,
            "amountMinor": refund.amount_minor,
            "reason": reason,
        }))
        .bind(Option::<String>::None)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

impl Db {
    /// Applies a classified merchant event to the refund it names — the
    /// `refund_succeeded`/`refund_failed`/`dispute` half of
    /// [`Self::apply_order_event`]'s family. The signed event is the only
    /// terminal truth: `refund_succeeded` books the wallet take-back and
    /// lands `succeeded`; `refund_failed` releases the reservation; a
    /// `dispute` opens its own contested record. The event is marked applied
    /// exactly once, whatever it did — a duplicate or a refund that already
    /// settled answers the standing state.
    ///
    /// `wallet` is the refund organization's ledger — the caller resolves it
    /// from the order, like the fulfillment path does.
    pub async fn apply_refund_event(
        &self,
        wallet: &crate::Wallet,
        receipt: &MerchantEventReceipt,
        on: time::Date,
    ) -> Result<(), WalletError> {
        if receipt.state != "accepted" {
            return Ok(());
        }
        let event = self
            .merchant_event_data(receipt.id)
            .await?
            .ok_or_else(|| invalid("missing stored merchant event"))?;
        match receipt.kind {
            MerchantEventKind::RefundSucceeded => {
                self.settle_order_refund(wallet, &event, receipt, on)
                    .await?;
            }
            MerchantEventKind::RefundFailed => {
                self.fail_refund_event(&event, receipt).await?;
            }
            MerchantEventKind::Dispute => {
                self.record_order_dispute(&event, receipt).await?;
            }
            _ => return Ok(()),
        }
        self.mark_merchant_event_applied(receipt.id).await?;
        Ok(())
    }

    /// The `refund_succeeded` arm: find the refund the provider id names —
    /// the order's own row when the event carries no `refund_id` (a rail that
    /// echoes only the payment) — and book the wallet take-back under the
    /// refund-derived ledger key. A refund the customer already spent refuses
    /// at the ledger (`InsufficientFunds`): the refund still lands
    /// `succeeded` — the PSP's answer is fact — and the audit detail records
    /// the unbooked compensation the ledger could not carry, never a
    /// negative wallet.
    async fn settle_order_refund(
        &self,
        wallet: &crate::Wallet,
        event: &crate::VerifiedMerchantEvent,
        receipt: &MerchantEventReceipt,
        on: time::Date,
    ) -> Result<(), WalletError> {
        let mut tx = self.pool().begin().await?;
        let refund = self.find_refund_for_event(&mut tx, event, receipt).await?;
        let Some(refund) = refund else {
            tx.commit().await?;
            return Ok(());
        };
        drop(tx);
        let late = refund.state != "provider_pending";
        if refund.state == "declined" {
            return Ok(());
        }
        // The order's purpose routes the correction: a `plan_period` refund
        // corrects the journal the order settled — the plan-fee cash was
        // never wallet money, so `refund_debit`'s pool draw has nothing to
        // take back (issue #611). Every other purpose keeps T17's shape.
        let purpose: Option<String> =
            sqlx::query_scalar("SELECT purpose FROM oxsum.payment_orders WHERE order_id=$1")
                .bind(refund.order_id)
                .fetch_optional(self.pool())
                .await?;
        let plan_fee = purpose.as_deref() == Some("plan_period");
        if refund.state == "succeeded" && !plan_fee {
            return Ok(());
        }
        if purpose.as_deref() == Some("bundle") {
            // Cancel before the terminal mark. A crash retries this idempotent
            // burn before recording success, and held releases burn later too.
            let lot: Option<Uuid> = sqlx::query_scalar(
                "SELECT lot_id FROM oxsum.promo_lots WHERE organization_id=$1 \
                 AND source='bundle_promo' AND source_ref=$2",
            )
            .bind(refund.organization_id)
            .bind(refund.order_id.to_string())
            .fetch_optional(self.pool())
            .await?;
            if let Some(lot) = lot {
                self.cancel_promo_lot(wallet, lot, on).await?;
            }
        }
        // The ledger append runs outside any held row lock, keyed on the
        // refund id: a retried or recovered settle replays the standing entry.
        // `FundedReservations` refuses a take-back the purchased pool cannot
        // carry — the customer already spent the refunded credit — which is
        // the "failed compensation" the audit records, never a negative wallet.
        let mut entry_id = if plan_fee {
            None
        } else {
            match wallet
                .refund_debit(
                    &refund.refund_id.to_string(),
                    &format!("refund of order {}", refund.order_id),
                    refund.amount_minor,
                    on,
                )
                .await
            {
                Ok(receipt) => Some(*receipt.entry_id.as_uuid()),
                Err(WalletError::InsufficientFunds) => None,
                Err(error) => return Err(error),
            }
        };
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("order-refund:{}", refund.refund_id))
            .execute(&mut *tx)
            .await?;
        let Some(current) = refund_in(&mut tx, refund.refund_id).await? else {
            tx.commit().await?;
            return Ok(());
        };
        // Terminal states are immutable by the row guard: a late success on a
        // `released`/`failed` refund can only be recorded — the money came
        // back anyway, so the take-back entry (booked above, idempotent) and
        // the audit detail are the whole settlement. Non-terminal states
        // land `succeeded`.
        if !matches!(
            current.state.as_str(),
            "succeeded" | "declined" | "failed" | "released"
        ) {
            sqlx::query(
                "UPDATE oxsum.refunds SET state='succeeded',\
                 provider_refund_id=COALESCE(provider_refund_id,$2),next_retry_at=NULL,updated_at=now() \
                 WHERE refund_id=$1",
            )
            .bind(refund.refund_id)
            .bind(event.refund_id.as_deref())
            .execute(&mut *tx)
            .await?;
        }
        // Plan cash, source allocation, credit note and customer documents
        // share this transaction. Recovery of a succeeded legacy refund follows
        // the same source-bound path instead of returning before its missing leg.
        if plan_fee {
            let allocation = self
                .apply_plan_refund_in(
                    &mut tx,
                    wallet,
                    refund.refund_id,
                    Some(receipt.id),
                    current.state == "succeeded",
                    true,
                    on,
                )
                .await?;
            entry_id = allocation.entry_id;
        }
        // The note applies with its entry id when the take-back booked; when
        // the pool could not carry it, `issued` stays and the audit detail
        // records the compensation the ledger owes but cannot carry.
        if let Some(entry_id) = entry_id.filter(|_| !plan_fee) {
            sqlx::query(
                "UPDATE oxsum.credit_notes SET state='applied',entry_id=$2,updated_at=now() \
                 WHERE refund_id=$1 AND state='issued'",
            )
            .bind(refund.refund_id)
            .bind(entry_id)
            .execute(&mut *tx)
            .await?;
        }
        if entry_id.is_none() || late {
            sqlx::query(
                "INSERT INTO oxsum.finance_audit\
                 (audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) \
                 VALUES($1,$2,$3,'refund.approve',$4,$5,$6) ON CONFLICT DO NOTHING",
            )
            .bind(Uuid::new_v4())
            .bind(refund.business_id)
            .bind(refund.resolved_by.unwrap_or_else(Uuid::nil))
            .bind(refund.refund_id.to_string())
            .bind(json!({
                "orderId": refund.order_id,
                "amountMinor": refund.amount_minor,
                "compensation": if entry_id.is_none() {
                    "insufficientFunds"
                } else {
                    "booked"
                },
                "lateEvent": if late { Some(receipt.event_id.as_str()) } else { None },
                "previousState": refund.state,
            }))
            .bind(format!("refund-settle:{}:{}", refund.refund_id, receipt.id))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Resolves only inside the authenticated order and exact refund terms.
    /// New Hyperswitch body IDs allow an early event to find its own refund;
    /// an unknown supplied ID never falls back to another partial reservation.
    async fn find_refund_for_event(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &crate::VerifiedMerchantEvent,
        receipt: &MerchantEventReceipt,
    ) -> Result<Option<OrderRefund>, WalletError> {
        let Some(order_id) = receipt.payment_id else {
            return Ok(None);
        };
        let rows = if let Some(provider_refund) = event.refund_id.as_deref() {
            sqlx::query(&format!(
                "{REFUND_SELECT} WHERE r.order_id=$1 AND (r.provider_refund_id=$2 OR \
                 (r.provider_refund_id IS NULL AND r.hyperswitch_body_id AND r.refund_id::text=$2)) FOR UPDATE OF r"
            )).bind(order_id).bind(provider_refund).fetch_all(&mut **tx).await?
        } else {
            sqlx::query(&format!(
                "{REFUND_SELECT} WHERE r.order_id=$1 AND r.state='provider_pending' FOR UPDATE OF r"
            ))
            .bind(order_id)
            .fetch_all(&mut **tx)
            .await?
        };
        // A legacy payment-only event is usable only with one unambiguous
        // reservation. Amount, currency and customer must still agree.
        if rows.len() != 1 {
            return Err(WalletError::Conflict(
                "refund event has no unique reservation; reconcile it".into(),
            ));
        }
        let refund = refund_from_row(&rows[0])?;
        if event.organization_id != Some(refund.organization_id)
            || event.amount_minor != Some(refund.display_amount_minor)
            || event.currency.as_deref() != Some(refund.display_currency.as_str())
        {
            return Err(WalletError::Conflict(
                "refund event terms differ from the reservation".into(),
            ));
        }
        Ok(Some(refund))
    }

    /// The `refund_failed` arm: the PSP's definite refusal releases the
    /// reservation — `released` frees the bound, the issued note reverses,
    /// and the audit keeps the provider's reason.
    async fn fail_refund_event(
        &self,
        event: &crate::VerifiedMerchantEvent,
        receipt: &MerchantEventReceipt,
    ) -> Result<(), WalletError> {
        let mut tx = self.pool().begin().await?;
        let Some(refund) = self.find_refund_for_event(&mut tx, event, receipt).await? else {
            tx.commit().await?;
            return Ok(());
        };
        if refund.state == "provider_pending" {
            self.release_refund_in(&mut tx, &refund, Some("provider refund failed"))
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Retain one authenticated case. Non-plan amounts must map exactly from
    /// pinned fiat; incompatible duplicate identities remain visible inbox review.
    pub async fn record_order_dispute(
        &self,
        event: &crate::VerifiedMerchantEvent,
        receipt: &MerchantEventReceipt,
    ) -> Result<(), WalletError> {
        let Some(order_id) = receipt.payment_id else {
            return Ok(());
        };
        let mut tx = self.pool().begin().await?;
        let provider_dispute_id = event
            .refund_id
            .clone()
            .or_else(|| event.provider_payment_id.clone())
            .unwrap_or_else(|| order_id.to_string());
        let purpose: Option<String> =
            sqlx::query_scalar("SELECT purpose FROM oxsum.payment_orders WHERE order_id=$1")
                .bind(order_id)
                .fetch_optional(&mut *tx)
                .await?;
        if purpose.as_deref().is_some_and(|p| p != "plan_period") {
            let claimed: bool =
                sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
                    .bind(format!(
                        "merchant-dispute:{}:{provider_dispute_id}",
                        receipt.merchant_id
                    ))
                    .fetch_one(&mut *tx)
                    .await?;
            if !claimed {
                return Err(WalletError::PoolBusy);
            }
            crate::nonplan_returns::lock_source_in(&mut tx, order_id).await?;
        }
        let Some(order) = refund_order_in(&mut tx, order_id).await? else {
            tx.commit().await?;
            return Ok(());
        };
        let nonplan = order.purpose != "plan_period";
        let amount = if nonplan {
            match crate::nonplan_returns::disputed_minor(
                order.amount_minor,
                order.display_amount_minor,
                &order.display_currency,
                event,
            ) {
                Ok(amount) => amount,
                Err(reason) => {
                    sqlx::query("UPDATE oxsum.merchant_event_inbox SET reason=$2,updated_at=now() WHERE receipt_id=$1 AND state='accepted'")
                        .bind(receipt.id).bind(reason).execute(&mut *tx).await?;
                    tx.commit().await?;
                    return Err(WalletError::Conflict(format!(
                        "dispute source requires review: {reason}"
                    )));
                }
            }
        } else {
            order.amount_minor
        };
        let currency = event.currency.as_deref().unwrap_or(&order.display_currency);
        let standing=sqlx::query("SELECT order_id,organization_id,amount_minor,currency FROM oxsum.disputes WHERE merchant_id=$1 AND provider_dispute_id=$2")
            .bind(receipt.merchant_id).bind(&provider_dispute_id).fetch_optional(&mut *tx).await?;
        if nonplan && let Some(standing) = standing {
            if standing.try_get::<Uuid, _>("order_id")? != order_id
                || standing.try_get::<Uuid, _>("organization_id")? != order.organization_id
                || standing.try_get::<i64, _>("amount_minor")? != amount
                || standing.try_get::<String, _>("currency")? != currency
            {
                sqlx::query("UPDATE oxsum.merchant_event_inbox SET reason='disputeTermsMismatch',updated_at=now() WHERE receipt_id=$1 AND state='accepted'")
                    .bind(receipt.id).execute(&mut *tx).await?;
                tx.commit().await?;
                return Err(WalletError::Conflict(
                    "dispute case has incompatible original terms".into(),
                ));
            }
            tx.commit().await?;
            return Ok(());
        }
        sqlx::query("INSERT INTO oxsum.disputes(dispute_id,order_id,organization_id,merchant_id,provider_dispute_id,amount_minor,currency,state,inbox_receipt_id) VALUES($1,$2,$3,$4,$5,$6,$7,'opened',$8) ON CONFLICT (merchant_id,provider_dispute_id) DO NOTHING")
            .bind(Uuid::new_v4()).bind(order_id).bind(order.organization_id).bind(receipt.merchant_id)
            .bind(provider_dispute_id).bind(amount.max(1)).bind(currency).bind(receipt.id).execute(&mut *tx).await?;
        if !nonplan {
            self.track_journal_dispute_in(&mut tx, order_id, amount)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The retry sweep's work list: `provider_pending` refunds whose next
    /// attempt is due, in bounded pages. This read does not acquire a lease:
    /// callers must use [`Self::claim_refund_attempt`] before provider I/O.
    pub async fn due_refund_retries(&self, limit: i64) -> Result<Vec<OrderRefund>, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let rows = sqlx::query(&format!(
            "{REFUND_SELECT} WHERE r.state='provider_pending' AND r.next_retry_at <= now() \
             ORDER BY r.next_retry_at LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(refund_from_row).collect()
    }

    /// Claims one due recovery attempt before network I/O. The row lease and
    /// attempt budget survive a process crash; concurrent workers cannot spend
    /// the same attempt. Exhaustion retains the reservation for reconciliation.
    pub async fn claim_refund_attempt(
        &self,
        refund_id: Uuid,
    ) -> Result<Option<RefundRetryTarget>, WalletError> {
        let changed = sqlx::query(
            "UPDATE oxsum.refunds SET retry_count=retry_count+1,\
             next_retry_at=CASE WHEN retry_count+1 < $2 THEN now() + \
             (($3::bigint[])[retry_count+1] * interval '1 second') ELSE NULL END,updated_at=now() \
             WHERE refund_id=$1 AND state='provider_pending' AND next_retry_at<=now() AND retry_count<$2"
        ).bind(refund_id).bind(REFUND_MAX_ATTEMPTS).bind(REFUND_RETRY_DELAYS_SECS.as_slice())
            .execute(self.pool()).await?.rows_affected();
        if changed == 0 {
            return Ok(None);
        }
        self.refund_retry_target(refund_id).await
    }

    /// Releases a first claimed attempt only when the adapter proves a refusal
    /// before creation. Later refusals cannot disprove an earlier lost response.
    pub async fn refuse_claimed_refund(&self, refund_id: Uuid) -> Result<bool, WalletError> {
        let mut tx = self.pool().begin().await?;
        let Some(refund) = refund_in(&mut tx, refund_id).await? else {
            return Ok(false);
        };
        let release = refund.state == "provider_pending"
            && refund.retry_count == 1
            && refund.provider_refund_id.is_none();
        if release {
            self.release_refund_in(&mut tx, &refund, Some("provider refused before creation"))
                .await?;
        }
        tx.commit().await?;
        Ok(release)
    }

    /// Everything a refund's PSP retry needs: the refund plus its order's
    /// merchant coordinates and provider payment — the caller resolves the
    /// signing context from the pair.
    pub async fn refund_retry_target(
        &self,
        refund_id: Uuid,
    ) -> Result<Option<RefundRetryTarget>, WalletError> {
        let row = sqlx::query(&format!("{REFUND_SELECT} WHERE r.refund_id=$1"))
            .bind(refund_id)
            .fetch_optional(self.pool())
            .await?;
        let Some(refund) = row.as_ref().map(refund_from_row).transpose()? else {
            return Ok(None);
        };
        let order = sqlx::query(
            "SELECT merchant_id,merchant_version_id,provider_payment_id \
             FROM oxsum.payment_orders WHERE order_id=$1",
        )
        .bind(refund.order_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| invalid("the refund's order is gone"))?;
        Ok(Some(RefundRetryTarget {
            refund,
            merchant_id: order.try_get("merchant_id")?,
            merchant_version_id: order.try_get("merchant_version_id")?,
            provider_payment_id: order.try_get("provider_payment_id")?,
        }))
    }

    /// The business's refunds, newest first — the staff finance list.
    /// `ReportsRead` covers reads; `RefundsWrite` is for the resolve.
    pub async fn business_refunds(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<OrderRefundPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let rows = sqlx::query(&format!(
            "{REFUND_SELECT} WHERE r.business_id=$1 AND ($2::uuid IS NULL OR r.refund_id>$2) \
             ORDER BY r.refund_id LIMIT $3"
        ))
        .bind(business)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut refunds = rows
            .iter()
            .map(refund_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let more = refunds.len() > limit as usize;
        refunds.truncate(limit as usize);
        let next_cursor = if more {
            refunds.last().map(|r| r.refund_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(OrderRefundPage {
            refunds,
            next_cursor,
        })
    }

    /// One refund the business owns — the staff read behind `resolve`.
    pub async fn business_refund(
        &self,
        token: &str,
        refund_id: Uuid,
    ) -> Result<Option<OrderRefund>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let refund = refund_in(&mut tx, refund_id).await?;
        tx.commit().await?;
        Ok(refund.filter(|r| r.business_id == business))
    }

    /// The business's order-linked credit notes — the corrections the refund
    /// flow issues, newest first, as the finance surface lists them.
    pub async fn business_order_credit_notes(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<OrderCreditNotePage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let rows = sqlx::query(
            "SELECT n.credit_note_id,n.organization_id,n.order_id,n.refund_id,n.entry_id,\
                    n.amount_minor,n.reason,n.state,n.created_at \
             FROM oxsum.credit_notes n JOIN oxsum.refunds r ON r.refund_id=n.refund_id \
             WHERE r.business_id=$1 AND n.order_id IS NOT NULL \
               AND ($2::uuid IS NULL OR n.credit_note_id>$2) \
             ORDER BY n.credit_note_id LIMIT $3",
        )
        .bind(business)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut notes = rows
            .iter()
            .map(|row| {
                Ok(OrderCreditNote {
                    credit_note_id: row.try_get("credit_note_id")?,
                    organization_id: row.try_get("organization_id")?,
                    order_id: row.try_get("order_id")?,
                    refund_id: row.try_get("refund_id")?,
                    entry_id: row.try_get("entry_id")?,
                    amount_minor: row.try_get("amount_minor")?,
                    reason: row.try_get("reason")?,
                    state: row.try_get("state")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<OrderCreditNote>, WalletError>>()?;
        let more = notes.len() > limit as usize;
        notes.truncate(limit as usize);
        let next_cursor = if more {
            notes.last().map(|n| n.credit_note_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(OrderCreditNotePage { notes, next_cursor })
    }

    /// The business's disputes — contested charges only, never refunds.
    pub async fn business_disputes(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<OrderDisputePage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let rows = sqlx::query(
            "SELECT d.dispute_id,d.order_id,d.organization_id,d.provider_dispute_id,\
                    d.amount_minor,d.currency,d.state,d.recharge_entry_id,d.created_at,d.updated_at \
             FROM oxsum.disputes d JOIN oxsum.merchant_accounts m USING(merchant_id) \
             WHERE m.business_id=$1 AND ($2::uuid IS NULL OR d.dispute_id>$2) \
             ORDER BY d.dispute_id LIMIT $3",
        )
        .bind(business)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut disputes = rows
            .iter()
            .map(dispute_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let more = disputes.len() > limit as usize;
        disputes.truncate(limit as usize);
        let next_cursor = if more {
            disputes.last().map(|d| d.dispute_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(OrderDisputePage {
            disputes,
            next_cursor,
        })
    }

    /// The business's finance audit — one immutable row per staff financial
    /// action, newest first. `action` narrows to one audited action. This is
    /// the staff console's audit page (T24): the same `finance_audit` the
    /// refund/dispute/journal/statement/reconciliation writes already land.
    /// `after` is the previous page's last audit id; the keyset resolves its
    /// `created_at` back under the same business so the cursor stays opaque.
    pub async fn business_finance_audit(
        &self,
        token: &str,
        after: Option<Uuid>,
        action: Option<&str>,
        limit: i64,
    ) -> Result<FinanceAuditPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        // Resolve the cursor's audit id to its `(created_at, audit_id)` keyset
        // under this business; a foreign or unknown id pages from the top.
        let cursor: Option<(OffsetDateTime, Uuid)> = match after {
            Some(id) => {
                let row = sqlx::query(
                    "SELECT created_at, audit_id FROM oxsum.finance_audit \
                     WHERE business_id=$1 AND audit_id=$2",
                )
                .bind(business)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
                match row {
                    Some(r) => Some((r.try_get("created_at")?, r.try_get("audit_id")?)),
                    None => None,
                }
            }
            None => None,
        };
        // One row over the page is the "there is more" signal. The keyset is
        // (created_at, audit_id) descending — the audit's own ordering.
        let rows = sqlx::query(
            "SELECT audit_id,actor_user_id,action,target,detail,note_id,created_at \
             FROM oxsum.finance_audit \
             WHERE business_id=$1 \
               AND ($2::text IS NULL OR action=$2) \
               AND ($3::timestamptz IS NULL OR (created_at,audit_id)<($3,$4)) \
             ORDER BY created_at DESC, audit_id DESC LIMIT $5",
        )
        .bind(business)
        .bind(action)
        .bind(cursor.map(|c| c.0))
        .bind(cursor.map(|c| c.1))
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut entries = rows
            .iter()
            .map(|row| {
                Ok(FinanceAuditEntry {
                    audit_id: row.try_get("audit_id")?,
                    actor_user_id: row.try_get("actor_user_id")?,
                    action: row.try_get("action")?,
                    target: row.try_get("target")?,
                    detail: row.try_get("detail")?,
                    note_id: row.try_get("note_id")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<FinanceAuditEntry>, WalletError>>()?;
        let more = entries.len() > limit as usize;
        entries.truncate(limit as usize);
        let next_cursor = if more {
            entries.last().map(|e| e.audit_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(FinanceAuditPage {
            entries,
            next_cursor,
        })
    }
}

/// One immutable `finance_audit` row as the audit page answers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinanceAuditEntry {
    /// Audit row identifier.
    pub audit_id: Uuid,
    /// Staff user who made the decision.
    pub actor_user_id: Uuid,
    /// Dotted action name, such as `refund.approve`.
    pub action: String,
    /// Identifier of the affected resource.
    pub target: String,
    /// Action-specific detail, including the reason and amounts.
    pub detail: serde_json::Value,
    /// Credit note the action produced, when it produced one.
    pub note_id: Option<Uuid>,
    /// When the audit row was written.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One page of the business's finance audit.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinanceAuditPage {
    /// Audit rows in this page, newest first.
    pub entries: Vec<FinanceAuditEntry>,
    /// The last row's audit id; the next call resolves it back to the
    /// `(created_at, audit_id)` keyset against the same business.
    pub next_cursor: Option<Uuid>,
}

fn dispute_from_row(row: &PgRow) -> Result<OrderDispute, WalletError> {
    Ok(OrderDispute {
        dispute_id: row.try_get("dispute_id")?,
        order_id: row.try_get("order_id")?,
        organization_id: row.try_get("organization_id")?,
        provider_dispute_id: row.try_get("provider_dispute_id")?,
        amount_minor: row.try_get("amount_minor")?,
        currency: row.try_get("currency")?,
        state: row.try_get("state")?,
        recharge_entry_id: row.try_get("recharge_entry_id")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// What [`Db::refund_retry_target`] returns — the reservation plus the
/// merchant coordinates its PSP call needs.
pub struct RefundRetryTarget {
    /// The refund reservation to re-issue.
    pub refund: OrderRefund,
    /// Merchant account whose credentials sign the call.
    pub merchant_id: Uuid,
    /// Merchant configuration version to sign with.
    pub merchant_version_id: Uuid,
    /// PSP payment the refund points at.
    pub provider_payment_id: Option<String>,
}

/// A dispute outcome as the staff resolution or the provider event spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DisputeResolution {
    /// Evidence was requested — the dispute is being contested.
    EvidenceDue,
    /// The charge stands — the platform won; no money moves.
    Won,
    /// The chargeback stands — the platform lost; the drawn receivable
    /// re-charges through the dispute's own ledger entry.
    Lost,
    /// Withdrawn or settled without arbitration.
    Closed,
}
impl DisputeResolution {
    fn as_str(self) -> &'static str {
        match self {
            Self::EvidenceDue => "evidence_due",
            Self::Won => "won",
            Self::Lost => "lost",
            Self::Closed => "closed",
        }
    }
}
