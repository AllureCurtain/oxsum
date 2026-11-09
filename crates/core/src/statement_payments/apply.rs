//! Atomic receivable settlement, exact customer document and durable fulfillment.

use std::collections::BTreeMap;

use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use super::{evidence, lock_owner_in};
use crate::{Db, PaymentFulfillment, Wallet, WalletError, error};

pub(crate) async fn fulfill(
    db: &Db,
    order_id: Uuid,
    wallet: &Wallet,
    on: Date,
) -> Result<PaymentFulfillment, WalletError> {
    let mut tx = db.pool().begin().await?;
    let scope=sqlx::query("SELECT p.organization_id,o.tenant_id FROM oxsum.payment_orders p JOIN oxsum.organizations o USING(organization_id) WHERE p.order_id=$1")
        .bind(order_id).fetch_optional(&mut *tx).await?.ok_or_else(||WalletError::NotFound("unknown statement payment".into()))?;
    if scope.try_get::<String, _>("tenant_id")? != wallet.tenant_id() {
        return Err(WalletError::Forbidden(
            "statement payment wallet belongs to another customer".into(),
        ));
    }
    let organization = scope.try_get("organization_id")?;
    lock_owner_in(&mut tx, organization, db.pool()).await?;
    let order = crate::payment_orders::order_in(&mut tx, order_id)
        .await?
        .ok_or_else(|| error::invalid("statement order disappeared"))?;
    let terms = evidence::validate(&order)?;
    let previous = evidence::effects_in(&mut tx, wallet, order_id).await?;
    if order.status == "fulfilled" {
        if previous
            .iter()
            .filter(|e| e.kind == "payment")
            .try_fold(0i64, |sum, e| {
                sum.checked_add(e.cash)
                    .ok_or_else(|| error::invalid("statement cash overflows"))
            })?
            != order.amount_minor
        {
            return Err(error::invalid(
                "fulfilled statement payment has incomplete cash effects",
            ));
        }
        let answer = crate::payment_orders::fulfillment_in(&mut tx, order_id)
            .await?
            .ok_or_else(|| error::invalid("statement fulfillment is missing"))?;
        tx.commit().await?;
        return Ok(answer);
    }
    if order.status != "paid" || order.paid_at.is_none() {
        return Err(WalletError::Conflict(
            "statement payment is not confirmed paid".into(),
        ));
    }
    if !previous.is_empty() {
        return Err(WalletError::Conflict(
            "incomplete statement cash history requires review".into(),
        ));
    }
    let receipt = evidence::payment_receipt_in(&mut tx, &order).await?;
    let reservation: String = sqlx::query_scalar(
        "SELECT state FROM oxsum.statement_payment_reservations WHERE order_id=$1 FOR UPDATE",
    )
    .bind(order_id)
    .fetch_one(&mut *tx)
    .await?;
    let source = crate::financial_reconciliation::debt_sources::inspect_in(
        &mut tx,
        organization,
        wallet.tenant_id(),
        terms.statement_id,
    )
    .await?;
    let current: BTreeMap<_, _> = source
        .slices
        .iter()
        .map(|s| (s.journal_id, s.remaining_minor))
        .collect();
    let apply = reservation == "reserved"
        && source.verified
        && terms.allocations.iter().all(|s| {
            current
                .get(&s.journal_id)
                .is_some_and(|n| *n >= s.amount_minor)
        });
    // An authenticated late payment is always retained. A released or changed
    // allocation becomes source-linked customer payable, never wallet funding.
    let mut effects = Vec::new();
    for slice in &terms.allocations {
        let journal = crate::journals::journal_in(&mut tx, slice.journal_id)
            .await?
            .ok_or_else(|| error::invalid("repayment journal is missing"))?;
        if journal.organization_id != organization
            || journal.business_id != terms.business_id
            || journal.customer_plan_id != slice.customer_plan_id
        {
            return Err(error::invalid("repayment journal owner differs"));
        }
        let receivable = if apply { slice.amount_minor } else { 0 };
        let effect = evidence::insert_in(
            &mut tx,
            wallet,
            &order,
            "payment",
            order_id,
            Some(slice.journal_id),
            None,
            Some(receipt),
            slice.amount_minor,
            receivable,
            slice.amount_minor - receivable,
            0,
            on,
        )
        .await?;
        if receivable > 0 {
            sqlx::query("UPDATE oxsum.invoice_journals SET settled_minor=settled_minor+$2,state=CASE WHEN settled_minor+$2=total_minor THEN 'paid' ELSE 'posted' END,paid_at=CASE WHEN settled_minor+$2=total_minor THEN now() ELSE NULL END,updated_at=now() WHERE journal_id=$1")
                .bind(slice.journal_id).bind(receivable).execute(&mut *tx).await?;
            line_in(&mut tx, terms.statement_id, -receivable, effect.id).await?;
        }
        accounting_in(&mut tx, &effect, on).await?;
        effects.push(effect);
    }
    if apply {
        sqlx::query("UPDATE oxsum.plan_statements SET paid_minor=paid_minor+$2,state=CASE WHEN paid_minor+$2=total_minor THEN 'paid' ELSE state END,paid_at=CASE WHEN paid_minor+$2=total_minor THEN now() ELSE NULL END,paid_order_id=$3,updated_at=now() WHERE statement_id=$1")
            .bind(terms.statement_id).bind(order.amount_minor).bind(order_id).execute(&mut *tx).await?;
        for slice in &terms.allocations {
            crate::plan_debt::refresh_in(&mut tx, slice.customer_plan_id).await?;
        }
    }
    sqlx::query("UPDATE oxsum.statement_payment_reservations SET state='applied',release_receipt_id=NULL WHERE order_id=$1").bind(order_id).execute(&mut *tx).await?;
    let payload = serde_json::json!({"arm":"statement_repayment","statementId":terms.statement_id,"paidMinor":order.amount_minor,"repaidMinor":if apply {order.amount_minor}else{0},"payableMinor":if apply {0}else{order.amount_minor},"requiresReview":!apply,"allocations":effects});
    let outbox = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.payment_outbox(outbox_id,order_id,event_type,payload) VALUES($1,$2,'payment_order.statement_repayment_fulfilled',$3)")
        .bind(outbox).bind(order_id).bind(&payload).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO oxsum.payment_fulfillments(fulfillment_id,order_id,purpose,purpose_payload,outbox_id) VALUES($1,$2,'statement_repayment',$3,$4)")
        .bind(Uuid::new_v4()).bind(order_id).bind(&payload).bind(outbox).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE oxsum.payment_orders SET status='fulfilled',updated_at=now() WHERE order_id=$1",
    )
    .bind(order_id)
    .execute(&mut *tx)
    .await?;
    // Automatic fulfillment has no human finance decision. Its immutable
    // source effects, fulfillment and outbox are the automatic audit trail.

    let answer = crate::payment_orders::fulfillment_in(&mut tx, order_id)
        .await?
        .ok_or_else(|| error::invalid("statement fulfillment missing"))?;
    tx.commit().await?;
    Ok(answer)
}

