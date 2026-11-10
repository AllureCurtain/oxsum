//! Apply one verified historical correction under live business authority.

use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use super::{PlanUsageCorrection, PlanUsageCorrectionRequest, evidence};
use crate::{BusinessCapability, Db, Wallet, WalletError, business_staff as authority, error};

impl Db {
    /// Preview or apply one historical duplicate-usage correction. Requires a
    /// personal business session with FundsWrite and the matching customer
    /// wallet. Preview rolls back without audit or ledger writes. Apply retains
    /// refused outcomes in finance audit, or commits documents, immutable
    /// allocation and ledger together. This never calls a provider or credits cash.
    pub async fn correct_legacy_plan_usage(
        &self,
        token: &str,
        wallet: &Wallet,
        request: &PlanUsageCorrectionRequest,
        apply: bool,
        on: Date,
    ) -> Result<PlanUsageCorrection, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        let scope = sqlx::query("SELECT j.organization_id,j.customer_plan_id,o.tenant_id FROM oxsum.invoice_journals j JOIN oxsum.organizations o USING(organization_id) WHERE j.journal_id=$1 AND j.business_id=$2")
            .bind(request.journal_id).bind(business).fetch_optional(&mut *tx).await?
            .ok_or_else(|| WalletError::NotFound("unknown plan journal".into()))?;
        let organization: Uuid = scope.try_get("organization_id")?;
        let plan: Uuid = scope.try_get("customer_plan_id")?;
        let tenant: String = scope.try_get("tenant_id")?;
        if tenant != wallet.tenant_id() {
            return Err(WalletError::Conflict(
                "correction wallet does not own the journal".into(),
            ));
        }
        let request_json = serde_json::to_value(request).map_err(error::invalid)?;
        if let Some(row) = sqlx::query("SELECT detail FROM oxsum.finance_audit WHERE business_id=$1 AND action='plan_usage.correct' AND idempotency_key=$2")
            .bind(business).bind(&request.idempotency_key).fetch_optional(&mut *tx).await? {
            let detail: serde_json::Value = row.try_get("detail")?;
            if detail["request"] != request_json {
                return Err(WalletError::Conflict("correction key names different request terms".into()));
            }
            let mut report: PlanUsageCorrection = serde_json::from_value(detail["report"].clone()).map_err(error::invalid)?;
            report.already_applied = report.status == "applied";
            tx.rollback().await?;
            return Ok(report);
        }
        // Same order as activation/refund/close. No statement lock is held
        // while waiting for the plan or the ledger append lock.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text,0))",
        )
        .bind(organization.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query("SELECT subscription_id FROM oxsum.org_subscriptions WHERE organization_id=$1 AND state='active' FOR UPDATE")
            .bind(organization).fetch_optional(&mut *tx).await?;
        sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE")
            .bind(plan).fetch_one(&mut *tx).await?;
        let journal = sqlx::query("SELECT j.*,p.authorization_kind,p.authorization_ref,p.period_start,p.period_end,p.customer_plan_id AS period_plan FROM oxsum.invoice_journals j JOIN oxsum.plan_periods p USING(plan_period_id) WHERE journal_id=$1 FOR UPDATE OF j")
            .bind(request.journal_id).fetch_one(&mut *tx).await?;
        if let Some(row) =
            sqlx::query("SELECT report FROM oxsum.plan_usage_corrections WHERE journal_id=$1")
                .bind(request.journal_id)
                .fetch_optional(&mut *tx)
                .await?
        {
            let mut report: PlanUsageCorrection =
                serde_json::from_value(row.try_get("report")?).map_err(error::invalid)?;
            report.already_applied = true;
            if apply {
                record_audit(&mut tx, business, actor.user_id, request, &report).await?;
                tx.commit().await?;
            } else {
                tx.rollback().await?;
            }
            return Ok(report);
        }
        crate::statement_payments::guard_journal_in(&mut tx, request.journal_id).await?;
        wallet.lock_append_in(&mut tx).await?;
        let mut report = evidence::inspect(&mut tx, wallet, &journal).await?;
        let evidence_bytes = serde_json::to_vec(&report.evidence).map_err(error::invalid)?;
        report.evidence_sha256 = Sha256::digest(evidence_bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if apply {
            if report.status == "proposed" {
                let receipt = wallet
                    .plan_usage_correction_in(&mut tx, &report, on)
                    .await?;
                report.entry_id = Some(*receipt.entry_id.as_uuid());
                report.status = "applied".into();
                apply_documents(&mut tx, &report).await?;
                sqlx::query("INSERT INTO oxsum.plan_usage_corrections(journal_id,correction_id,business_id,organization_id,plan_period_id,statement_id,original_entry_id,entry_id,duplicate_minor,receivable_minor,payable_minor,report,actor_user_id,reason,idempotency_key) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)")
                    .bind(report.journal_id).bind(report.correction_id).bind(business).bind(organization)
                    .bind(journal.try_get::<Uuid,_>("plan_period_id")?).bind(report.statement_id)
                    .bind(report.original_entry_id).bind(report.entry_id).bind(report.duplicate_minor)
                    .bind(report.receivable_minor).bind(report.payable_minor)
                    .bind(serde_json::to_value(&report).map_err(error::invalid)?).bind(actor.user_id)
                    .bind(&request.reason).bind(&request.idempotency_key).execute(&mut *tx).await?;
                let sources = report.evidence["orderCashSources"]
                    .as_array()
                    .ok_or_else(|| error::invalid("validated funding sources missing"))?;
                for source in sources {
                    let order: Uuid = serde_json::from_value(source["orderId"].clone())
                        .map_err(error::invalid)?;
                    sqlx::query("INSERT INTO oxsum.plan_usage_correction_sources(journal_id,order_id) VALUES($1,$2)")
                        .bind(report.journal_id).bind(order).execute(&mut *tx).await?;
                }
            }
            record_audit(&mut tx, business, actor.user_id, request, &report).await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(report)
    }
}

async fn record_audit(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    actor: Uuid,
    request: &PlanUsageCorrectionRequest,
    report: &PlanUsageCorrection,
) -> Result<(), WalletError> {
    sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES($1,$2,$3,'plan_usage.correct',$4,$5,$6)")
        .bind(Uuid::new_v4()).bind(business).bind(actor).bind(request.journal_id.to_string())
        .bind(json!({"request":request,"report":report})).bind(&request.idempotency_key)
        .execute(&mut **tx).await?;
    Ok(())
}

async fn apply_documents(
    tx: &mut Transaction<'_, Postgres>,
    report: &PlanUsageCorrection,
) -> Result<(), WalletError> {
    let statement = report
        .statement_id
        .ok_or_else(|| error::invalid("validated statement missing"))?;
    // The existing kinds and append-only guards express a reversal of the
    // erroneous usage line without relabeling it as a PSP refund or discount.
    sqlx::query("INSERT INTO oxsum.invoice_journal_lines(line_id,journal_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'usage_overage',$3,'Correction of previously settled usage billed twice',$4)")
        .bind(Uuid::new_v4()).bind(report.journal_id).bind(-report.duplicate_minor).bind(report.correction_id)
        .execute(&mut **tx).await?;
    sqlx::query("UPDATE oxsum.invoice_journals SET total_minor=$2,settled_minor=$3,updated_at=now() WHERE journal_id=$1")
        .bind(report.journal_id).bind(report.after_total_minor).bind(report.after_settled_minor).execute(&mut **tx).await?;
    if report.after_total_minor == report.after_settled_minor {
        sqlx::query("UPDATE oxsum.invoice_journals SET state='paid',paid_at=COALESCE(paid_at,now()),updated_at=now() WHERE journal_id=$1 AND state='posted'")
            .bind(report.journal_id).execute(&mut **tx).await?;
    }
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'usage',$3,'Correction of previously settled usage billed twice',$4)")
        .bind(Uuid::new_v4()).bind(statement).bind(-report.duplicate_minor).bind(report.correction_id)
        .execute(&mut **tx).await?;
    if report.payable_minor > 0 {
        sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'payment_applied',$3,'Excess collection moved to refund payable; cash has not been returned',$4)")
            .bind(Uuid::new_v4()).bind(statement).bind(report.payable_minor).bind(report.correction_id)
            .execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE oxsum.plan_statements SET total_minor=$2,paid_minor=$3,updated_at=now() WHERE statement_id=$1")
        .bind(statement).bind(report.after_total_minor).bind(report.after_settled_minor).execute(&mut **tx).await?;
    if report.after_total_minor == report.after_settled_minor {
        sqlx::query("UPDATE oxsum.plan_statements SET state='paid',paid_at=COALESCE(paid_at,now()),updated_at=now() WHERE statement_id=$1 AND state IN ('issued','past_due')")
            .bind(statement).execute(&mut **tx).await?;
        // The caller already locked this plan before journal/ledger/statement.
        // Retained document-only voids remain debt; renewal and manual flags
        // are independent of this correction's debt-restoration decision.
        let plan: Uuid = sqlx::query_scalar(
            "SELECT customer_plan_id FROM oxsum.invoice_journals WHERE journal_id=$1",
        )
        .bind(report.journal_id)
        .fetch_one(&mut **tx)
        .await?;
        crate::plan_debt::refresh_in(tx, plan).await?;
    }
    Ok(())
}
