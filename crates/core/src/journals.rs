//! Plan fee journals and revenue allocation (issue #611, T20).
//!
//! A journal is the per-period invoice: `draft → posted → paid | void`.
//! Retained policy 1 issues it when the authorized period closes; policy 2
//! issues it at paid activation and recognizes completed UTC service days
//! separately. Boundary rollover, renewal and cancellation share
//! [`Db::journal_closing_period_in`], which catches up policy 2 without
//! issuing another invoice.
//!
//! Both policies collect a `planPeriod` order as deferred fees (`debit Cash /
//! credit Liabilities:UnearnedPlanFees`). Policy 1 recognizes the full fee at
//! close (`debit Receivables / credit Income:PlanFees`) and reclassifies the
//! prepayment into that receivable. Policy 2 releases completed service-day
//! shares from UnearnedPlanFees into PlanFees; its prepaid invoice does not
//! itself recognize income. Uncollected debt remains an open receivable for
//! explicit repayment.
//! Wallet-settled usage is already recognized by its settlement and is never
//! added to a new plan journal. Legacy overage lines remain readable for
//! explicit correction; renewal state and unpaid debt are separate facts.
//!
//! The ledger legs keep revenue, receivable and liability in their own
//! accounts: `Income:PlanFees`, `Income:Discounts`, `Assets:Receivables`,
//! `Liabilities:UnearnedPlanFees`, `Liabilities:RefundPayable`, `Equity:Promo`.
//! Every posting derives its idempotency key from the journal or order id, so
//! a replayed rollover, activation or event never books twice.
//!
//! Corrections never edit a posted journal: a refund appends a
//! credit-note-linked `discount` line, a lost dispute restores the receivable
//! (`paid → posted`, `disputed_minor` rising — the row guard's one backward
//! step), and a staff void appends reversing lines and the mirroring entry.
//! The line table refuses UPDATE/DELETE outright.

use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::business_staff as authority;
use crate::db::Db;
use crate::error::WalletError;
use crate::plans::SoldPlan;

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}

/// A journal's lifecycle — `draft` exists only inside the close transaction,
/// so the stored states a read can answer are `posted`, `paid` and `void`.
/// Wire spelling is camelCase like every API enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JournalState {
    /// Exists only inside the close transaction — never a resting state.
    Draft,
    /// Billed, awaiting settlement — the receivable is open.
    Posted,
    /// Fully settled — nothing more is owed.
    Paid,
    /// Cancelled by staff — reversing lines net it to zero.
    Void,
}
impl JournalState {
    /// The spelling `invoice_journals.state` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Posted => "posted",
            Self::Paid => "paid",
            Self::Void => "void",
        }
    }
    /// Reads a stored state back; an unknown one refuses rather than guesses.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "draft" => Some(Self::Draft),
            "posted" => Some(Self::Posted),
            "paid" => Some(Self::Paid),
            "void" => Some(Self::Void),
            _ => None,
        }
    }
}

/// What a journal line bills — the wire spelling is camelCase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JournalLineKind {
    /// The sold version's `price_minor` for the period.
    BaseFee,
    /// A historical duplicated wallet-overage charge. Retained for old journal
    /// reads and source-linked corrections; new closes never emit this kind.
    UsageOverage,
    /// A credit against the bill — a refund's credit note, or a complimentary
    /// grant's write-off.
    Discount,
    /// Promo money that covered the fee — debited off `Equity:Promo`.
    PromoCredit,
    /// Reserved — no writer emits one yet.
    Tax,
}
impl JournalLineKind {
    /// The spelling `invoice_journal_lines.kind` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BaseFee => "base_fee",
            Self::UsageOverage => "usage_overage",
            Self::Discount => "discount",
            Self::PromoCredit => "promo_credit",
            Self::Tax => "tax",
        }
    }
    #[must_use]
    fn parse(text: &str) -> Option<Self> {
        match text {
            "base_fee" => Some(Self::BaseFee),
            "usage_overage" => Some(Self::UsageOverage),
            "discount" => Some(Self::Discount),
            "promo_credit" => Some(Self::PromoCredit),
            "tax" => Some(Self::Tax),
            _ => None,
        }
    }
}

/// One period's invoice — the header the lists answer.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceJournal {
    /// Policy 1 recognizes at close; policy 2 invoices prepaid service at activation.
    pub accounting_policy_version: i16,
    /// Local journal identifier.
    pub journal_id: Uuid,
    /// Customer plan the journal bills.
    pub customer_plan_id: Uuid,
    /// Customer organization that owes the bill.
    pub organization_id: Uuid,
    /// Business that owns the journal, hidden from customer responses.
    #[serde(skip_serializing)]
    pub business_id: Uuid,
    /// The authorized period the journal bills, when plan-bound.
    pub plan_period_id: Option<Uuid>,
    /// Zero-based period number within the customer plan.
    pub period_index: i32,
    /// Lifecycle state of the journal.
    pub state: JournalState,
    /// The lines' sum — what the receivable was opened for.
    pub total_minor: i64,
    /// How much the clearing legs already covered.
    pub settled_minor: i64,
    /// Three-letter ISO 4217 currency code of the amounts.
    pub currency: String,
    /// The order whose payment completed the settlement.
    pub paid_order_id: Option<Uuid>,
    /// What open disputes contest of the receivable.
    pub disputed_minor: i64,
    /// Journal this one replaces, when reissued after a void.
    pub replaces_journal_id: Option<Uuid>,
    /// Staff's reason for voiding the journal.
    pub void_reason: Option<String>,
    /// When the journal was posted.
    #[serde(with = "time::serde::rfc3339::option")]
    pub posted_at: Option<OffsetDateTime>,
    /// When the journal was fully settled.
    #[serde(with = "time::serde::rfc3339::option")]
    pub paid_at: Option<OffsetDateTime>,
    /// When the journal was voided.
    #[serde(with = "time::serde::rfc3339::option")]
    pub voided_at: Option<OffsetDateTime>,
    /// When the journal row was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One line of a journal — signed `amount_minor`; charges positive, credits
/// negative.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceJournalLine {
    /// Local line identifier.
    pub line_id: Uuid,
    /// Journal the line belongs to.
    pub journal_id: Uuid,
    /// What the line bills or credits.
    pub kind: JournalLineKind,
    /// Signed: charges positive, credits negative.
    pub amount_minor: i64,
    /// Human-readable description of the line.
    pub description: String,
    /// The fact the line came from — period id, credit note id, lot or grant.
    pub source_ref: Option<Uuid>,
    /// When the line was written.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The detail read: a journal plus its lines.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceJournalDetail {
    /// The journal header.
    #[serde(flatten)]
    pub journal: InvoiceJournal,
    /// The journal's lines by creation time, with line-ID tie breaking.
    pub lines: Vec<InvoiceJournalLine>,
}

