//! One transaction owns cancellation, successor issuance and replayable evidence.

use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    InvoiceCorrectionPreview, InvoiceCorrectionPreviewRequest, InvoiceCorrectionRequest,
    InvoiceCorrectionResult, preview,
};
use crate::{
    BusinessCapability, Db, JournalState, Wallet, WalletError, business_staff as authority, error,
    journals, plan_recognition, plan_statements, statement_payments,
};

impl Db {
    /// Cancel and reissue one proven invoice, preserving its service and original
    /// payment ownership. Exact retries return the immutable operation result.
    ///
    /// # Errors
    /// Current authority, source revision and line bounds are mandatory. Pending
    /// payments, inconsistent sources, stale previews and altered replay refuse
    /// without document, financial or audit effects.
    pub async fn correct_invoice(
        &self,
        token: &str,
        journal: Uuid,
        request: &InvoiceCorrectionRequest,
    ) -> Result<InvoiceCorrectionResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        if request.source_revision.len() != 64
            || !request
                .source_revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(error::invalid(
                "sourceRevision must be a lowercase SHA-256 digest",
            ));
        }
        let mut canonical = request.clone();
        canonical.lines.sort_by_key(|line| line.source_line_id);
        let retained = serde_json::json!({"journalId": journal, "request": canonical});
        let authorized = self
            .business_capability(token, BusinessCapability::FundsWrite)
            .await?;
        let tenant: String = sqlx::query_scalar("SELECT o.tenant_id FROM oxsum.invoice_journals j JOIN oxsum.organizations o USING(organization_id) WHERE j.journal_id=$1 AND j.business_id=$2 AND j.state<>'draft'")
            .bind(journal).bind(authorized.business_id).fetch_optional(self.pool()).await?
            .ok_or_else(|| WalletError::NotFound("unknown invoice correction source".into()))?;
        let wallet = Wallet::open_existing(self.pool().clone(), &tenant).await?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        // Recovery precedes source inspection: later payments or another
        // correction cannot invalidate a successfully committed request.
        if let Some(row) = sqlx::query("SELECT request,result FROM oxsum.invoice_corrections WHERE business_id=$1 AND actor_user_id=$2 AND idempotency_key=$3")
            .bind(business).bind(actor.user_id).bind(&request.idempotency_key)
            .fetch_optional(&mut *tx).await?
        {
            if row.try_get::<serde_json::Value, _>("request")? != retained {
                return Err(WalletError::Conflict("invoice correction key names different request terms".into()));
            }
            let answer = serde_json::from_value(row.try_get("result")?).map_err(error::invalid)?;
            tx.rollback().await?;
            return Ok(answer);
        }
        let row = sqlx::query("SELECT j.organization_id,o.tenant_id FROM oxsum.invoice_journals j JOIN oxsum.organizations o USING(organization_id) WHERE j.journal_id=$1 AND j.business_id=$2 AND j.state<>'draft'")
            .bind(journal).bind(business).fetch_optional(&mut *tx).await?
            .ok_or_else(|| WalletError::NotFound("unknown invoice correction source".into()))?;
        let organization = row.try_get("organization_id")?;
        if row.try_get::<String, _>("tenant_id")? != tenant {
            return Err(WalletError::Conflict("invoice wallet owner changed".into()));
        }
        statement_payments::lock_owner_in(&mut tx, organization, self.pool()).await?;
        let original = journals::journal_in(&mut tx, journal)
            .await?
            .ok_or_else(|| error::invalid("correction source disappeared"))?;
        let preview = preview::inspect_in(
            &mut tx,
            organization,
            &tenant,
            journal,
            &InvoiceCorrectionPreviewRequest {
                lines: canonical.lines.clone(),
            },
        )
        .await?;
        if preview.source_revision != request.source_revision {
            return Err(WalletError::Conflict(
                "invoice correction preview is stale; review a new preview".into(),
            ));
        }
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        if original.state != JournalState::Void {
            self.void_journal_financial_in(
                &mut tx,
                &wallet,
                &original,
                &request.reason,
                now.date(),
            )
            .await?;
        }
        let after: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        if plan_recognition::schedule::completed_boundary(
            preview.service_start,
            preview.service_end,
            after,
        ) != preview.recognition_boundary
            || after.date() != now.date()
        {
            return Err(WalletError::Conflict(
                "service boundary changed during correction; review a new preview".into(),
            ));
        }
        let answer = issue_in(
            &mut tx,
            &wallet,
            business,
            actor.user_id,
            &original,
            &canonical,
            &retained,
            preview,
            now,
        )
        .await?;
        authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.invoice.correct",
            &retained,
        )
        .await?;
        crate::plan_debt::refresh_in(&mut tx, original.customer_plan_id).await?;
        tx.commit().await?;
        Ok(answer)
    }
}

