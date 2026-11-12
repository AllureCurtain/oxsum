//! Return confirmed cash once against retained repayment allocations, never wallet.

use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use super::{apply, evidence, lock_owner_in};
use crate::{Db, MerchantEventReceipt, Wallet, WalletError, error};

/// The caller owns subscription/plans and dispute decision; this never commits.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reverse_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    order_id: Uuid,
    kind: &str,
    source: Uuid,
    receipt: Option<Uuid>,
    amount: i64,
    on: Date,
) -> Result<Vec<evidence::Effect>, WalletError> {
    if !matches!(kind, "refund" | "dispute") || amount <= 0 {
        return Err(error::invalid("invalid statement return"));
    }
    let order = crate::payment_orders::order_in(tx, order_id)
        .await?
        .ok_or_else(|| error::invalid("statement source order missing"))?;
    let terms = evidence::validate(&order)?;
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(order.organization_id)
            .fetch_one(&mut **tx)
            .await?;
    if tenant != wallet.tenant_id() {
        return Err(WalletError::Forbidden(
            "statement return wallet differs".into(),
        ));
    }
    let retained = evidence::effects_in(tx, wallet, order_id).await?;
    let existing: Vec<_> = retained
        .iter()
        .filter(|e| e.kind == kind && e.source == source)
        .cloned()
        .collect();
    if !existing.is_empty() {
        if existing.iter().try_fold(0i64, |sum, e| {
            sum.checked_add(e.cash)
                .ok_or_else(|| error::invalid("statement return overflows"))
        })? != amount
        {
            return Err(error::invalid("statement return amount changed"));
        }
        return Ok(existing);
    }
    if order.status != "fulfilled" {
        return Err(WalletError::Conflict(
            "fulfill the confirmed statement payment before returning its cash".into(),
        ));
    }
    let mut left = amount;
    let mut effects = Vec::new();
    let mut restored = 0i64;
    for original in retained.iter().filter(|e| e.kind == "payment") {
        let used = retained
            .iter()
            .filter(|e| e.original == Some(original.id))
            .try_fold(0i64, |sum, e| {
                sum.checked_add(e.cash)
                    .ok_or_else(|| error::invalid("statement returned total overflows"))
            })?;
        let take = original
            .cash
            .checked_sub(used)
            .ok_or_else(|| error::invalid("statement retained cash overflows"))?
            .max(0)
            .min(left);
        if take == 0 {
            continue;
        }
        let journal_id = original
            .journal
            .ok_or_else(|| error::invalid("statement source journal missing"))?;
        let journal = crate::journals::journal_in(tx, journal_id)
            .await?
            .ok_or_else(|| error::invalid("statement source journal missing"))?;
        let returned_receivable = retained
            .iter()
            .filter(|e| e.original == Some(original.id))
            .try_fold(0i64, |sum, e| {
                sum.checked_add(e.receivable)
                    .ok_or_else(|| error::invalid("statement return overflows"))
            })?;
        let document_state: String = sqlx::query_scalar(
            "SELECT state FROM oxsum.plan_statements WHERE statement_id=$1 FOR UPDATE",
        )
        .bind(terms.statement_id)
        .fetch_one(&mut **tx)
        .await?;
        let receivable = if journal.state != crate::JournalState::Void && document_state != "void" {
            take.min((original.receivable - returned_receivable).max(0))
                .min(journal.settled_minor)
        } else {
            0
        };
        let payable = if original.payable > 0 {
            take.min((original.payable - used).max(0))
        } else if journal.state == crate::JournalState::Void {
            let allocation = sqlx::query(
                "SELECT amount_minor,entry_id,entry_hash FROM oxsum.statement_void_allocations \
                 WHERE effect_id=$1 AND journal_id=$2",
            )
            .bind(original.id)
            .bind(journal_id)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some(row) = allocation {
                let entry: Uuid = row.try_get("entry_id")?;
                if wallet.verified_source_hash_in(tx, entry).await?.as_deref()
                    != Some(row.try_get::<String, _>("entry_hash")?.as_str())
                {
                    return Err(error::invalid("void payable source is unverified"));
                }
                let prior = retained
                    .iter()
                    .filter(|e| e.original == Some(original.id))
                    .try_fold(0i64, |sum, e| {
                        sum.checked_add(e.payable)
                            .ok_or_else(|| error::invalid("void return overflows"))
                    })?;
                take.min((row.try_get::<i64, _>("amount_minor")? - prior).max(0))
            } else {
                0
            }
        } else {
            0
        }
        .min(take - receivable);
        let effect = evidence::insert_in(
            tx,
            wallet,
            &order,
            kind,
            source,
            Some(journal_id),
            Some(original.id),
            receipt,
            take,
            receivable,
            payable,
            take - receivable - payable,
            on,
        )
        .await?;
        if receivable > 0 {
            sqlx::query("UPDATE oxsum.invoice_journals SET settled_minor=settled_minor-$2,state='posted',paid_at=NULL,updated_at=now() WHERE journal_id=$1")
                .bind(journal_id).bind(receivable).execute(&mut **tx).await?;
            apply::line_in(tx, terms.statement_id, receivable, effect.id).await?;
            restored = restored
                .checked_add(receivable)
                .ok_or_else(|| error::invalid("restored statement debt overflows"))?;
        }
        apply::accounting_in(tx, &effect, on).await?;
        effects.push(effect);
        left -= take;
    }
    if left > 0 {
        effects.push(
            evidence::insert_in(
                tx, wallet, &order, kind, source, None, None, receipt, left, 0, 0, left, on,
            )
            .await?,
        );
    }
    if restored > 0 {
        sqlx::query("UPDATE oxsum.plan_statements SET paid_minor=paid_minor-$2,state=CASE WHEN state='paid' THEN 'issued' ELSE state END,paid_at=NULL,updated_at=now() WHERE statement_id=$1 AND state<>'void'")
            .bind(terms.statement_id).bind(restored).execute(&mut **tx).await?;
        for s in &terms.allocations {
            crate::plan_debt::refresh_in(tx, s.customer_plan_id).await?;
        }
    }
    Ok(effects)
}

