//! Source-verified preview with no document, ledger or provider effects.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{InvoiceCorrectionPreview, InvoiceCorrectionPreviewRequest, amounts};
use crate::financial_reconciliation::correction_sources;
use crate::{
    BusinessCapability, Db, InvoiceJournalDetail, InvoiceJournalLine, JournalLineKind,
    JournalState, PlanPaymentTerms, WalletError, business_staff as authority, error, journals,
};

impl Db {
    /// Preview a source-bound correction under current personal FundsWrite authority.
    /// The transaction captures one repeatable snapshot and performs no financial,
    /// document, provider or audit mutation. A preview does not reserve its source.
    ///
    /// # Errors
    /// Missing authority or foreign sources refuse before source capture. Unproven
    /// sources, pending returns/payment intents, invalid reductions and inconsistent
    /// history return an error instead of guessed successor or payable amounts.
    pub async fn preview_invoice_correction(
        &self,
        token: &str,
        journal: Uuid,
        request: &InvoiceCorrectionPreviewRequest,
    ) -> Result<InvoiceCorrectionPreview, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
            .await?;
        let scope = sqlx::query("SELECT j.organization_id,o.tenant_id FROM oxsum.invoice_journals j JOIN oxsum.organizations o USING(organization_id) WHERE j.journal_id=$1 AND j.business_id=$2 AND j.state<>'draft'")
            .bind(journal).bind(business).fetch_optional(&mut *tx).await?
            .ok_or_else(|| WalletError::NotFound("unknown invoice correction source".into()))?;
        let organization: Uuid = scope.try_get("organization_id")?;
        let tenant: String = scope.try_get("tenant_id")?;
        let answer = inspect_in(&mut tx, organization, &tenant, journal, request).await?;
        tx.rollback().await?;
        Ok(answer)
    }
}