/// A cursor-paginated page of invoice journals.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceJournalPage {
    /// Journals in newest-first order.
    pub journals: Vec<InvoiceJournal>,
    /// Journal identifier to pass as `after` for the next page; `None` at the
    /// end.
    pub next_cursor: Option<Uuid>,
}

const JOURNAL_COLS: &str = "accounting_policy_version,journal_id,customer_plan_id,organization_id,business_id,\
     plan_period_id,period_index,state,total_minor,settled_minor,currency,\
     paid_order_id,disputed_minor,replaces_journal_id,void_reason,\
     posted_at,paid_at,voided_at,created_at,updated_at";

const LINE_COLS: &str = "line_id,journal_id,kind,amount_minor,description,source_ref,created_at";

fn journal_from_row(row: &PgRow) -> Result<InvoiceJournal, WalletError> {
    Ok(InvoiceJournal {
        accounting_policy_version: row.try_get("accounting_policy_version")?,
        journal_id: row.try_get("journal_id")?,
        customer_plan_id: row.try_get("customer_plan_id")?,
        organization_id: row.try_get("organization_id")?,
        business_id: row.try_get("business_id")?,
        plan_period_id: row.try_get("plan_period_id")?,
        period_index: row.try_get("period_index")?,
        state: JournalState::parse(&row.try_get::<String, _>("state")?)
            .ok_or_else(|| invalid("unknown journal state"))?,
        total_minor: row.try_get("total_minor")?,
        settled_minor: row.try_get("settled_minor")?,
        currency: row.try_get("currency")?,
        paid_order_id: row.try_get("paid_order_id")?,
        disputed_minor: row.try_get("disputed_minor")?,
        replaces_journal_id: row.try_get("replaces_journal_id")?,
        void_reason: row.try_get("void_reason")?,
        posted_at: row.try_get("posted_at")?,
        paid_at: row.try_get("paid_at")?,
        voided_at: row.try_get("voided_at")?,
        created_at: row.try_get("created_at")?,
    })
}

fn line_from_row(row: &PgRow) -> Result<InvoiceJournalLine, WalletError> {
    Ok(InvoiceJournalLine {
        line_id: row.try_get("line_id")?,
        journal_id: row.try_get("journal_id")?,
        kind: JournalLineKind::parse(&row.try_get::<String, _>("kind")?)
            .ok_or_else(|| invalid("unknown journal line kind"))?,
        amount_minor: row.try_get("amount_minor")?,
        description: row.try_get("description")?,
        source_ref: row.try_get("source_ref")?,
        created_at: row.try_get("created_at")?,
    })
}

/// What a journal write needs to know about the period it bills — the row
/// `plan_periods` recorded plus the sold version it ran under (passed
/// explicitly: the scheduled-change path swaps `plan_version_id` in the same
/// transaction, so the caller hands the pre-swap version over).
pub(crate) struct ClosingPeriod {
    pub plan_period_id: Uuid,
    pub period_index: i32,
    pub period_start: OffsetDateTime,
    pub period_end: OffsetDateTime,
    pub authorization_kind: String,
    pub authorization_ref: Uuid,
}

/// A computed line before it lands — kind, signed amount, description, source.
struct PendingLine {
    kind: JournalLineKind,
    amount_minor: i64,
    description: String,
    source_ref: Option<Uuid>,
}

impl Db {
    /// The customer's own journals — the entitlement surface's billing read,
    /// scoped to the caller's organization like every customer read.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn plan_journals(
        &self,
        organization_id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<InvoiceJournalPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let rows = sqlx::query(&format!(
            "SELECT {JOURNAL_COLS} FROM oxsum.invoice_journals \
             WHERE organization_id=$1 AND state <> 'draft' \
               AND ($2::uuid IS NULL OR (created_at, journal_id) < \
                    (SELECT created_at, journal_id FROM oxsum.invoice_journals WHERE journal_id=$2)) \
             ORDER BY created_at DESC, journal_id DESC LIMIT $3"
        ))
        .bind(organization_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(self.pool())
        .await?;
        let mut journals = rows
            .iter()
            .map(journal_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let more = journals.len() > limit as usize;
        journals.truncate(limit as usize);
        let next_cursor = if more {
            journals.last().map(|j| j.journal_id)
        } else {
            None
        };
        Ok(InvoiceJournalPage {
            journals,
            next_cursor,
        })
    }

    /// One journal plus its lines, scoped to the owning organization — a
    /// journal of another organization is a not-found, the same scoping the
    /// order read uses.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn plan_journal(
        &self,
        organization_id: Uuid,
        journal_id: Uuid,
    ) -> Result<Option<InvoiceJournalDetail>, WalletError> {
        let Some(journal) = self.journal_row(journal_id).await? else {
            return Ok(None);
        };
        if journal.organization_id != organization_id || journal.state == JournalState::Draft {
            return Ok(None);
        }
        Ok(Some(InvoiceJournalDetail {
            lines: self.journal_lines(journal_id).await?,
            journal,
        }))
    }