pub(crate) async fn refund(
    db: &Db,
    wallet: &Wallet,
    refund_id: Uuid,
    receipt: &MerchantEventReceipt,
    on: Date,
) -> Result<(), WalletError> {
    let mut tx = db.pool().begin().await?;
    let event = crate::merchant_events::data_in(&mut tx, receipt.id)
        .await?
        .ok_or_else(|| error::invalid("refund event is missing"))?;
    let scope =
        sqlx::query("SELECT organization_id,order_id FROM oxsum.refunds WHERE refund_id=$1")
            .bind(refund_id)
            .fetch_one(&mut *tx)
            .await?;
    lock_owner_in(&mut tx, scope.try_get("organization_id")?, db.pool()).await?;
    let order_id = scope.try_get("order_id")?;
    crate::nonplan_returns::lock_source_in(&mut tx, order_id).await?;
    let row=sqlx::query("SELECT r.*,n.credit_note_id,n.state AS note_state FROM oxsum.refunds r LEFT JOIN oxsum.credit_notes n USING(refund_id) WHERE r.refund_id=$1 FOR UPDATE OF r")
        .bind(refund_id).fetch_one(&mut *tx).await?;
    if event.organization_id != Some(row.try_get("organization_id")?)
        || event.payment_id != Some(order_id)
        || event.amount_minor != Some(row.try_get("display_amount_minor")?)
        || event.currency.as_deref() != Some(row.try_get::<String, _>("display_currency")?.as_str())
        || receipt.kind != crate::MerchantEventKind::RefundSucceeded
    {
        return Err(error::invalid(
            "statement refund event differs from reservation",
        ));
    }
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_event_inbox e JOIN oxsum.payment_orders p ON p.order_id=$2 WHERE e.receipt_id=$1 AND e.merchant_id=p.merchant_id AND e.version_id=p.merchant_version_id AND e.state IN ('accepted','applied'))").bind(receipt.id).bind(order_id).fetch_one(&mut *tx).await?;
    let reference: Option<String> = row.try_get("provider_refund_id")?;
    let body_id: bool = row.try_get("hyperswitch_body_id")?;
    if !valid
        || event.refund_id.as_deref().is_some_and(|id| {
            Some(id) != reference.as_deref()
                && !(reference.is_none() && body_id && id == refund_id.to_string())
        })
    {
        return Err(error::invalid(
            "statement refund receipt is not bound to this reservation",
        ));
    }
    let note: Option<Uuid> = row.try_get("credit_note_id")?;
    if note.is_none() || row.try_get::<Option<Uuid>, _>("resolved_by")?.is_none() {
        // A provider-side refund that has no finance authorization cannot be
        // auto-applied — but a `Conflict` here leaves the inbox event `accepted`
        // and `recoverable_merchant_events` re-applies it forever. The decision
        // is final (the refund needs a human, not a retry), so stamp the reason
        // and let `apply_refund_event` mark the event applied; the refund row
        // stays `provider_pending` for an operator to resolve.
        sqlx::query("UPDATE oxsum.merchant_event_inbox SET reason='unauthorizedStatementRefund',updated_at=now() WHERE receipt_id=$1 AND state='accepted'")
            .bind(receipt.id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(());
    }
    let effects = reverse_in(
        &mut tx,
        wallet,
        order_id,
        "refund",
        refund_id,
        Some(receipt.id),
        row.try_get("amount_minor")?,
        on,
    )
    .await?;
    let state: String = row.try_get("state")?;
    if state == "provider_pending" {
        sqlx::query("UPDATE oxsum.refunds SET state='succeeded',provider_refund_id=COALESCE(provider_refund_id,$2),next_retry_at=NULL,updated_at=now() WHERE refund_id=$1").bind(refund_id).bind(event.refund_id).execute(&mut *tx).await?;
    }
    // Reversed notes remain historical; immutable effects retain late cash.
    sqlx::query("UPDATE oxsum.credit_notes SET state='applied',entry_id=$2 WHERE credit_note_id=$1 AND state='issued'").bind(note).bind(effects.first().map(|e|e.entry)).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES($1,$2,$3,'statement.refund',$4,$5,$6) ON CONFLICT DO NOTHING")
        .bind(Uuid::new_v4()).bind(row.try_get::<Uuid,_>("business_id")?).bind(row.try_get::<Option<Uuid>,_>("resolved_by")?.ok_or_else(||error::invalid("refund authorization actor is missing"))?).bind(refund_id.to_string()).bind(serde_json::json!({"orderId":order_id,"allocations":effects,"previousState":state})).bind(format!("statement-refund/{refund_id}")).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