pub(super) async fn inspect_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    tenant: &str,
    journal: Uuid,
    request: &InvoiceCorrectionPreviewRequest,
) -> Result<InvoiceCorrectionPreview, WalletError> {
    let (history_revision, retained_payable) =
        correction_sources::revision_in(tx, organization, tenant, journal).await?;
    let detail = journals::journal_snapshot_in(tx, organization, journal)
        .await?
        .ok_or_else(|| WalletError::NotFound("unknown invoice correction source".into()))?;
    let j = &detail.journal;
    let generation =
        sqlx::query("SELECT * FROM oxsum.invoice_accounting_generations WHERE journal_id=$1")
            .bind(journal)
            .fetch_optional(&mut **tx)
            .await?;
    if j.replaces_journal_id.is_some() != generation.is_some() {
        return Err(WalletError::Conflict(
            "successor accounting context is missing or inconsistent".into(),
        ));
    }
    let has_successor: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM oxsum.invoice_journals WHERE replaces_journal_id=$1)",
    )
    .bind(journal)
    .fetch_one(&mut **tx)
    .await?;
    if has_successor {
        return Err(WalletError::Conflict(
            "invoice already has a successor".into(),
        ));
    }
    let period = generation
        .as_ref()
        .map(|g| g.try_get::<Uuid, _>("plan_period_id"))
        .transpose()?
        .or(j.plan_period_id)
        .ok_or_else(|| WalletError::Conflict("original service period is missing".into()))?;
    let p = sqlx::query("SELECT * FROM oxsum.plan_periods WHERE plan_period_id=$1 AND customer_plan_id=$2 AND period_index=$3")
        .bind(period).bind(j.customer_plan_id).bind(j.period_index).fetch_optional(&mut **tx).await?
        .ok_or_else(|| WalletError::Conflict("invoice service period differs from its original source".into()))?;
    let statements = sqlx::query("SELECT * FROM oxsum.plan_statements WHERE journal_id=$1 AND period_index=$2 ORDER BY statement_id LIMIT 2")
        .bind(journal).bind(j.period_index).fetch_all(&mut **tx).await?;
    if statements.len() != 1 {
        return Err(WalletError::Conflict(
            "invoice original statement is not unambiguous".into(),
        ));
    }
    let s = &statements[0];
    let statement: Uuid = s.try_get("statement_id")?;
    if s.try_get::<Uuid, _>("organization_id")? != organization
        || s.try_get::<Option<Uuid>, _>("customer_plan_id")? != Some(j.customer_plan_id)
        || s.try_get::<String, _>("currency")? != j.currency
    {
        return Err(WalletError::Conflict(
            "invoice and statement source terms differ".into(),
        ));
    }
    crate::statement_payments::guard_journal_in(tx, journal).await?;
    let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.refunds r WHERE r.organization_id=$1 AND r.state IN ('requested','provider_pending')) OR EXISTS(SELECT 1 FROM oxsum.disputes d WHERE d.organization_id=$1 AND d.state IN ('opened','evidence_due'))")
        .bind(organization).fetch_one(&mut **tx).await?;
    if pending {
        return Err(WalletError::Conflict(
            "resolve pending returns or disputes before invoice correction".into(),
        ));
    }
    let originals = original_lines(&detail)?;
    let mut base = 0i128;
    let mut historical_usage = 0i128;
    let mut has_historical_usage = false;
    for line in &originals {
        match line.kind {
            JournalLineKind::BaseFee
                if line.amount_minor >= 0
                    && (generation.is_some() || line.source_ref == Some(period)) =>
            {
                base += i128::from(line.amount_minor);
            }
            JournalLineKind::Discount | JournalLineKind::PromoCredit if line.amount_minor <= 0 => {}
            JournalLineKind::UsageOverage => {
                historical_usage += i128::from(line.amount_minor);
                has_historical_usage = true;
            }
            _ => {
                return Err(WalletError::Conflict(
                    "invoice charges require their original source correction before reissue"
                        .into(),
                ));
            }
        }
    }
    if has_historical_usage {
        let original = generation
            .as_ref()
            .map(|g| g.try_get::<Uuid, _>("original_journal_id"))
            .transpose()?
            .unwrap_or(journal);
        let repaired: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.plan_usage_corrections WHERE journal_id=$1)",
        )
        .bind(original)
        .fetch_one(&mut **tx)
        .await?;
        if !repaired || historical_usage != 0 {
            return Err(WalletError::Conflict(
                "historical usage requires its exact source correction before reissue".into(),
            ));
        }
    }
    let start: OffsetDateTime = p.try_get("period_start")?;
    let end: OffsetDateTime = p.try_get("period_end")?;
    let version = if let Some(generation) = &generation {
        if generation.try_get::<OffsetDateTime, _>("service_start")? != start
            || generation.try_get::<OffsetDateTime, _>("service_end")? != end
            || generation.try_get::<i64, _>("fee_minor")? < j.total_minor
        {
            return Err(WalletError::Conflict(
                "successor differs from its retained consideration".into(),
            ));
        }
        generation.try_get::<Uuid, _>("plan_version_id")?
    } else if j.accounting_policy_version == 2 {
        let a = sqlx::query(
            "SELECT * FROM oxsum.plan_accounting_periods WHERE plan_period_id=$1 AND journal_id=$2",
        )
        .bind(period)
        .bind(journal)
        .fetch_one(&mut **tx)
        .await?;
        if i128::from(a.try_get::<i64, _>("fee_minor")?) != base
            || a.try_get::<OffsetDateTime, _>("service_start")? != start
            || a.try_get::<OffsetDateTime, _>("service_end")? != end
        {
            return Err(WalletError::Conflict(
                "daily fee source differs from its frozen service terms".into(),
            ));
        }
        a.try_get::<Uuid, _>("plan_version_id")?
    } else {
        if p.try_get::<String, _>("authorization_kind")? != "payment_order" {
            return Err(WalletError::Conflict(
                "legacy service has no immutable sold-version authorization".into(),
            ));
        }
        let raw: Option<serde_json::Value> = sqlx::query_scalar("SELECT plan_payment FROM oxsum.payment_orders WHERE order_id=$1 AND organization_id=$2")
            .bind(p.try_get::<Uuid, _>("authorization_ref")?).bind(organization).fetch_one(&mut **tx).await?;
        let terms: PlanPaymentTerms = serde_json::from_value(raw.ok_or_else(|| {
            WalletError::Conflict("legacy payment has no immutable sold-version terms".into())
        })?)
        .map_err(error::invalid)?;
        if terms.customer_plan_id != j.customer_plan_id
            || terms.period_index != j.period_index
            || i128::from(terms.fee_minor) != base
        {
            return Err(WalletError::Conflict(
                "legacy fee differs from its original authorized terms".into(),
            ));
        }
        terms.plan_version_id
    };
    let sold = sqlx::query("SELECT price_minor,currency,accounting_policy_version FROM oxsum.sold_plans WHERE plan_version_id=$1 AND business_id=$2")
        .bind(version).bind(j.business_id).fetch_one(&mut **tx).await?;
    if (generation.is_none() && i128::from(sold.try_get::<i64, _>("price_minor")?) != base)
        || sold.try_get::<String, _>("currency")? != j.currency
        || sold.try_get::<i16, _>("accounting_policy_version")? != j.accounting_policy_version
    {
        return Err(WalletError::Conflict(
            "invoice differs from the original sold terms".into(),
        ));
    }
    let payable = retained_payable.unwrap_or(j.settled_minor);
    let lines = amounts::lines(&originals, request)?;
    let now = sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let (amounts, boundary) = amounts::calculate(
        &lines,
        payable,
        j.accounting_policy_version,
        start,
        end,
        now,
    )?;
    if amounts.original_total_minor != j.total_minor {
        return Err(WalletError::Conflict(
            "original consideration differs from its retained lines".into(),
        ));
    }
    let mut answer = InvoiceCorrectionPreview {
        journal_id: journal,
        statement_id: statement,
        organization_id: organization,
        customer_plan_id: j.customer_plan_id,
        plan_period_id: period,
        plan_version_id: version,
        currency: j.currency.clone(),
        accounting_policy_version: j.accounting_policy_version,
        service_start: start,
        service_end: end,
        recognition_boundary: boundary,
        source_revision: String::new(),
        lines,
        amounts,
    };
    let bytes = serde_json::to_vec(&serde_json::json!({"historyRevision": history_revision, "preview": answer, "dueAt": s.try_get::<Option<OffsetDateTime>, _>("due_at")?})).map_err(error::invalid)?;
    let mut digest = Sha256::new();
    digest.update(b"oxsum/invoice-correction-preview/v1\0");
    digest.update(bytes);
    answer.source_revision = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(answer)
}