    /// A journal's itemization by creation time, with line-ID tie breaking.
    /// Lines written in the same transaction can share a timestamp; their
    /// position does not imply a billing-kind or insertion order.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn journal_lines(
        &self,
        journal_id: Uuid,
    ) -> Result<Vec<InvoiceJournalLine>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {LINE_COLS} FROM oxsum.invoice_journal_lines \
             WHERE journal_id=$1 ORDER BY created_at, line_id"
        ))
        .bind(journal_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(line_from_row).collect()
    }

    /// The staff cross-organization journal list (issue #611): `state` narrows
    /// to one lifecycle state (`posted` is the dunning work list), `period`
    /// bounds the close-side ordering to journals posted inside a window, and
    /// `customerPlanId` scopes to one binding. `ReportsRead` is required in
    /// the transaction.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a bad state or limit; storage
    /// failures surface as [`WalletError`].
    pub async fn business_journals(
        &self,
        token: &str,
        state: Option<&str>,
        customer_plan_id: Option<Uuid>,
        period_index: Option<i32>,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<InvoiceJournalPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        if let Some(state) = state
            && JournalState::parse(state).is_none()
        {
            return Err(invalid("invalid journal state filter"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(
            &mut tx,
            business,
            token,
            crate::BusinessCapability::ReportsRead,
        )
        .await?;
        let rows = sqlx::query(&format!(
            "SELECT {JOURNAL_COLS} FROM oxsum.invoice_journals \
             WHERE business_id=$1 AND state <> 'draft' \
               AND ($3::text IS NULL OR state=$3) \
               AND ($4::uuid IS NULL OR customer_plan_id=$4) \
               AND ($5::int IS NULL OR period_index=$5) \
               AND ($2::uuid IS NULL OR (created_at, journal_id) < \
                    (SELECT created_at, journal_id FROM oxsum.invoice_journals WHERE journal_id=$2)) \
             ORDER BY created_at DESC, journal_id DESC LIMIT $6"
        ))
        .bind(business)
        .bind(after)
        .bind(state)
        .bind(customer_plan_id)
        .bind(period_index)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut journals = rows
            .iter()
            .map(journal_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let more = journals.len() > limit as usize;
        journals.truncate(limit as usize);
        let next_cursor = if more {
            journals.last().map(|j| j.journal_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(InvoiceJournalPage {
            journals,
            next_cursor,
        })
    }

    /// One journal the business owns — the staff read behind `void`.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn business_journal(
        &self,
        token: &str,
        journal_id: Uuid,
    ) -> Result<Option<InvoiceJournalDetail>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(
            &mut tx,
            business,
            token,
            crate::BusinessCapability::ReportsRead,
        )
        .await?;
        let Some(journal) = journal_in(&mut tx, journal_id).await? else {
            tx.commit().await?;
            return Ok(None);
        };
        if journal.business_id != business || journal.state == JournalState::Draft {
            tx.commit().await?;
            return Ok(None);
        }
        let lines = lines_in(&mut tx, journal_id).await?;
        tx.commit().await?;
        Ok(Some(InvoiceJournalDetail { journal, lines }))
    }

    /// The staff void: appends reversing lines so the journal nets to zero,
    /// posts the mirroring ledger entry — the open receivable reversed on a
    /// `posted` journal, the settlement unwound to `RefundPayable` on a `paid`
    /// one — and lands `void` with the reason and a `finance_audit` row. A
    /// correction that needs a different bill then reissues under
    /// `replaces_journal_id`; the journal itself is never edited or deleted.
    ///
    /// `wallet` is the journal organization's ledger, resolved by the caller.
    /// `FundsWrite` is required in the transaction; the replay of an
    /// `idempotency_key` answers the standing voided journal.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unknown or draft journal;
    /// [`WalletError::Conflict`] for changed replay terms, an unrelated wallet,
    /// or ambiguous/reissued source documents; storage failures surface as
    /// [`WalletError`].
    pub async fn void_invoice_journal(
        &self,
        token: &str,
        wallet: &crate::Wallet,
        journal_id: Uuid,
        reason: &str,
        idempotency_key: &str,
        on: time::Date,
    ) -> Result<InvoiceJournalDetail, WalletError> {
        authority::validate(idempotency_key, Some(reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            crate::BusinessCapability::FundsWrite,
        )
        .await?;
        let coordinates = sqlx::query(
            "SELECT j.customer_plan_id,o.tenant_id FROM oxsum.invoice_journals j \
             JOIN oxsum.organizations o USING(organization_id) \
             WHERE j.journal_id=$1 AND j.business_id=$2 AND j.state<>'draft'",
        )
        .bind(journal_id)
        .bind(business)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("unknown journal".into()))?;
        if coordinates.try_get::<String, _>("tenant_id")? != wallet.tenant_id() {
            return Err(WalletError::Conflict(
                "journal wallet does not own the customer".into(),
            ));
        }
        let plan: Uuid = coordinates.try_get("customer_plan_id")?;
        sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE")
            .bind(plan).fetch_one(&mut *tx).await?;
        let journal = journal_in(&mut tx, journal_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("unknown journal".into()))?;
        if let Some(prior) = sqlx::query("SELECT target,detail FROM oxsum.finance_audit WHERE business_id=$1 AND action='journal.void' AND idempotency_key=$2")
            .bind(business).bind(idempotency_key).fetch_optional(&mut *tx).await? {
            let detail:serde_json::Value=prior.try_get("detail")?;
            if prior.try_get::<String,_>("target")?!=journal_id.to_string() || detail["reason"]!=reason {
                return Err(WalletError::Conflict("journal void key names different request terms".into()));
            }
            let lines=lines_in(&mut tx,journal_id).await?;
            tx.commit().await?;
            return Ok(InvoiceJournalDetail{journal,lines});
        }
        let reversal_entry = self
            .void_journal_financial_in(&mut tx, wallet, &journal, reason, on)
            .await?;
        let audit_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.finance_audit\
             (audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) \
             VALUES($1,$2,$3,'journal.void',$4,$5,$6)",
        )
        .bind(audit_id)
        .bind(business)
        .bind(actor.user_id)
        .bind(journal_id.to_string())
        .bind(serde_json::json!({
            "organizationId": journal.organization_id,
            "periodIndex": journal.period_index,
            "totalMinor": journal.total_minor,
            "settledMinor": journal.settled_minor,
            "previousState": journal.state.as_str(),
            "reversalEntryId": reversal_entry,
            "reason": reason,
        }))
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(db) = &e
                && db.constraint() == Some("finance_audit_business_id_action_idempotency_key_key")
            {
                // A replayed void answers the journal as it stands — the row
                // guard lets the no-op update through since nothing changes.
                return WalletError::Conflict("journal void idempotency key already used".into());
            }
            WalletError::Storage(e.into())
        })?;
        let voided = journal_in(&mut tx, journal_id)
            .await?
            .ok_or_else(|| invalid("the journal did not land"))?;
        let lines = lines_in(&mut tx, journal_id).await?;
        tx.commit().await?;
        Ok(InvoiceJournalDetail {
            journal: voided,
            lines,
        })
    }

    /// Shared cashless financial void. The caller owns business and plan locks,
    /// then this locked journal; append lock precedes statement validation.
    pub(crate) async fn void_journal_financial_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &crate::Wallet,
        journal: &InvoiceJournal,
        reason: &str,
        on: time::Date,
    ) -> Result<Option<Uuid>, WalletError> {
        let journal_id = journal.journal_id;
        if journal.state == JournalState::Void {
            // A new explicit review may clear an old writer's stale debt flag.
            // Exact audited replays return before this helper and stay inert.
            crate::plan_debt::refresh_in(tx, journal.customer_plan_id).await?;
            return Ok(None);
        }
        if journal.state == JournalState::Draft || journal.replaces_journal_id.is_some() {
            return Err(WalletError::Conflict(
                "draft or reissued journal requires explicit financial reconciliation".into(),
            ));
        }
        crate::statement_payments::guard_journal_in(tx, journal_id).await?;
        wallet.lock_append_in(tx).await?;
        let documents =
            crate::statement_payments::voiding::documents_in(tx, wallet, journal).await?;
        if documents.is_none() {
            self.validate_journal_void_statements_in(tx, journal)
                .await?;
        }
        let lines = lines_in(tx, journal_id).await?;
        // Reverse every line onto the journal itself — the void's record is
        // the second set of lines that nets the bill to zero.
        let mut charges = 0i64;
        let mut discount = 0i64;
        let mut promo = 0i64;
        for line in &lines {
            match line.kind {
                JournalLineKind::BaseFee | JournalLineKind::UsageOverage | JournalLineKind::Tax => {
                    charges += line.amount_minor;
                }
                JournalLineKind::Discount => discount -= line.amount_minor,
                JournalLineKind::PromoCredit => promo -= line.amount_minor,
            }
            insert_line_in(
                tx,
                journal_id,
                line.kind,
                -line.amount_minor,
                &format!("void reversal of {}", line.description),
                Some(line.line_id),
            )
            .await?;
        }
        let entry = if journal.accounting_policy_version == 2 {
            self.void_daily_plan_in(tx, wallet, journal, on).await?
        } else {
            wallet
                .plan_journal_void_in(
                    tx,
                    &journal_id,
                    charges,
                    discount,
                    promo,
                    journal.settled_minor,
                    journal.state == JournalState::Paid,
                    on,
                )
                .await?
                .map(|r| *r.entry_id.as_uuid())
        };

        sqlx::query("UPDATE oxsum.invoice_journals SET state='void',voided_at=now(),void_reason=$2,updated_at=now() WHERE journal_id=$1")
            .bind(journal_id).bind(reason).execute(&mut **tx).await?;
        crate::statement_payments::voiding::allocate_in(tx, wallet, journal, entry).await?;
        if let Some(documents) = documents {
            for id in documents {
                self.void_plan_document_in(tx, id, reason).await?;
            }
        } else {
            self.void_statement_for_journal_in(tx, journal_id, reason)
                .await?;
        }
        crate::plan_debt::refresh_in(tx, journal.customer_plan_id).await?;
        Ok(entry)
    }
}

