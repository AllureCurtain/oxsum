//! Explicit owner/finance preview, revision check, apply and immutable outcomes.

use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use super::{NonplanRefundRecovery, NonplanRefundRecoveryRequest, apply, evidence};
use crate::{
    BusinessCapability, Db, Wallet, WalletError, business_staff as authority, error,
    nonplan_returns,
};

impl Db {
    /// Inspect or explicitly recover one exact historical non-plan refund.
    /// Preview writes no ledger/document/audit. Apply requires its unchanged
    /// source revision and live owner/finance authority; actual Cash is never
    /// repeated. Missing authenticated receipt evidence is an explicit refusal.
    pub async fn recover_nonplan_refund(
        &self,
        token: &str,
        wallet: &Wallet,
        request: &NonplanRefundRecoveryRequest,
        apply: bool,
        on: Date,
    ) -> Result<NonplanRefundRecovery, WalletError> {
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
        let scope=sqlx::query("SELECT r.order_id,r.organization_id,o.tenant_id FROM oxsum.refunds r JOIN oxsum.organizations o USING(organization_id) WHERE r.refund_id=$1 AND r.business_id=$2")
            .bind(request.refund_id).bind(business).fetch_optional(&mut *tx).await?
            .ok_or_else(||WalletError::NotFound("unknown refund".into()))?;
        if scope.try_get::<String, _>("tenant_id")? != wallet.tenant_id() {
            return Err(evidence::review("wallet does not own the selected refund"));
        }
        let terms = json!({"refundId":request.refund_id,"eventReceiptId":request.event_receipt_id,"idempotencyKey":request.idempotency_key,"reason":request.reason.trim(),"expectedRevision":request.expected_revision});
        if let Some(row)=sqlx::query("SELECT actor_user_id,detail FROM oxsum.finance_audit WHERE business_id=$1 AND action='nonplan_refund.recover' AND idempotency_key=$2")
            .bind(business).bind(&request.idempotency_key).fetch_optional(&mut *tx).await? {
            let detail:serde_json::Value=row.try_get("detail")?;
            if row.try_get::<Uuid,_>("actor_user_id")?!=actor.user_id || detail["request"]!=terms {
                return Err(evidence::review("recovery key names another actor or request"));
            }
            let mut report:NonplanRefundRecovery=serde_json::from_value(detail["report"].clone()).map_err(error::invalid)?;
            if report.status=="applied" {
                nonplan_returns::lock_source_in(&mut tx,scope.try_get("order_id")?).await?;
                sqlx::query("SELECT refund_id FROM oxsum.refunds WHERE refund_id=$1 FOR UPDATE").bind(request.refund_id).fetch_one(&mut *tx).await?;
                wallet.lock_append_in(&mut tx).await?;
                let effect=apply::effect_in(&mut tx,wallet,request.refund_id).await?
                    .ok_or_else(||evidence::review("committed recovery effect is missing"))?;
                report.effect=Some(effect);
                report.already_applied=true;
            }
            tx.rollback().await?;
            return Ok(report);
        }
        nonplan_returns::lock_source_in(&mut tx, scope.try_get("order_id")?).await?;
        sqlx::query("SELECT refund_id FROM oxsum.refunds WHERE refund_id=$1 FOR UPDATE")
            .bind(request.refund_id)
            .fetch_one(&mut *tx)
            .await?;
        wallet.lock_append_in(&mut tx).await?;
        if let Some(effect) = apply::effect_in(&mut tx, wallet, request.refund_id).await? {
            let source = evidence::source_in(&mut tx, wallet, request.refund_id).await?;
            evidence::event_in(
                &mut tx,
                &source,
                request.event_receipt_id,
                request.event_receipt_id == effect.event_receipt_id,
            )
            .await?;
            let row=sqlx::query("SELECT source_revision,source FROM oxsum.nonplan_refund_effects WHERE refund_id=$1")
                .bind(request.refund_id).fetch_one(&mut *tx).await?;
            let report = NonplanRefundRecovery {
                refund_id: request.refund_id,
                status: "applied".into(),
                refusal: None,
                source_revision: row.try_get("source_revision")?,
                source: row.try_get("source")?,
                existing_cash_minor: effect.cash_minor,
                cash_to_book_minor: 0,
                wallet_minor: effect.wallet_minor,
                suspense_minor: effect.suspense_minor,
                wallet_to_restore_minor: 0,
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
            request.refund_id,
            request.event_receipt_id,
            true,
        )
        .await;
        let mut inspection = match inspected {
            Ok(value) => value,
            Err(WalletError::Conflict(reason)) => {
                let source = json!({"refundId":request.refund_id,"eventReceiptId":request.event_receipt_id,"refusal":reason});
                let report = NonplanRefundRecovery {
                    refund_id: request.refund_id,
                    status: "refused".into(),
                    refusal: Some(reason),
                    source_revision: evidence::revision(&source)?,
                    source,
                    effect: None,
                    existing_cash_minor: 0,
                    cash_to_book_minor: 0,
                    wallet_minor: 0,
                    suspense_minor: 0,
                    wallet_to_restore_minor: 0,
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
            require_revision(request, &inspection.report)?;
            apply::commit_in(
                self,
                &mut tx,
                wallet,
                &mut inspection,
                request.event_receipt_id,
                true,
                on,
            )
            .await?;
            audit_in(
                &mut tx,
                business,
                actor.user_id,
                &terms,
                &request.idempotency_key,
                &inspection.report,
            )
            .await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(inspection.report)
    }
}

fn require_revision(
    request: &NonplanRefundRecoveryRequest,
    report: &NonplanRefundRecovery,
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
    report: &NonplanRefundRecovery,
) -> Result<(), WalletError> {
    sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES($1,$2,$3,'nonplan_refund.recover',$4,$5,$6)")
        .bind(Uuid::new_v4()).bind(business).bind(actor).bind(report.refund_id.to_string())
        .bind(json!({"request":request,"report":report})).bind(key).execute(&mut **tx).await?;
    Ok(())
}
