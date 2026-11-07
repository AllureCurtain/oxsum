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
//! record of its own (`opened → evidence_due → won | lost | closed`). A lost
//! dispute re-charges the drawn receivable through [`crate::Wallet`]'s
//! `repay_credit` shape under the dispute's own ledger key, so the ledger
//! never confuses a contested reversal with a granted refund.
//!
//! The promo leg of a `bundle` order is never refundable cash: the bound is
//! `amount_minor` alone, and cancelling the promo lot is T18's seam — a
//! refund never converts grant into money.

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
const REFUND_RETRY_DELAYS_SECS: [i64; 5] = [30, 120, 300, 900, 3600];

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
    Approve,
    Decline,
}

/// One refund, as the customer and the business surfaces read it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderRefund {
    pub refund_id: Uuid,
    pub order_id: Uuid,
    pub organization_id: Uuid,
    /// The credit amount asked back, in ledger minor units.
    pub amount_minor: i64,
    /// The fiat figure the PSP is asked to return.
    pub display_amount_minor: i64,
    pub display_currency: String,
    pub reason: String,
    /// `requested → provider_pending → succeeded | failed | released`; the
    /// request's other exit is `declined`.
    pub state: String,
    pub provider_refund_id: Option<String>,
    pub retry_count: i32,
    /// The business the refund's audit rows belong to — internal scoping,
    /// never serialized to the customer.
    #[serde(skip_serializing)]
    pub business_id: Uuid,
    /// The staff user who resolved it; `None` while requested.
    #[serde(skip_serializing)]
    pub resolved_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_retry_at: Option<OffsetDateTime>,
    /// The credit note the approval issued, once it exists.
    pub note_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderRefundPage {
    pub refunds: Vec<OrderRefund>,
    pub next_cursor: Option<Uuid>,
}

/// A contested charge as the dispute record tracks it — deliberately not a
/// refund: a dispute is a claim the provider arbitrates, and its `lost`
/// outcome re-charges the receivable rather than granting credit back.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderDispute {
    pub dispute_id: Uuid,
    pub order_id: Uuid,
    pub organization_id: Uuid,
    pub provider_dispute_id: String,
    /// The fiat figure contested, from the signed event.
    pub amount_minor: i64,
    pub currency: String,
    /// `opened → evidence_due → won | lost | closed`.
    pub state: String,
    pub recharge_entry_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderDisputePage {
    pub disputes: Vec<OrderDispute>,
    pub next_cursor: Option<Uuid>,
}

/// One finance-resolution audit row: who decided, on what, under which
/// reason, naming the customer-visible note it produced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinanceAudit {
    pub audit_id: Uuid,
    pub actor_user_id: Uuid,
    pub action: String,
    pub target: String,
    pub detail: serde_json::Value,
    pub note_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The credit-note projection T17 adds: an order-linked correction walks
/// `issued → applied` as its ledger entry lands, `reversed` when the refund
/// releases without applying.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderCreditNote {
    pub credit_note_id: Uuid,
    pub organization_id: Uuid,
    pub order_id: Uuid,
    pub refund_id: Uuid,
    /// The take-back entry once `applied`; `None` while issued-unapplied.
    pub entry_id: Option<Uuid>,
    pub amount_minor: i64,
    pub reason: String,
    pub state: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderCreditNotePage {
    pub notes: Vec<OrderCreditNote>,
    pub next_cursor: Option<Uuid>,
}

// The select list joins the refund's credit note (at most one — a resolution
// issues exactly one) so every read carries `note_id` without a second query.
const REFUND_SELECT: &str = "SELECT r.refund_id,r.order_id,r.organization_id,r.business_id,\
     r.amount_minor,r.display_amount_minor,r.display_currency,r.reason,r.state,\
     r.provider_refund_id,r.provider_payment_id,r.retry_count,r.next_retry_at,\
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
        retry_count: row.try_get("retry_count")?,
        business_id: row.try_get("business_id")?,
        resolved_by: row.try_get("resolved_by")?,
        next_retry_at: row.try_get("next_retry_at")?,
        note_id: row.try_get("note_id")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// The order columns a refund decision reads: the frozen terms, the lifecycle
