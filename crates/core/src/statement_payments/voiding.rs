//! Exact repayment ownership of a financially voided journal's refund payable.

use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{InvoiceJournal, Wallet, WalletError, error};

/// Widen the original void only for canonically proven, single-source documents.
/// A mixed document keeps its explicit finance-review refusal; its unrelated
/// journal's charges must never disappear as a side effect of this void.
pub(crate) async fn documents_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    journal: &InvoiceJournal,
) -> Result<Option<Vec<Uuid>>, WalletError> {
    let repaid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.statement_payment_effects WHERE journal_id=$1 AND receivable_minor>0)")
        .bind(journal.journal_id).fetch_one(&mut **tx).await?;
    if !repaid {
        return Ok(None);
    }
    crate::financial_reconciliation::debt_sources::verify_void_in(
        tx,
        journal.organization_id,
        wallet.tenant_id(),
    )
    .await?;
    let ids:Vec<Uuid> = sqlx::query_scalar("SELECT s.statement_id FROM oxsum.plan_statements s WHERE s.organization_id=$2 AND (s.journal_id=$1 OR EXISTS(SELECT 1 FROM oxsum.statement_payment_effects e WHERE e.statement_id=s.statement_id AND e.journal_id=$1)) ORDER BY s.statement_id FOR UPDATE")
        .bind(journal.journal_id).bind(journal.organization_id).fetch_all(&mut **tx).await?;
    for id in &ids {
        let unrelated:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.plan_statements s WHERE s.statement_id=$2 AND (s.journal_id IS DISTINCT FROM $1 OR s.replaces_statement_id IS NOT NULL OR s.state='open')) OR EXISTS(SELECT 1 FROM oxsum.plan_statement_lines l JOIN oxsum.disputes d ON d.dispute_id=l.source_ref JOIN oxsum.plan_periods p ON p.authorization_kind='payment_order' AND p.authorization_ref=d.order_id JOIN oxsum.invoice_journals j ON j.plan_period_id=p.plan_period_id WHERE l.statement_id=$2 AND l.kind='dispute_chargeback' AND j.journal_id<>$1) OR EXISTS(SELECT 1 FROM oxsum.statement_payment_effects WHERE statement_id=$2 AND receivable_minor>0 AND journal_id<>$1)")
            .bind(journal.journal_id).bind(id).fetch_one(&mut **tx).await?;
        if unrelated {
            return Err(WalletError::Conflict("mixed-source statement requires source-specific financial reconciliation before void".into()));
        }
    }
    // A foreign wrapper can contain this journal's chargeback without a repayment.
    let external:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.plan_statement_lines l JOIN oxsum.disputes d ON d.dispute_id=l.source_ref JOIN oxsum.plan_periods p ON p.authorization_kind='payment_order' AND p.authorization_ref=d.order_id JOIN oxsum.invoice_journals j ON j.plan_period_id=p.plan_period_id WHERE j.journal_id=$1 AND l.kind='dispute_chargeback' AND NOT(l.statement_id=ANY($2)))")
        .bind(journal.journal_id).bind(&ids).fetch_one(&mut **tx).await?;
    if external {
        return Err(WalletError::Conflict(
            "mixed-source statement requires source-specific financial reconciliation before void"
                .into(),
        ));
    }
    Ok(Some(ids))
}

/// Pin each repayment's remaining AR share to this exact, verified void entry.
pub(crate) async fn allocate_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    journal: &InvoiceJournal,
    entry: Option<Uuid>,
) -> Result<(), WalletError> {
    let rows=sqlx::query("SELECT p.effect_id,p.receivable_minor-COALESCE((SELECT SUM(r.receivable_minor) FROM oxsum.statement_payment_effects r WHERE r.original_effect_id=p.effect_id),0)::bigint AS amount FROM oxsum.statement_payment_effects p WHERE p.journal_id=$1 AND p.kind='payment' AND p.receivable_minor>0 ORDER BY p.effect_id")
        .bind(journal.journal_id).fetch_all(&mut **tx).await?;
    let mut total = 0i64;
    for row in rows {
        let amount: i64 = row.try_get("amount")?;
        if amount < 0 {
            return Err(error::invalid("void repayment source is overdrawn"));
        }
        if amount == 0 {
            continue;
        }
        total = total
            .checked_add(amount)
            .ok_or_else(|| error::invalid("void allocation overflows"))?;
        let entry = entry.ok_or_else(|| error::invalid("void repayment has no payable entry"))?;
        let hash = wallet
            .verified_source_hash_in(tx, entry)
            .await?
            .ok_or_else(|| error::invalid("void entry is unverified"))?;
        sqlx::query("INSERT INTO oxsum.statement_void_allocations(effect_id,journal_id,amount_minor,entry_id,entry_hash) VALUES($1,$2,$3,$4,$5)")
            .bind(row.try_get::<Uuid,_>("effect_id")?).bind(journal.journal_id).bind(amount).bind(entry).bind(hash).execute(&mut **tx).await?;
    }
    if total > journal.settled_minor {
        return Err(error::invalid("void allocations exceed settled source"));
    }
    Ok(())
}

/// Plan-order returns use only the void's unreserved original-payment share.
pub(crate) async fn original_payable_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    journal: Uuid,
) -> Result<i64, WalletError> {
    let key = format!("plan-journal/{journal}/void");
    if wallet
        .verified_source_hash_in(tx, *crate::entry_id_for(&key).as_uuid())
        .await?
        .is_none()
    {
        return Err(error::invalid("void payable has no verified source"));
    }
    let credit = wallet
        .plan_source_credit_in(tx, &key, "Liabilities:RefundPayable")
        .await?;
    let debit = wallet
        .plan_source_debit_in(tx, &key, "Liabilities:RefundPayable")
        .await?;
    let reserved:i64=sqlx::query_scalar("SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.statement_void_allocations WHERE journal_id=$1").bind(journal).fetch_one(&mut **tx).await?;
    let refunds:i64=sqlx::query_scalar("SELECT COALESCE(SUM(payable_minor),0)::bigint FROM oxsum.plan_refund_effects WHERE journal_id=$1").bind(journal).fetch_one(&mut **tx).await?;
    let disputes:Vec<Uuid>=sqlx::query_scalar("SELECT d.dispute_id FROM oxsum.disputes d JOIN oxsum.plan_periods p ON p.authorization_ref=d.order_id AND p.authorization_kind='payment_order' JOIN oxsum.invoice_journals j ON j.plan_period_id=p.plan_period_id WHERE j.journal_id=$1").bind(journal).fetch_all(&mut **tx).await?;
    let mut used = refunds;
    for dispute in disputes {
        used = used
            .checked_add(
                wallet
                    .plan_source_debit_in(
                        tx,
                        &format!("dispute-recharge/{dispute}"),
                        "Liabilities:RefundPayable",
                    )
                    .await?,
            )
            .ok_or_else(|| error::invalid("void returns overflow"))?;
    }
    Ok(credit
        .saturating_sub(debit)
        .saturating_sub(reserved)
        .saturating_sub(used)
        .max(0))
}
