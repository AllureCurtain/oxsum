//! Live Finance/Owner preview, source revision pin and atomic audited application.

use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use super::{NonplanChargebackRecovery, NonplanChargebackRecoveryRequest, apply, evidence};
use crate::{
    BusinessCapability, Db, Wallet, WalletError, business_staff as authority, error,
    nonplan_returns,
};

impl Db {
    /// Preview or explicitly compensate one exact historical non-plan chargeback.
    /// A pure preview writes nothing. Apply requires the inspected source revision;
    /// the old entry, original terminal/audit and CreditLine history never change.
    pub async fn recover_nonplan_chargeback(
        &self,
        token: &str,
        wallet: &Wallet,
        request: &NonplanChargebackRecoveryRequest,
        apply: bool,
        on: Date,
    ) -> Result<NonplanChargebackRecovery, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        if request.expected_revision.as_deref().is_some_and(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) || (apply && request.expected_revision.is_none())
        {
            return Err(error::invalid(
                "apply requires a 64-character lowercase source revision",
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        let scope=sqlx::query("SELECT d.order_id,d.organization_id,o.tenant_id FROM oxsum.disputes d JOIN oxsum.payment_orders p USING(order_id) JOIN oxsum.organizations o ON o.organization_id=d.organization_id WHERE d.dispute_id=$1 AND p.business_id=$2")
            .bind(request.dispute_id).bind(business).fetch_optional(&mut *tx).await?.ok_or_else(||WalletError::NotFound("unknown dispute".into()))?;
        if scope.try_get::<String, _>("tenant_id")? != wallet.tenant_id() {
            return Err(evidence::review("wallet does not own selected dispute"));
        }
        sqlx::query("SELECT organization_id FROM oxsum.organizations WHERE organization_id=$1 FOR KEY SHARE")
            .bind(scope.try_get::<Uuid,_>("organization_id")?).fetch_one(&mut *tx).await?;
        let terms = json!({"disputeId":request.dispute_id,"eventReceiptId":request.event_receipt_id,"idempotencyKey":request.idempotency_key,"reason":request.reason.trim(),"expectedRevision":request.expected_revision});
        let previous=sqlx::query("SELECT actor_user_id,detail FROM oxsum.finance_audit WHERE business_id=$1 AND action='nonplan_chargeback.recover' AND idempotency_key=$2")
            .bind(business).bind(&request.idempotency_key).fetch_optional(&mut *tx).await?;
        nonplan_returns::lock_source_in(&mut tx, scope.try_get("order_id")?).await?;
        sqlx::query("SELECT dispute_id FROM oxsum.disputes WHERE dispute_id=$1 FOR UPDATE")
            .bind(request.dispute_id)
            .fetch_one(&mut *tx)
            .await?;
        wallet.lock_append_in(&mut tx).await?;
        if let Some(row) = previous {
            let detail: serde_json::Value = row.try_get("detail")?;
            if row.try_get::<Uuid, _>("actor_user_id")? != actor.user_id
                || detail["request"] != terms
            {
                return Err(evidence::review(
                    "recovery key names another actor or request",
                ));
            }
            let mut report: NonplanChargebackRecovery =
                serde_json::from_value(detail["report"].clone()).map_err(error::invalid)?;
            if report.status == "applied" {
                report.effect = Some(
                    apply::effect_in(&mut tx, wallet, request.dispute_id)
                        .await?
                        .ok_or_else(|| evidence::review("committed correction is missing"))?,
                );
                report.already_applied = true;
                report.cash_to_book_minor = 0;
            }
            tx.rollback().await?;
            return Ok(report);
        }
        if let Some(effect) = apply::effect_in(&mut tx, wallet, request.dispute_id).await? {
            if effect.event_receipt_id != request.event_receipt_id {
                return Err(evidence::review(
                    "retained effect belongs to another receipt",
                ));
            }
            let row = sqlx::query(
                "SELECT source FROM oxsum.nonplan_chargeback_corrections WHERE dispute_id=$1",
            )
            .bind(request.dispute_id)
            .fetch_one(&mut *tx)
            .await?;
            let report = NonplanChargebackRecovery {
                dispute_id: request.dispute_id,
                status: "applied".into(),
                refusal: None,
                source_revision: effect.source_revision.clone(),
                source: row.try_get("source")?,
                false_receipt_minor: effect.false_receipt_minor,
                actual_loss_minor: effect.actual_loss_minor,
                cash_to_book_minor: 0,
                wallet_minor: effect.wallet_minor,
                suspense_minor: effect.suspense_minor,
                effect: Some(effect),
                already_applied: true,
            };
            if apply {
                require_revision(request, &report)?;
                audit_in(
                    &mut tx,
                    business,
                    actor.user_id,
                    &terms,
                    &request.idempotency_key,
                    &report,
                )
                .await?;
                tx.commit().await?;
            } else {
                tx.rollback().await?;
            }
            return Ok(report);
        }
        let inspected = apply::inspect_in(
            &mut tx,
            wallet,
            request.dispute_id,
            request.event_receipt_id,
        )
        .await;
        let mut inspected = match inspected {
            Ok(value) => value,
            Err(WalletError::Conflict(reason)) => {
                let source = json!({"disputeId":request.dispute_id,"eventReceiptId":request.event_receipt_id,"refusal":reason});
                let report = NonplanChargebackRecovery {
                    dispute_id: request.dispute_id,
                    status: "refused".into(),
                    refusal: Some(reason),
                    source_revision: evidence::revision(&source)?,
                    source,
                    false_receipt_minor: 0,
                    actual_loss_minor: 0,
                    cash_to_book_minor: 0,
                    wallet_minor: 0,
                    suspense_minor: 0,
                    effect: None,
                    already_applied: false,
                };
                if apply {
                    require_revision(request, &report)?;
                    audit_in(
                        &mut tx,
                        business,
                        actor.user_id,
                        &terms,
                        &request.idempotency_key,
                        &report,
                    )
                    .await?;
                    tx.commit().await?;
                } else {
                    tx.rollback().await?;
                }
                return Ok(report);
            }
            Err(error) => return Err(error),
        };
        if apply {
            require_revision(request, &inspected.report)?;
            apply::commit_in(
                &mut tx,
                wallet,
                &mut inspected,
                request.event_receipt_id,
                on,
            )
            .await?;
            audit_in(
                &mut tx,
                business,
                actor.user_id,
                &terms,
                &request.idempotency_key,
                &inspected.report,
            )
            .await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(inspected.report)
    }
}

fn require_revision(
    request: &NonplanChargebackRecoveryRequest,
    report: &NonplanChargebackRecovery,
) -> Result<(), WalletError> {
    if request.expected_revision.as_deref() != Some(report.source_revision.as_str()) {
        return Err(evidence::review(
            "source revision changed; preview and review again",
        ));
    }
    Ok(())
}

async fn audit_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    actor: Uuid,
    request: &serde_json::Value,
    key: &str,
    report: &NonplanChargebackRecovery,
) -> Result<(), WalletError> {
    sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES($1,$2,$3,'nonplan_chargeback.recover',$4,$5,$6)")
        .bind(Uuid::new_v4()).bind(business).bind(actor).bind(report.dispute_id.to_string()).bind(json!({"request":request,"report":report})).bind(key).execute(&mut **tx).await?;
    Ok(())
}