/// and the merchant coordinates the PSP call needs.
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
    })
}

const REFUND_ORDER_COLS: &str = "business_id,organization_id,amount_minor,\
     display_amount_minor,display_currency,provider_payment_id,status";

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
/// the provider_pending reservations still holding a bound — `requested`
/// rows don't count until a resolution reserves them, and declined/failed/
/// released rows have freed theirs.
async fn committed_minor_in(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<i64, WalletError> {
    let committed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.refunds \
         WHERE order_id=$1 AND state IN ('requested','provider_pending','succeeded')",
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
         WHERE order_id=$1 AND state IN ('requested','provider_pending','succeeded')",
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
    pub amount_minor: Option<i64>,
    pub reason: String,
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
        // Only money that actually arrived may refund: `paid` or `fulfilled`
        // — a `provider_pending` order has nothing to give back yet.
        if !matches!(order.status.as_str(), "paid" | "fulfilled") {
            return Err(WalletError::Conflict(format!(
                "a {} payment order has nothing refundable",
                order.status
            )));
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
              requested_by,request_key) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,'requested',$9,$10,$11)",
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
             WHERE order_id=$1 AND organization_id=$2 AND status IN ('paid','fulfilled')",
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
             WHERE order_id=$1 AND state IN ('requested','provider_pending','succeeded')",
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
                // Re-check the bound inside the locked transaction: refunds
                // resolved since the request was written may have consumed
                // the remainder this one was sized against.
                let order = refund_order_in(&mut tx, existing.order_id)
                    .await?
                    .ok_or_else(|| invalid("the refund's order is gone"))?;
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
    /// dedup settle it. `refund_id` doubles as the provider's idempotency
    /// key, so a re-issued call can never mint a second refund.
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

    /// Marks a provider call's outcome without a terminal answer: a timeout
    /// or transport failure leaves `provider_pending` and schedules the next
    /// attempt on the backoff; a definite refusal past the attempt budget is
    /// [`Self::release_order_refund`]'s job — this only reschedules.
    pub async fn retry_order_refund(&self, refund_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.refunds SET retry_count=retry_count+1,\
             next_retry_at=now() + ($2::bigint * interval '1 second'),updated_at=now() \
             WHERE refund_id=$1 AND state='provider_pending' \
               AND retry_count + 1 < $3",
        )
        .bind(refund_id)
        .bind(REFUND_RETRY_DELAYS_SECS[0])
        .bind(REFUND_MAX_ATTEMPTS)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Schedules the next provider attempt inside the attempt budget, or
    /// releases the reservation when the budget ran out — the
    /// definite-failure exit: `released` frees the bound and `reverses` the
    /// standing credit note, with the audit trail recording why.
    pub async fn fail_order_refund(
        &self,
        refund_id: Uuid,
        error: &str,
    ) -> Result<OrderRefund, WalletError> {
        let mut tx = self.pool().begin().await?;
        let Some(existing) = refund_in(&mut tx, refund_id).await? else {
            return Err(WalletError::NotFound("unknown refund".into()));
        };
        if existing.state != "provider_pending" {
            tx.commit().await?;
            return Ok(existing);
        }
        if existing.retry_count + 1 < REFUND_MAX_ATTEMPTS {
            let delay = REFUND_RETRY_DELAYS_SECS
                [(existing.retry_count as usize + 1).min(REFUND_RETRY_DELAYS_SECS.len() - 1)];
            sqlx::query(
                "UPDATE oxsum.refunds SET retry_count=retry_count+1,\
                 next_retry_at=now() + ($2::bigint * interval '1 second'),updated_at=now() \
                 WHERE refund_id=$1",
            )
            .bind(refund_id)
            .bind(delay)
            .execute(&mut *tx)
            .await?;
        } else {
            self.release_refund_in(&mut tx, &existing, Some(error))
                .await?;
        }
        let standing = refund_in(&mut tx, refund_id)
            .await?
            .ok_or_else(|| invalid("the refund did not land"))?;
        tx.commit().await?;
        Ok(standing)
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
        if matches!(refund.state.as_str(), "succeeded" | "declined") {
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
        // A plan-fee refund's correction lands on the journal the order
        // settled: the note's `discount` line, the `Discounts → Cash|Receivables`
        // entry and the note's `applied` mark in this same transaction (issue
        // #611). A journal not written yet — a mid-period refund — is picked
        // up by the close's own credit-note scan instead.
        if plan_fee {
            let note = sqlx::query_scalar::<_, Uuid>(
                "SELECT credit_note_id FROM oxsum.credit_notes \
                 WHERE refund_id=$1 AND state='issued'",
            )
            .bind(refund.refund_id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(note_id) = note {
                let applied = self
                    .apply_refund_to_journal_in(
                        &mut tx,
                        wallet,
                        refund.order_id,
                        refund.refund_id,
                        note_id,
                        refund.amount_minor,
                        &refund.reason,
                        on,
                    )
                    .await?;
                if applied.is_some() {
                    // The journal path applies the note itself — the generic
                    // apply below is skipped so the entry id it records is the
                    // journal correction's, not a phantom take-back.
                    entry_id = None;
                }
            }
        }
        // The note applies with its entry id when the take-back booked; when
        // the pool could not carry it, `issued` stays and the audit detail
        // records the compensation the ledger owes but cannot carry.
        if let Some(entry_id) = entry_id {
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

    /// The refund an event names: its own `provider_refund_id` when the event
    /// carries one, else the order's live `provider_pending` row — a rail
    /// that reports only the payment settles the reservation it already has.
    async fn find_refund_for_event(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &crate::VerifiedMerchantEvent,
        receipt: &MerchantEventReceipt,
    ) -> Result<Option<OrderRefund>, WalletError> {
        if let Some(provider_refund) = event.refund_id.as_deref() {
            let row = sqlx::query(&format!(
                "{REFUND_SELECT} WHERE r.provider_refund_id=$1 FOR UPDATE OF r"
            ))
            .bind(provider_refund)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some(row) = row {
                return refund_from_row(&row).map(Some);
            }
        }
        // Fall back to the order the binding classified: one live
        // provider_pending refund on it is the reservation this event
        // settles.
        let Some(order_id) = receipt.payment_id else {
            return Ok(None);
        };
        let row = sqlx::query(&format!(
            "{REFUND_SELECT} WHERE r.order_id=$1 AND r.state='provider_pending' \
             ORDER BY r.created_at LIMIT 1 FOR UPDATE OF r"
        ))
        .bind(order_id)
        .fetch_optional(&mut **tx)
        .await?;
        row.as_ref().map(refund_from_row).transpose()
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

    /// The `dispute` arm: a contested charge opens its own record — never a
    /// refund — deduped on `(merchant_id, provider_dispute_id)` so a replayed
    /// or re-delivered event keeps the standing row. The disputed figure is
    /// the event's fiat amount, or the order's own when the event carries
    /// none.
    pub async fn record_order_dispute(
        &self,
        event: &crate::VerifiedMerchantEvent,
        receipt: &MerchantEventReceipt,
    ) -> Result<(), WalletError> {
        let Some(order_id) = receipt.payment_id else {
            return Ok(());
        };
        let mut tx = self.pool().begin().await?;
        let Some(order) = refund_order_in(&mut tx, order_id).await? else {
            tx.commit().await?;
            return Ok(());
        };
        let provider_dispute_id = event
            .refund_id
            .clone()
            .or_else(|| event.provider_payment_id.clone())
            .unwrap_or_else(|| order_id.to_string());
        // The contested amount is the order's funded credit — `amount_minor`,
        // never the display figure the event echoes: a `lost` re-charge books
        // ledger-minor through `dispute-recharge`, and the credit is the
        // receivable that restores.
        let amount = order.amount_minor;
        sqlx::query(
            "INSERT INTO oxsum.disputes\
             (dispute_id,order_id,organization_id,merchant_id,provider_dispute_id,\
              amount_minor,currency,state,inbox_receipt_id) \
             VALUES($1,$2,$3,$4,$5,$6,$7,'opened',$8) ON CONFLICT (merchant_id,provider_dispute_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(order_id)
        .bind(order.organization_id)
        .bind(receipt.merchant_id)
        .bind(provider_dispute_id)
        .bind(amount.max(1))
        .bind(event.currency.as_deref().unwrap_or(&order.display_currency))
        .bind(receipt.id)
        .execute(&mut *tx)
        .await?;
        // A plan-fee order's dispute contests the journal it settled: the
        // contested figure tracks on the journal from the moment the record
        // opens, so the staff read never shows a silently-contested bill
        // (issue #611).
        let purpose: Option<String> =
            sqlx::query_scalar("SELECT purpose FROM oxsum.payment_orders WHERE order_id=$1")
                .bind(order_id)
                .fetch_optional(&mut *tx)
                .await?;
        if purpose.as_deref() == Some("plan_period") {
            self.track_journal_dispute_in(&mut tx, order_id, amount)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The retry sweep's work list: `provider_pending` refunds whose next
    /// attempt is due, in bounded pages. The refund's own row serializes a
    /// re-issue against the event path, so two workers racing one refund can
    /// never mint a second provider call — the provider idempotency key is
    /// the refund id.
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
    pub audit_id: Uuid,
    pub actor_user_id: Uuid,
    pub action: String,
    pub target: String,
    pub detail: serde_json::Value,
    pub note_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinanceAuditPage {
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
    pub refund: OrderRefund,
    pub merchant_id: Uuid,
    pub merchant_version_id: Uuid,
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

impl Db {
    /// The staff resolution on an open dispute — `RefundsWrite`, audited like
    /// a refund's. `won`/`closed` end the record; `evidence_due` moves it to
    /// the contesting state; `lost` re-charges the drawn receivable through
    /// [`crate::Wallet`]'s `repay_credit` shape under the dispute's own
    /// ledger key — `dispute-recharge/<dispute_id>` — so the customer-visible
    /// ledger reads a contested-charge restoration, distinct from a granted
    /// refund. A re-charge the line no longer carries is refused rather than
    /// booked into a pool the outcome forbids.
    ///
    /// `wallet` is the dispute organization's ledger. Replaying a resolved
    /// dispute answers the standing row — the entry's own idempotency makes
    /// a re-run a single charge.
    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_order_dispute(
        &self,
        wallet: &crate::Wallet,
        token: &str,
        dispute_id: Uuid,
        resolution: DisputeResolution,
        reason: Option<&str>,
        idempotency_key: &str,
        on: time::Date,
    ) -> Result<OrderDispute, WalletError> {
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
        let row = sqlx::query(
            "SELECT d.dispute_id,d.order_id,d.organization_id,d.provider_dispute_id,\
                    d.amount_minor,d.currency,d.state,d.recharge_entry_id,d.created_at,d.updated_at \
             FROM oxsum.disputes d JOIN oxsum.merchant_accounts m USING(merchant_id) \
             WHERE d.dispute_id=$1 AND m.business_id=$2 FOR UPDATE OF d",
        )
        .bind(dispute_id)
        .bind(business)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(dispute) = row.as_ref().map(dispute_from_row).transpose()? else {
            return Err(WalletError::NotFound("unknown dispute".into()));
        };
        if matches!(dispute.state.as_str(), "won" | "lost" | "closed") {
            tx.commit().await?;
            return Ok(dispute);
        }
        let state = resolution.as_str();
        tx.commit().await?;
        // The order's purpose routes the re-charge: a `plan_period` dispute
        // restores the journal's receivable (`debit Receivables / credit
        // Cash`, journal `paid → posted`) — the contested plan fee is owed
        // again as past-due, never a wallet line's debt (issue #611).
        let purpose: Option<String> =
            sqlx::query_scalar("SELECT purpose FROM oxsum.payment_orders WHERE order_id=$1")
                .bind(dispute.order_id)
                .fetch_optional(self.pool())
                .await?;
        let plan_fee = purpose.as_deref() == Some("plan_period");
        // The re-charge runs outside the held row lock, keyed on the dispute
        // id: a retried resolve replays the standing entry. `repay_credit`
        // refuses outright over the outstanding draw rather than capping, so
        // the recharge is clamped to what the line still carries — a dispute
        // contested past the receivable restores the draw, not the full order.
        let mut recharged = None;
        let mut recharge_entry = None;
        let recharge = if resolution == DisputeResolution::Lost && !plan_fee {
            let drawn = wallet.credit_used().await?;
            if drawn <= 0 {
                return Err(invalid("a lost dispute has no drawn receivable to restore"));
            }
            let amount = dispute.amount_minor.min(drawn);
            recharged = Some(amount);
            Some(wallet.dispute_recharge(&dispute_id, amount, on).await?)
        } else {
            None
        };
        if let Some(receipt) = &recharge {
            recharge_entry = Some(*receipt.entry_id.as_uuid());
        }
        if resolution == DisputeResolution::Lost && plan_fee {
            // Restore the journal's receivable inside a fresh transaction —
            // the entry posts under the dispute's own key so a replay is a
            // no-op, and the journal walks `paid → posted` with the contested
            // figure already tracked. The dispute row names the same
            // derived entry id the wallet-funded arm would record.
            let mut jtx = self.pool().begin().await?;
            let restored = self
                .recharge_journal_dispute_in(
                    &mut jtx,
                    wallet,
                    dispute.order_id,
                    dispute_id,
                    dispute.amount_minor,
                    on,
                )
                .await?;
            jtx.commit().await?;
            if restored.is_none() {
                return Err(invalid(
                    "a lost dispute has no journal receivable to restore",
                ));
            }
            recharged = Some(dispute.amount_minor);
            recharge_entry =
                Some(*crate::entry_id_for(&format!("dispute-recharge/{dispute_id}")).as_uuid());
        }
        let amount_minor = dispute.amount_minor;
        let mut tx = self.pool().begin().await?;
        let audit_id = Uuid::new_v4();
        sqlx::query(
            "UPDATE oxsum.disputes SET state=$2,recharge_entry_id=$3,resolved_by=$4,\
             audit_ref=$5,updated_at=now() WHERE dispute_id=$1",
        )
        .bind(dispute_id)
        .bind(state)
        .bind(recharge_entry)
        .bind(actor.user_id)
        .bind(audit_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO oxsum.finance_audit\
             (audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) \
             VALUES($1,$2,$3,'dispute.resolve',$4,$5,$6)",
        )
        .bind(audit_id)
        .bind(business)
        .bind(actor.user_id)
        .bind(dispute_id.to_string())
        .bind(json!({
            "orderId": dispute.order_id,
            "resolution": state,
            "contestedMinor": amount_minor,
            "rechargedMinor": recharged,
            "currency": dispute.currency,
            "rechargeEntryId": recharge_entry,
            "reason": reason,
        }))
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await?;
        let resolved = sqlx::query(
            "SELECT d.dispute_id,d.order_id,d.organization_id,d.provider_dispute_id,\
                    d.amount_minor,d.currency,d.state,d.recharge_entry_id,d.created_at,d.updated_at \
             FROM oxsum.disputes d WHERE d.dispute_id=$1",
        )
        .bind(dispute_id)
        .fetch_one(&mut *tx)
        .await?;
        let dispute = dispute_from_row(&resolved)?;
        tx.commit().await?;
        Ok(dispute)
    }
}