#[allow(clippy::too_many_arguments)]
async fn issue_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    business: Uuid,
    actor: Uuid,
    original: &crate::InvoiceJournal,
    request: &InvoiceCorrectionRequest,
    retained: &serde_json::Value,
    preview: InvoiceCorrectionPreview,
    now: OffsetDateTime,
) -> Result<InvoiceCorrectionResult, WalletError> {
    let journal = Uuid::new_v4();
    let statement = Uuid::new_v4();
    let correction = Uuid::new_v4();
    let total = preview.amounts.corrected_total_minor;
    sqlx::query("INSERT INTO oxsum.invoice_journals(journal_id,customer_plan_id,organization_id,business_id,period_index,state,total_minor,settled_minor,currency,accounting_policy_version,replaces_journal_id) VALUES($1,$2,$3,$4,$5,'draft',$6,0,$7,$8,$9)")
        .bind(journal).bind(original.customer_plan_id).bind(original.organization_id).bind(business)
        .bind(original.period_index).bind(total).bind(&original.currency).bind(original.accounting_policy_version)
        .bind(original.journal_id).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO oxsum.plan_statements(statement_id,customer_plan_id,organization_id,business_id,period_index,period_start,period_end,journal_id,state,total_minor,currency,replaces_statement_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'open',$9,$10,$11)")
        .bind(statement).bind(original.customer_plan_id).bind(original.organization_id).bind(business)
        .bind(original.period_index).bind(preview.service_start).bind(preview.service_end).bind(journal)
        .bind(total).bind(&original.currency).bind(preview.statement_id).execute(&mut **tx).await?;
    let mut inserted = Vec::new();
    for line in &preview.lines {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.invoice_journal_lines(line_id,journal_id,kind,amount_minor,description,source_ref) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(id).bind(journal).bind(line.kind.as_str()).bind(line.corrected_minor)
            .bind(&line.description).bind(line.source_line_id).execute(&mut **tx).await?;
        inserted.push((id, line));
    }
    for line in journals::lines_for_statement_in(tx, journal).await? {
        sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(Uuid::new_v4()).bind(statement).bind(plan_statements::statement_kind(&line).as_str())
            .bind(line.amount_minor).bind(&line.description).bind(line.source_ref).execute(&mut **tx).await?;
    }
    sqlx::query(
        "UPDATE oxsum.invoice_journals SET state='posted',posted_at=$2 WHERE journal_id=$1",
    )
    .bind(journal)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // Reissue preserves the original due date, so correction cannot extend debt
    // terms or reset a prior overdue state without a separate approved action.
    sqlx::query("UPDATE oxsum.plan_statements SET state='issued',issued_at=$2,due_at=(SELECT due_at FROM oxsum.plan_statements WHERE statement_id=$3) WHERE statement_id=$1")
        .bind(statement).bind(now).bind(preview.statement_id).execute(&mut **tx).await?;
    if total == 0 {
        sqlx::query(
            "UPDATE oxsum.invoice_journals SET state='paid',paid_at=$2 WHERE journal_id=$1",
        )
        .bind(journal)
        .bind(now)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.plan_statements SET state='paid',paid_at=$2 WHERE statement_id=$1",
        )
        .bind(statement)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    let entry = wallet
        .invoice_correction_accrual_in(
            tx,
            journal,
            preview.amounts.successor_earned_minor,
            preview.amounts.successor_deferred_minor,
            now.date(),
        )
        .await?;
    let hash = match entry {
        Some(entry) => Some(
            wallet
                .verified_source_hash_in(tx, entry)
                .await?
                .ok_or_else(|| error::invalid("corrected accrual is unverified"))?,
        ),
        None => None,
    };
    let answer = InvoiceCorrectionResult {
        correction_id: correction,
        original_journal_id: original.journal_id,
        original_statement_id: preview.statement_id,
        journal_id: journal,
        statement_id: statement,
        preview: preview.clone(),
    };
    let boundary: i64 = sqlx::query_scalar(&format!(
        "SELECT COALESCE(MAX(log_index),0)::bigint FROM ledger_{}.entries",
        wallet.tenant_id()
    ))
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO oxsum.invoice_corrections(correction_id,business_id,organization_id,actor_user_id,predecessor_journal_id,predecessor_statement_id,journal_id,statement_id,source_revision,idempotency_key,reason,request,result,entry_id,entry_hash,ledger_boundary) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)")
        .bind(correction).bind(business).bind(original.organization_id).bind(actor)
        .bind(original.journal_id).bind(preview.statement_id).bind(journal).bind(statement)
        .bind(&request.source_revision).bind(&request.idempotency_key).bind(&request.reason)
        .bind(retained).bind(serde_json::to_value(&answer).map_err(error::invalid)?).bind(entry).bind(hash).bind(boundary)
        .execute(&mut **tx).await?;
    for (id, line) in inserted {
        sqlx::query("INSERT INTO oxsum.invoice_correction_lines(line_id,correction_id,source_line_id,original_minor,corrected_minor) VALUES($1,$2,$3,$4,$5)")
            .bind(id).bind(correction).bind(line.source_line_id).bind(line.original_minor).bind(line.corrected_minor)
            .execute(&mut **tx).await?;
    }
    sqlx::query("INSERT INTO oxsum.invoice_accounting_generations(journal_id,correction_id,predecessor_journal_id,original_journal_id,plan_period_id,customer_plan_id,organization_id,business_id,plan_version_id,accounting_policy_version,fee_minor,service_start,service_end,recognition_from,payment_on) VALUES($1,$2,$3,COALESCE((SELECT original_journal_id FROM oxsum.invoice_accounting_generations WHERE journal_id=$3),$3),$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)")
        .bind(journal).bind(correction).bind(original.journal_id).bind(preview.plan_period_id)
        .bind(original.customer_plan_id).bind(original.organization_id).bind(business).bind(preview.plan_version_id)
        .bind(original.accounting_policy_version).bind(total).bind(preview.service_start).bind(preview.service_end)
        .bind(preview.recognition_boundary).bind(now.date()).execute(&mut **tx).await?;
    if original.accounting_policy_version == 2 {
        let period = plan_recognition::period_in(tx, journal)
            .await?
            .ok_or_else(|| error::invalid("correction accounting generation did not land"))?;
        plan_recognition::effect_in(
            tx,
            &period,
            "correction",
            correction,
            preview.recognition_boundary.date(),
            now.date(),
            entry,
            &plan_recognition::Effect {
                billed: total,
                earned: preview.amounts.successor_earned_minor,
                deferred: preview.amounts.successor_deferred_minor,
                receivable: total,
                ..Default::default()
            },
        )
        .await?;
    }
    Ok(answer)
}