pub(crate) async fn line_in(
    tx: &mut Transaction<'_, Postgres>,
    statement: Uuid,
    amount: i64,
    source: Uuid,
) -> Result<(), WalletError> {
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'payment_applied',$3,'Source-linked statement payment allocation',$4)")
        .bind(Uuid::new_v4()).bind(statement).bind(amount).bind(source).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn accounting_in(
    tx: &mut Transaction<'_, Postgres>,
    effect: &evidence::Effect,
    on: Date,
) -> Result<(), WalletError> {
    let Some(journal) = effect.journal else {
        return Ok(());
    };
    let Some(period) = crate::plan_recognition::period_in(tx, journal).await? else {
        return Ok(());
    };
    let sign = if effect.kind == "payment" { 1 } else { -1 };
    // Only AR collection changes. Earned/deferred service revenue is untouched.
    crate::plan_recognition::effect_in(
        tx,
        &period,
        if sign == 1 {
            "statement_payment"
        } else {
            "statement_return"
        },
        effect.id,
        on,
        on,
        Some(effect.entry),
        &crate::plan_recognition::Effect {
            collected: sign * effect.receivable,
            receivable: -sign * effect.receivable,
            payable: sign * effect.payable,
            cash: sign * effect.cash,
            ..Default::default()
        },
    )
    .await
}