/// The journal row inside the caller's transaction — `FOR UPDATE` so the
/// settlement and correction writers serialize on it.
pub(crate) async fn journal_in(
    tx: &mut Transaction<'_, Postgres>,
    journal_id: Uuid,
) -> Result<Option<InvoiceJournal>, WalletError> {
    let row = sqlx::query(&format!(
        "SELECT {JOURNAL_COLS} FROM oxsum.invoice_journals WHERE journal_id=$1 FOR UPDATE"
    ))
    .bind(journal_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(journal_from_row).transpose()
}

/// Read one customer invoice and its lines within the caller's snapshot.
pub(crate) async fn journal_snapshot_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    journal: Uuid,
) -> Result<Option<InvoiceJournalDetail>, WalletError> {
    let row=sqlx::query(&format!("SELECT {JOURNAL_COLS} FROM oxsum.invoice_journals WHERE journal_id=$1 AND organization_id=$2 AND state<>'draft'"))
        .bind(journal).bind(organization).fetch_optional(&mut **tx).await?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some(InvoiceJournalDetail {
        journal: journal_from_row(&row)?,
        lines: lines_in(tx, journal).await?,
    }))
}

async fn lines_in(
    tx: &mut Transaction<'_, Postgres>,
    journal_id: Uuid,
) -> Result<Vec<InvoiceJournalLine>, WalletError> {
    journal_lines_in(tx, journal_id).await
}

/// The journal lines a statement restates (issue #612, T21): the same read
/// `lines_in` runs, named for the consumer so the statement module need not
/// reach inside the journal's own helpers.
pub(crate) async fn lines_for_statement_in(
    tx: &mut Transaction<'_, Postgres>,
    journal_id: Uuid,
) -> Result<Vec<InvoiceJournalLine>, WalletError> {
    journal_lines_in(tx, journal_id).await
}

async fn journal_lines_in(
    tx: &mut Transaction<'_, Postgres>,
    journal_id: Uuid,
) -> Result<Vec<InvoiceJournalLine>, WalletError> {
    let rows = sqlx::query(&format!(
        "SELECT {LINE_COLS} FROM oxsum.invoice_journal_lines \
         WHERE journal_id=$1 ORDER BY created_at, line_id"
    ))
    .bind(journal_id)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(line_from_row).collect()
}

/// Inserts one journal line — the only write the lines table accepts.
async fn insert_line_in(
    tx: &mut Transaction<'_, Postgres>,
    journal_id: Uuid,
    kind: JournalLineKind,
    amount_minor: i64,
    description: &str,
    source_ref: Option<Uuid>,
) -> Result<Uuid, WalletError> {
    let line_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO oxsum.invoice_journal_lines \
             (line_id, journal_id, kind, amount_minor, description, source_ref) \
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(line_id)
    .bind(journal_id)
    .bind(kind.as_str())
    .bind(amount_minor)
    .bind(description)
    .bind(source_ref)
    .execute(&mut **tx)
    .await?;
    Ok(line_id)
}

