//! Exact repayment ownership of a financially voided journal's refund payable.

use std::collections::BTreeMap;

use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{InvoiceJournal, Wallet, WalletError, error};

/// Select only this journal's proven lines, including foreign statement wrappers.
pub(crate) async fn documents_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    journal: &InvoiceJournal,
) -> Result<Option<BTreeMap<Uuid, Vec<Uuid>>>, WalletError> {
    let complex: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.statement_payment_effects WHERE organization_id=$1 AND receivable_minor>0) OR EXISTS(SELECT 1 FROM oxsum.plan_statement_lines l JOIN oxsum.plan_statements s USING(statement_id) WHERE s.organization_id=$1 AND l.kind='dispute_chargeback') OR EXISTS(SELECT 1 FROM oxsum.statement_source_voids v JOIN oxsum.plan_statements s USING(statement_id) WHERE s.organization_id=$1)")
        .bind(journal.organization_id).fetch_one(&mut **tx).await?;
    if !complex || (journal.total_minor == 0 && journal.settled_minor == 0) {
        return Ok(None);
    }
    let selected = crate::financial_reconciliation::debt_sources::void_lines_in(
        tx,
        journal.organization_id,
        wallet.tenant_id(),
        journal.journal_id,
    )
    .await?;
    for statement in selected.keys() {
        let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.statement_payment_reservations WHERE statement_id=$1 AND state='reserved')").bind(statement).fetch_one(&mut **tx).await?;
        if live {
            return Err(WalletError::Conflict("affected statement has a live payment intent; resolve its provider outcome before source correction".into()));
        }
        sqlx::query(
            "SELECT statement_id FROM oxsum.plan_statements WHERE statement_id=$1 FOR UPDATE",
        )
        .bind(statement)
        .fetch_one(&mut **tx)
        .await?;
    }
    Ok(Some(selected))
}

/// Append exact opposite document lines beside the already-posted financial void.
pub(crate) async fn correct_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    journal: &InvoiceJournal,
    documents: &BTreeMap<Uuid, Vec<Uuid>>,
    entry: Option<Uuid>,
    reason: &str,
) -> Result<(), WalletError> {
    for (statement, lines) in documents {
        let entry =
            entry.ok_or_else(|| error::invalid("source correction has no financial void"))?;
        let hash = wallet
            .verified_source_hash_in(tx, entry)
            .await?
            .ok_or_else(|| error::invalid("source correction void is unverified"))?;
        for original in lines {
            let row=sqlx::query("SELECT kind,amount_minor FROM oxsum.plan_statement_lines WHERE statement_id=$1 AND line_id=$2").bind(statement).bind(original).fetch_one(&mut **tx).await?;
            let amount: i64 = row.try_get("amount_minor")?;
            let opposite = amount
                .checked_neg()
                .ok_or_else(|| error::invalid("source correction overflows"))?;
            let reversal = Uuid::new_v4();
            sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(reversal).bind(statement).bind(row.try_get::<String,_>("kind")?).bind(opposite).bind(format!("source void {}: {reason}",journal.journal_id)).bind(original).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO oxsum.statement_source_voids(original_line_id,reversal_line_id,statement_id,journal_id,entry_id,entry_hash) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(original).bind(reversal).bind(statement).bind(journal.journal_id).bind(entry).bind(&hash).execute(&mut **tx).await?;
        }
        let (total,paid):(i64,i64)=sqlx::query_as("SELECT COALESCE(SUM(amount_minor) FILTER(WHERE kind<>'payment_applied'),0)::bigint,-COALESCE(SUM(amount_minor) FILTER(WHERE kind='payment_applied'),0)::bigint FROM oxsum.plan_statement_lines WHERE statement_id=$1").bind(statement).fetch_one(&mut **tx).await?;
        if total < 0 || paid < 0 || paid > total {
            return Err(error::invalid(
                "source correction leaves invalid document balances",
            ));
        }
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.plan_statement_lines l WHERE l.statement_id=$1 AND NOT EXISTS(SELECT 1 FROM oxsum.statement_source_voids v WHERE v.original_line_id=l.line_id OR v.reversal_line_id=l.line_id))").bind(statement).fetch_one(&mut **tx).await?;
        sqlx::query("UPDATE oxsum.plan_statements SET total_minor=$2,paid_minor=$3,state=CASE WHEN NOT $4 THEN 'void' WHEN $2=$3 THEN 'paid' ELSE state END,voided_at=CASE WHEN NOT $4 THEN now() ELSE voided_at END,void_reason=CASE WHEN NOT $4 THEN $5 ELSE void_reason END,paid_at=CASE WHEN $4 AND $2=$3 THEN COALESCE(paid_at,now()) ELSE NULL END,updated_at=now() WHERE statement_id=$1")
            .bind(statement).bind(total).bind(paid).bind(active).bind(reason).execute(&mut **tx).await?;
        let plan: Option<Uuid> = sqlx::query_scalar(
            "SELECT customer_plan_id FROM oxsum.plan_statements WHERE statement_id=$1",
        )
        .bind(statement)
        .fetch_one(&mut **tx)
        .await?;
        if let Some(plan) = plan {
            crate::plan_debt::refresh_in(tx, plan).await?;
        }
    }
    Ok(())
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