/// Remove only a verified terminal void's exact opposite lines, never real credits.
fn original_lines(detail: &InvoiceJournalDetail) -> Result<Vec<InvoiceJournalLine>, WalletError> {
    if detail.journal.state != JournalState::Void {
        return Ok(detail.lines.clone());
    }
    let ids: BTreeSet<_> = detail.lines.iter().map(|line| line.line_id).collect();
    let mut reversed = BTreeSet::new();
    let mut originals = Vec::new();
    for line in &detail.lines {
        if let Some(source) = line.source_ref.filter(|id| ids.contains(id)) {
            let source = detail
                .lines
                .iter()
                .find(|old| old.line_id == source)
                .ok_or_else(|| error::invalid("void reversal source is missing"))?;
            if line.kind != source.kind
                || source.amount_minor.checked_neg() != Some(line.amount_minor)
                || !reversed.insert(source.line_id)
            {
                return Err(WalletError::Conflict(
                    "void reversal differs from its exact original line".into(),
                ));
            }
        } else {
            originals.push(line.clone());
        }
    }
    if originals
        .iter()
        .map(|line| line.line_id)
        .collect::<BTreeSet<_>>()
        != reversed
    {
        return Err(WalletError::Conflict(
            "void does not preserve one exact reversal per source line".into(),
        ));
    }
    Ok(originals)
}