impl Db {
    /// Reads one journal by id — the row the detail and staff reads project.
    async fn journal_row(&self, journal_id: Uuid) -> Result<Option<InvoiceJournal>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {JOURNAL_COLS} FROM oxsum.invoice_journals WHERE journal_id=$1"
        ))
        .bind(journal_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(journal_from_row).transpose()
    }

    /// Issue the v2 prepaid fee invoice without recognizing service revenue.
    /// Activation already owns the plan and posted the original prepayment.
    pub(crate) async fn invoice_daily_plan_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &crate::Wallet,
        plan: Uuid,
        sold: &SoldPlan,
        now: OffsetDateTime,
    ) -> Result<(), WalletError> {
        let row=sqlx::query("SELECT p.*,c.organization_id FROM oxsum.plan_periods p JOIN oxsum.customer_plans c USING(customer_plan_id) WHERE p.customer_plan_id=$1 AND p.state='active' ORDER BY p.period_index DESC LIMIT 1")
            .bind(plan).fetch_one(&mut **tx).await?;
        let period: Uuid = row.try_get("plan_period_id")?;
        let kind: String = row.try_get("authorization_kind")?;
        let order: Uuid = row.try_get("authorization_ref")?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT journal_id FROM oxsum.invoice_journals WHERE plan_period_id=$1",
        )
        .bind(period)
        .fetch_optional(&mut **tx)
        .await?;
        if existing.is_some() {
            return Err(WalletError::Conflict(
                "new daily activation already has a historical invoice".into(),
            ));
        }
        if kind == "payment_order" {
            let prepaid = wallet
                .plan_source_credit_in(
                    tx,
                    &format!("payment-order/{order}/prepay"),
                    crate::wallet::UNEARNED_PLAN_FEES,
                )
                .await?;
            if prepaid != sold.price_minor {
                return Err(WalletError::Conflict(
                    "daily activation requires its exact original prepayment".into(),
                ));
            }
        } else if sold.price_minor != 0 {
            return Err(invalid("a complimentary daily period must be free"));
        }
        let journal = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.invoice_journals(journal_id,customer_plan_id,organization_id,business_id,plan_period_id,period_index,state,total_minor,settled_minor,currency,accounting_policy_version) VALUES($1,$2,$3,$4,$5,$6,'draft',$7,0,$8,2)")
            .bind(journal).bind(plan).bind(row.try_get::<Uuid,_>("organization_id")?).bind(sold.business_id).bind(period).bind(row.try_get::<i32,_>("period_index")?).bind(sold.price_minor).bind(&sold.currency).execute(&mut **tx).await?;
        insert_line_in(
            tx,
            journal,
            JournalLineKind::BaseFee,
            sold.price_minor,
            "Prepaid service fee; income follows completed UTC service days",
            Some(period),
        )
        .await?;
        sqlx::query("UPDATE oxsum.invoice_journals SET state='posted',settled_minor=$2,posted_at=now() WHERE journal_id=$1").bind(journal).bind(sold.price_minor).execute(&mut **tx).await?;
        sqlx::query("UPDATE oxsum.invoice_journals SET state='paid',paid_order_id=$2,paid_at=now() WHERE journal_id=$1").bind(journal).bind((kind=="payment_order").then_some(order)).execute(&mut **tx).await?;
        let journal = journal_in(tx, journal)
            .await?
            .ok_or_else(|| invalid("daily journal did not land"))?;
        self.initialize_plan_accounting_in(tx, &journal, sold.plan_version_id, now.date())
            .await?;
        self.issue_statement_for_journal_in(
            tx,
            &journal,
            Some(row.try_get("period_start")?),
            Some(row.try_get("period_end")?),
        )
        .await?;
        if sold.price_minor > 0 {
            self.reconcile_statement_for_journal_in(tx, &journal, order, sold.price_minor)
                .await?;
        }
        self.recognize_plan_period_in(tx, wallet, journal.journal_id, now, now.date())
            .await?;
        Ok(())
    }

    /// Finalizes the accounting a closing plan period needs, shared by
    /// rollover's cancel/change/past-due arms and activation's renewal arm.
    /// Policy 2 catches up its original activation invoice and returns `None`.
    /// For policy 1, the journal lands `draft` and walks to `posted` or `paid`
    /// inside this transaction beside its accrual entry.
    ///
    /// Policy-1 lines: `base_fee` the sold version's price; `discount` the credit notes
    /// applied to the order that
    /// authorized the period; `promo_credit` promo covering the fee — a
    /// complimentary grant's full coverage, or a payment's bundle promo leg,
    /// clamped to what the promo pool settles. `total_minor` is their sum.
    ///
    /// Policy-1 settlement: a `payment_order`-authorized period's prepayment
    /// reclassifies `debit UnearnedPlanFees / credit Receivables` up to the
    /// journal's total — `paid` when the prepayment covers it, `posted` for
    /// the remainder, retained for explicit statement repayment or correction.
    /// A complimentary period is covered in full by its credit lines — `paid`
    /// with nothing owed.
    ///
    /// Idempotent by construction: the `(customer_plan_id, period_index)`
    /// partial unique refuses a second live journal, so a replayed close is a
    /// no-op returning `None` — every caller treats `None` as "already billed".
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]; a promo accrual the pool
    /// cannot carry surfaces [`WalletError::InsufficientFunds`].
    pub(crate) async fn journal_closing_period_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &crate::Wallet,
        plan_row: &PgRow,
        period: &ClosingPeriod,
        sold: &SoldPlan,
        on: time::Date,
    ) -> Result<Option<Uuid>, WalletError> {
        let customer_plan_id: Uuid = plan_row.try_get("customer_plan_id")?;
        let organization_id: Uuid = plan_row.try_get("organization_id")?;
        // An immutable v2 accounting period owns its original invoice even
        // after a financial void. Closing must never create a replacement fee.
        let accounted: Option<Uuid> = sqlx::query_scalar(
            "SELECT journal_id FROM oxsum.plan_accounting_periods WHERE plan_period_id=$1",
        )
        .bind(period.plan_period_id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(journal) = accounted {
            let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut **tx)
                .await?;
            self.recognize_plan_period_in(tx, wallet, journal, now, on)
                .await?;
            return Ok(None);
        }
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT journal_id FROM oxsum.invoice_journals \
             WHERE customer_plan_id=$1 AND period_index=$2 AND state <> 'void'",
        )
        .bind(customer_plan_id)
        .bind(period.period_index)
        .fetch_optional(&mut **tx)
        .await?;
        if existing.is_some() {
            return Ok(None);
        }
        if sold.accounting_policy_version == 2 {
            return Err(WalletError::Conflict(
                "daily period lacks its original activation invoice".into(),
            ));
        }

        let mut lines: Vec<PendingLine> = Vec::new();
        // Base fee: what the period's sold version charged.
        lines.push(PendingLine {
            kind: JournalLineKind::BaseFee,
            amount_minor: sold.price_minor,
            description: format!(
                "plan {} v{} base fee, period {}",
                sold.name, sold.version, period.period_index
            ),
            source_ref: Some(period.plan_period_id),
        });

        // Usage already settled through the wallet or credit line has its own
        // Income:Usage entry. Summing usage_records here would bill that same
        // turn again as Receivables/PlanFees. Policy versions change fee timing,
        // never this once-only usage boundary.

        // Pre-close cash returns already reduced deferred funding. Their
        // immutable allocation, including a late success on a reversed note,
        // is the source of the closing credit; note state alone is not cash.
        if period.authorization_kind == "payment_order" {
            let unknown: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.credit_notes n JOIN oxsum.refunds r ON r.refund_id=n.refund_id WHERE n.order_id=$1 AND (n.state='applied' OR r.state='succeeded') AND NOT EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=r.refund_id))")
                .bind(period.authorization_ref).fetch_one(&mut **tx).await?;
            if unknown {
                return Err(WalletError::Conflict(
                    "recover the original plan refund before closing its period".into(),
                ));
            }
            let notes = sqlx::query(
                "SELECT n.credit_note_id, e.deferred_minor AS amount_minor, n.reason \
                 FROM oxsum.plan_refund_effects e JOIN oxsum.credit_notes n USING(credit_note_id) \
                 WHERE e.order_id=$1 AND e.plan_period_id=$2 AND e.journal_id IS NULL AND e.deferred_minor>0",
            )
            .bind(period.authorization_ref)
            .bind(period.plan_period_id)
            .fetch_all(&mut **tx)
            .await?;
            for note in &notes {
                let note_id: Uuid = note.try_get("credit_note_id")?;
                let amount: i64 = note.try_get("amount_minor")?;
                let reason: String = note.try_get("reason")?;
                lines.push(PendingLine {
                    kind: JournalLineKind::Discount,
                    amount_minor: -amount,
                    description: format!("credit note {note_id}: {reason}"),
                    source_ref: Some(note_id),
                });
            }
        }

        let charge_total: i64 = lines.iter().map(|l| l.amount_minor).sum();

        // Coverage: a complimentary period is covered in full — the grant's
        // `discount` line writes the whole fee off, so nothing is ever owed on
        // it (spec: promo/discount covering the whole fee, state `paid`). A
        // paid period's order may carry a bundle promo leg, which funds the
        // fee from `Equity:Promo` up to `promo_minor`, clamped to what the pool
        // settles — `FundedReservations` is the backstop inside the append.
        let mut promo_cover = 0i64;
        let mut grant_cover = 0i64;
        if period.authorization_kind == "complimentary" {
            grant_cover = charge_total.max(0);
        } else if period.authorization_kind == "payment_order" {
            let order_promo: i64 = sqlx::query_scalar(
                "SELECT promo_minor FROM oxsum.payment_orders WHERE order_id=$1",
            )
            .bind(period.authorization_ref)
            .fetch_optional(&mut **tx)
            .await?
            .unwrap_or(0);
            if order_promo > 0 && charge_total > 0 {
                let promo_balance = wallet.promo_settled_in(&mut *tx.as_mut()).await?;
                promo_cover = charge_total.min(order_promo).min(promo_balance.max(0));
            }
        }
        if promo_cover > 0 {
            lines.push(PendingLine {
                kind: JournalLineKind::PromoCredit,
                amount_minor: -promo_cover,
                description: format!("promo credit, period {}", period.period_index),
                source_ref: Some(period.authorization_ref),
            });
        }
        if grant_cover > 0 {
            lines.push(PendingLine {
                kind: JournalLineKind::Discount,
                amount_minor: -grant_cover,
                description: format!(
                    "complimentary grant {}, period {}",
                    period.authorization_ref, period.period_index
                ),
                source_ref: Some(period.authorization_ref),
            });
        }
        // `total_minor` is the lines' sum exactly — the row guard enforces
        // the same figure on every later adjustment.
        let total: i64 = charge_total - promo_cover - grant_cover;

        // The journal lands `draft` then posts in the same transaction —
        // close is atomic, draft is never a resting state outside it.
        let journal_id = Uuid::new_v4();
        let business: Uuid =
            sqlx::query_scalar("SELECT business_id FROM oxsum.sold_plans WHERE plan_version_id=$1")
                .bind(sold.plan_version_id)
                .fetch_one(&mut **tx)
                .await?;
        sqlx::query(
            "INSERT INTO oxsum.invoice_journals \
                 (journal_id,customer_plan_id,organization_id,business_id,plan_period_id,\
                  period_index,state,total_minor,settled_minor,currency) \
             VALUES ($1,$2,$3,$4,$5,$6,'draft',$7,0,$8)",
        )
        .bind(journal_id)
        .bind(customer_plan_id)
        .bind(organization_id)
        .bind(business)
        .bind(period.plan_period_id)
        .bind(period.period_index)
        .bind(total)
        .bind(&sold.currency)
        .execute(&mut **tx)
        .await?;
        for line in &lines {
            insert_line_in(
                tx,
                journal_id,
                line.kind,
                line.amount_minor,
                &line.description,
                line.source_ref,
            )
            .await?;
        }
        // Track contested money from open disputes on the authorizing order.
        if period.authorization_kind == "payment_order" {
            let disputed: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.disputes \
                 WHERE order_id=$1 AND state IN ('opened','evidence_due')",
            )
            .bind(period.authorization_ref)
            .fetch_one(&mut **tx)
            .await?;
            if disputed > 0 {
                sqlx::query(
                    "UPDATE oxsum.invoice_journals SET disputed_minor=$2 WHERE journal_id=$1",
                )
                .bind(journal_id)
                .bind(disputed)
                .execute(&mut **tx)
                .await?;
            }
        }

        // The accrual: charge legs recognize revenue, credit legs clear the
        // receivable they carry — one entry beside the journal row.
        let charges: i64 = lines
            .iter()
            .filter(|l| l.amount_minor > 0)
            .map(|l| l.amount_minor)
            .sum();
        let discounts: i64 = lines
            .iter()
            .filter(|l| l.kind == JournalLineKind::Discount)
            .map(|l| -l.amount_minor)
            .sum();
        wallet
            .plan_journal_accrual_in(tx, &journal_id, charges, discounts, promo_cover, on)
            .await?;

        // Settlement: a paid order's prepayment reclassifies into the
        // receivable — `paid` when it covers the bill, `posted` for the rest.
        // A total at or under zero (credits covering the whole bill) is `paid`
        // outright — nothing was ever owed.
        let mut settled = 0i64;
        let mut paid_order = None;
        if total > 0 && period.authorization_kind == "payment_order" {
            let order = sqlx::query(
                "SELECT amount_minor, status FROM oxsum.payment_orders WHERE order_id=$1",
            )
            .bind(period.authorization_ref)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some(order) = order {
                let amount: i64 = order.try_get("amount_minor")?;
                let status: String = order.try_get("status")?;
                // Only money that arrived may settle: paid or fulfilled orders.
                if matches!(status.as_str(), "paid" | "fulfilled") {
                    let prepaid = wallet
                        .plan_source_credit_in(
                            tx,
                            &format!("payment-order/{}/prepay", period.authorization_ref),
                            crate::wallet::UNEARNED_PLAN_FEES,
                        )
                        .await?;
                    if prepaid != amount {
                        return Err(WalletError::Conflict(
                            "original period prepayment requires finance review".into(),
                        ));
                    }
                    let refunded:i64=sqlx::query_scalar("SELECT COALESCE(SUM(cash_minor),0)::bigint FROM oxsum.plan_refund_effects WHERE order_id=$1")
                        .bind(period.authorization_ref).fetch_one(&mut **tx).await?;
                    let apply = total.min(amount.saturating_sub(refunded).max(0));
                    if apply > 0 {
                        wallet
                            .plan_prepay_settle_in(tx, &journal_id, apply, on)
                            .await?;
                        settled += apply;
                    }
                    if settled >= total {
                        paid_order = Some(period.authorization_ref);
                    }
                }
            }
        }
        // The state machine walks one step at a time even inside the close:
        // `draft → posted` publishes the bill, then `posted → paid` records
        // the settlement the prepay already covered.
        sqlx::query(
            "UPDATE oxsum.invoice_journals \
             SET state='posted', settled_minor=$2, posted_at=now(), updated_at=now() \
             WHERE journal_id=$1",
        )
        .bind(journal_id)
        .bind(settled)
        .execute(&mut **tx)
        .await?;
        if settled >= total {
            sqlx::query(
                "UPDATE oxsum.invoice_journals \
                 SET state='paid', paid_order_id=$2, paid_at=now(), updated_at=now() \
                 WHERE journal_id=$1",
            )
            .bind(journal_id)
            .bind(paid_order)
            .execute(&mut **tx)
            .await?;
        }
        // The statement the customer reads (issue #612, T21): issued inside
        // this same transaction so the receivable and its customer-facing
        // document are one commit — `open → issued` beside the journal's own
        // `draft → posted → paid` walk.
        if let Some(journal) = journal_in(tx, journal_id).await? {
            self.issue_statement_for_journal_in(
                tx,
                &journal,
                Some(period.period_start),
                Some(period.period_end),
            )
            .await?;
            // Every prepaid amount reaches the customer statement, including
            // a partial cover when overage leaves the journal posted. Requiring
            // the whole journal to be paid would charge its base fee twice in
            // the outstanding statement even though the ledger settled it.
            if journal.settled_minor > 0 && period.authorization_kind == "payment_order" {
                self.reconcile_statement_for_journal_in(
                    tx,
                    &journal,
                    period.authorization_ref,
                    journal.settled_minor,
                )
                .await?;
            }
        }
        Ok(Some(journal_id))
    }

    /// Append a source-linked refund correction and synchronize the original
    /// statement. Payment counters describe net original cash, never debt relief.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn correct_refunded_journal_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        journal_id: Uuid,
        credit_note_id: Uuid,
        credit: i64,
        old_credit: i64,
        settled: i64,
        reason: &str,
    ) -> Result<(), WalletError> {
        let journal = journal_in(tx, journal_id)
            .await?
            .ok_or_else(|| invalid("refund journal disappeared"))?;
        let adjustment = credit - old_credit;
        if adjustment != 0 {
            insert_line_in(
                tx,
                journal_id,
                JournalLineKind::Discount,
                -adjustment,
                &format!("cash refund allocation for note {credit_note_id}: {reason}"),
                Some(credit_note_id),
            )
            .await?;
        }
        let total = journal
            .total_minor
            .checked_sub(adjustment)
            .ok_or_else(|| invalid("refund total overflows"))?;
        if total < 0 || settled < 0 || settled > total {
            return Err(WalletError::Conflict(
                "refund needs explicit journal allocation review".into(),
            ));
        }
        sqlx::query("UPDATE oxsum.invoice_journals SET total_minor=$2,settled_minor=$3,updated_at=now() WHERE journal_id=$1")
            .bind(journal_id).bind(total).bind(settled).execute(&mut **tx).await?;
        self.refund_statement_for_journal_in(tx, &journal, credit_note_id, credit, reason)
            .await?;
        Ok(())
    }

    /// The journal half of `record_order_dispute`: an opened dispute on a
    /// `planPeriod` order contests the journal it settled — `disputed_minor`
    /// tracks the contested figure, rising on each dispute the order draws.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub(crate) async fn track_journal_dispute_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        order_id: Uuid,
        amount_minor: i64,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.invoice_journals j SET disputed_minor=disputed_minor+$2,updated_at=now() \
             WHERE j.paid_order_id=$1 AND j.state IN ('posted','paid')",
        )
        .bind(order_id)
        .bind(amount_minor)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Record a lost dispute's complete cash fact against its original period.
    /// Previously refunded cash cannot create another customer receivable.
    /// Excess is retained reconciliation suspense; replay reads its actual entry.
    pub(crate) async fn recharge_journal_dispute_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &crate::Wallet,
        order_id: Uuid,
        dispute_id: Uuid,
        amount_minor: i64,
        on: time::Date,
    ) -> Result<Option<Uuid>, WalletError> {
        if amount_minor <= 0 {
            return Err(invalid("chargeback cash must be positive"));
        }
        let organization:Option<Uuid>=sqlx::query_scalar("SELECT organization_id FROM oxsum.payment_orders WHERE order_id=$1 AND purpose='plan_period' AND status IN ('paid','fulfilled')")
            .bind(order_id).fetch_optional(&mut **tx).await?;
        let Some(organization) = organization else {
            return Ok(None);
        };
        let tenant: String = sqlx::query_scalar(
            "SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1",
        )
        .bind(organization)
        .fetch_one(&mut **tx)
        .await?;
        if tenant != wallet.tenant_id() {
            return Err(invalid("chargeback wallet does not own the order"));
        }
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text,0))",
        )
        .bind(organization.to_string())
        .execute(&mut **tx)
        .await?;
        sqlx::query("SELECT subscription_id FROM oxsum.org_subscriptions WHERE organization_id=$1 AND state='active' FOR UPDATE")
            .bind(organization).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=(SELECT purpose_ref FROM oxsum.payment_orders WHERE order_id=$1) FOR UPDATE")
            .bind(order_id).fetch_optional(&mut **tx).await?;
        let corrected =
            crate::plan_usage_corrections::corrected_order_journals_in(tx, order_id).await?;
        if let Some(original_journal) = corrected.first() {
            // Corrected funding may span an old bill and a later prepayment.
            // Keep the full observed cash loss without guessing which payable
            // it extinguished. The immutable map and audit retain every source.
            sqlx::query("SELECT journal_id FROM oxsum.invoice_journals WHERE journal_id=ANY($1) ORDER BY journal_id FOR UPDATE")
                .bind(&corrected).fetch_all(&mut **tx).await?;
            let key = format!("dispute-recharge/{dispute_id}");
            let cash = wallet
                .plan_source_credit_in(tx, &key, "Assets:Cash")
                .await?;
            if cash > 0 && cash != amount_minor {
                return Err(WalletError::Conflict(
                    "chargeback amount changed on replay".into(),
                ));
            }
            if cash == 0 {
                wallet
                    .plan_journal_dispute_recharge_in(
                        tx,
                        original_journal,
                        &dispute_id,
                        amount_minor,
                        0,
                        0,
                        on,
                    )
                    .await?;
            }
            return Ok(Some(*original_journal));
        }
        let row=sqlx::query(&format!("SELECT {JOURNAL_COLS} FROM oxsum.invoice_journals j WHERE EXISTS(SELECT 1 FROM oxsum.plan_periods p WHERE p.plan_period_id=j.plan_period_id AND p.authorization_kind='payment_order' AND p.authorization_ref=$1) AND j.state IN ('posted','paid','void') ORDER BY j.created_at DESC LIMIT 1 FOR UPDATE OF j"))
            .bind(order_id).fetch_optional(&mut **tx).await?;
        let Some(journal) = row.as_ref().map(journal_from_row).transpose()? else {
            return Ok(None);
        };
        let key = format!("dispute-recharge/{dispute_id}");
        let cash = wallet
            .plan_source_credit_in(tx, &key, "Assets:Cash")
            .await?;
        if cash > 0 {
            if cash != amount_minor {
                return Err(WalletError::Conflict(
                    "chargeback amount changed on replay".into(),
                ));
            }
            return Ok(Some(journal.journal_id));
        }
        // A later statement payment cannot replenish this original order's
        // returnable source. Repeated losses still retain full external Cash,
        // but only the original paid amount less prior cash returns restores AR.
        let original_amount: i64 =
            sqlx::query_scalar("SELECT amount_minor FROM oxsum.payment_orders WHERE order_id=$1")
                .bind(order_id)
                .fetch_one(&mut **tx)
                .await?;
        let refunded_cash:i64=sqlx::query_scalar("SELECT COALESCE(SUM(cash_minor),0)::bigint FROM oxsum.plan_refund_effects WHERE order_id=$1").bind(order_id).fetch_one(&mut **tx).await?;
        let prior_loss = wallet.plan_order_chargebacks_in(tx, order_id).await?;
        let retained_original = original_amount
            .saturating_sub(refunded_cash)
            .saturating_sub(prior_loss)
            .max(0);
        let mut receivable = 0;
        let mut payable = 0;
        if journal.state == JournalState::Void {
            payable = amount_minor.min(retained_original).min(
                crate::statement_payments::voiding::original_payable_in(
                    tx,
                    wallet,
                    journal.journal_id,
                )
                .await?,
            );
        } else {
            receivable = amount_minor
                .min(retained_original)
                .min(journal.settled_minor);
        }
        wallet
            .plan_journal_dispute_recharge_in(
                tx,
                &journal.journal_id,
                &dispute_id,
                amount_minor,
                receivable,
                payable,
                on,
            )
            .await?;
        if let Some(period) = crate::plan_recognition::period_in(tx, journal.journal_id).await? {
            let entry = *crate::entry_id_for(&format!("dispute-recharge/{dispute_id}")).as_uuid();
            crate::plan_recognition::effect_in(
                tx,
                &period,
                "dispute",
                dispute_id,
                on,
                on,
                Some(entry),
                &crate::plan_recognition::Effect {
                    collected: -receivable,
                    receivable,
                    payable: -payable,
                    cash: -amount_minor,
                    ..crate::plan_recognition::Effect::default()
                },
            )
            .await?;
        }
        if receivable > 0 {
            sqlx::query("UPDATE oxsum.invoice_journals SET state='posted',settled_minor=settled_minor-$2,paid_at=NULL,disputed_minor=disputed_minor+$2,updated_at=now() WHERE journal_id=$1")
                .bind(journal.journal_id).bind(receivable).execute(&mut **tx).await?;
            self.chargeback_statement_in(tx, &journal, dispute_id, receivable)
                .await?;
        }
        Ok(Some(journal.journal_id))
    }

    /// Applies one confirmed plan-refund source. Kept as a compatibility seam;
    /// caller-supplied identities and amount must equal the persisted refund.
    /// Before close the allocation is recorded without inventing a journal.
    ///
    /// # Errors
    /// Unconfirmed or mismatched sources refuse; storage failures propagate.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply_refund_to_journal(
        &self,
        wallet: &crate::Wallet,
        order_id: Uuid,
        refund_id: Uuid,
        credit_note_id: Uuid,
        amount_minor: i64,
        _reason: &str,
        on: time::Date,
    ) -> Result<Option<Uuid>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let matches:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.refunds r JOIN oxsum.credit_notes n ON n.refund_id=r.refund_id WHERE r.refund_id=$1 AND r.order_id=$2 AND n.credit_note_id=$3 AND r.amount_minor=$4)")
            .bind(refund_id).bind(order_id).bind(credit_note_id).bind(amount_minor).fetch_one(&mut *tx).await?;
        if !matches {
            return Err(invalid("refund source terms differ"));
        }
        let applied = self
            .apply_plan_refund_in(&mut tx, wallet, refund_id, None, false, true, on)
            .await?;
        tx.commit().await?;
        Ok(applied.journal_id)
    }

    /// A committed `recharge_journal_dispute_in` — the same single-purpose
    /// wrapper [`Db::apply_refund_to_journal`] is, for callers holding no
    /// transaction.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recharge_journal_dispute(
        &self,
        wallet: &crate::Wallet,
        order_id: Uuid,
        dispute_id: Uuid,
        amount_minor: i64,
        on: time::Date,
    ) -> Result<Option<Uuid>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let restored = self
            .recharge_journal_dispute_in(&mut tx, wallet, order_id, dispute_id, amount_minor, on)
            .await?;
        tx.commit().await?;
        Ok(restored)
    }
}

/// The row a `plan_periods` UPDATE closed, re-read for the journal — bounds,
/// quota and authorization as the period stood, whatever the close wrote.
pub(crate) async fn closing_period_in(
    tx: &mut Transaction<'_, Postgres>,
    customer_plan_id: Uuid,
) -> Result<Option<ClosingPeriod>, WalletError> {
    let row = sqlx::query(
        "SELECT plan_period_id, period_index, period_start, period_end, \
                authorization_kind, authorization_ref \
         FROM oxsum.plan_periods \
         WHERE customer_plan_id=$1 AND state <> 'active' \
         ORDER BY period_index DESC LIMIT 1",
    )
    .bind(customer_plan_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(ClosingPeriod {
        plan_period_id: row.try_get("plan_period_id")?,
        period_index: row.try_get("period_index")?,
        period_start: row.try_get("period_start")?,
        period_end: row.try_get("period_end")?,
        authorization_kind: row.try_get("authorization_kind")?,
        authorization_ref: row.try_get("authorization_ref")?,
    }))
}
