//! Plan statements and customer credit (issue #612, T21).
//!
//! `oxsum.statements` already carries the monthly *usage* statement — the
//! credit-line receivable for metered spend. This module is the *plan-fee*
//! billing document: one `plan_statements` row per `(customer_plan_id,
//! period_index)`, written inside the same close transaction
//! [`Db::journal_closing_period_in`] posts the `invoice_journals` row it
//! wraps. The journal is the accounting — its lines spell `base_fee`,
//! `usage_overage`, `promo_credit`; the statement restates the same bill in
//! customer vocabulary — `planFee`, `usage`, `discount`, `refundCredit`,
//! `disputeChargeback`, `paymentApplied` — and carries the payment terms the
//! receivable answers against (`due_at` = `issued_at + net_terms_days`).
//!
//! The lifecycle is `open → issued → paid | past_due | void`, with
//! `past_due → issued` the staff release-review's one backward step and `open`
//! never resting outside the close transaction — the same machine the
//! journal's trigger enforces. A payment reconciling the journal appends an
//! order-linked `paymentApplied` line,
//! reduces `total_minor − paid_minor` to zero, and walks `issued → paid`; a
//! partial payment leaves `issued` with `paid_minor > 0`.
//!
//! The `statement-past-due` job flips `issued` rows whose `due_at` passed to
//! `past_due`, and past `grace_days` suspends the plan's wallet-side
//! admission — subscription-quota draws (already paid for) still serve; a
//! payment clearing past-grace debt removes only that debt suspension. Wallet
//! admission also evaluates effective overdue debt before the worker stamps a
//! flag. Renewal still requires a paid period authorization. `credit_limit_minor`
//! caps wallet-side overage: `outstanding + this_overage > credit_limit_minor`
//! refuses, counting live statement debt, posted journals without live documents
//! and current holds.

mod application_reads;
pub(crate) use application_reads::{application_detail_in, application_page_in};

use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::business_staff as authority;
use crate::db::Db;
use crate::error::WalletError;
use crate::journals::{InvoiceJournal, InvoiceJournalLine, JournalLineKind};

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}

/// A statement's lifecycle — `open` exists only inside the close transaction,
/// so the stored states a read can answer are `issued`, `paid`, `past_due`
/// and `void`. Wire spelling is camelCase like every API enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanStatementState {
    /// Exists only inside the close transaction — never a resting state.
    Open,
    /// Issued, awaiting payment — the receivable is open.
    Issued,
    /// Fully reconciled — nothing more is owed.
    Paid,
    /// Past `due_at` — inside `grace_days` the plan still serves, past it the
    /// wallet-side admission suspends.
    PastDue,
    /// Cancelled by staff — reversing lines net it to zero.
    Void,
}
impl PlanStatementState {
    /// The spelling `plan_statements.state` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Issued => "issued",
            Self::Paid => "paid",
            Self::PastDue => "past_due",
            Self::Void => "void",
        }
    }
    /// Reads a stored state back; an unknown one refuses rather than guesses.
    /// Accepts both the stored snake_case and the wire camelCase (`pastDue`),
    /// so a query filter spelled the API's way parses the same as the row's.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "open" => Some(Self::Open),
            "issued" => Some(Self::Issued),
            "paid" => Some(Self::Paid),
            "past_due" | "pastDue" => Some(Self::PastDue),
            "void" => Some(Self::Void),
            _ => None,
        }
    }
}

/// What a statement line bills — the wire spelling is camelCase, the row
/// snake_case like the journal's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StatementLineKind {
    /// The sold version's price — the journal's `base_fee`.
    PlanFee,
    /// A retained historical `usage_overage` line; new closes never rebill
    /// wallet-settled usage.
    Usage,
    /// A credit against the bill — a promo or a grant write-off.
    Discount,
    /// A refund's credit note restated for the customer.
    RefundCredit,
    /// A lost dispute's restored receivable, appended onto the next open
    /// statement.
    DisputeChargeback,
    /// A payment reconciling the statement — reduces what is owed.
    PaymentApplied,
}
impl StatementLineKind {
    /// The spelling `plan_statement_lines.kind` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PlanFee => "plan_fee",
            Self::Usage => "usage",
            Self::Discount => "discount",
            Self::RefundCredit => "refund_credit",
            Self::DisputeChargeback => "dispute_chargeback",
            Self::PaymentApplied => "payment_applied",
        }
    }
    #[must_use]
    fn parse(text: &str) -> Option<Self> {
        match text {
            "plan_fee" => Some(Self::PlanFee),
            "usage" => Some(Self::Usage),
            "discount" => Some(Self::Discount),
            "refund_credit" => Some(Self::RefundCredit),
            "dispute_chargeback" => Some(Self::DisputeChargeback),
            "payment_applied" => Some(Self::PaymentApplied),
            _ => None,
        }
    }
}

/// One period's statement — the header the lists answer.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStatement {
    /// Local statement identifier.
    pub statement_id: Uuid,
    /// NULL on a wallet-only statement — a dispute chargeback an org carries
    /// when no live plan stands.
    pub customer_plan_id: Option<Uuid>,
    /// Customer organization the statement bills.
    pub organization_id: Uuid,
    /// Business that owns the statement, hidden from customer responses.
    #[serde(skip_serializing)]
    pub business_id: Uuid,
    /// Zero-based plan period this statement covers; `None` on a
    /// chargeback-only statement.
    pub period_index: Option<i32>,
    /// Inclusive start of the billed period, when plan-bound.
    #[serde(with = "time::serde::rfc3339::option")]
    pub period_start: Option<OffsetDateTime>,
    /// Exclusive end of the billed period, when plan-bound.
    #[serde(with = "time::serde::rfc3339::option")]
    pub period_end: Option<OffsetDateTime>,
    /// The invoice journal whose receivable this statement surfaces.
    pub journal_id: Option<Uuid>,
    /// Lifecycle state of the statement.
    pub state: PlanStatementState,
    /// Gross charge/credit total, excluding payment allocation lines.
    pub total_minor: i64,
    /// What reconciled payments already covered.
    pub paid_minor: i64,
    /// Three-letter ISO 4217 currency code of the amounts.
    pub currency: String,
    /// When the statement was issued.
    #[serde(with = "time::serde::rfc3339::option")]
    pub issued_at: Option<OffsetDateTime>,
    /// Payment due date, `issued_at + net_terms_days`.
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_at: Option<OffsetDateTime>,
    /// When the statement was fully paid.
    #[serde(with = "time::serde::rfc3339::option")]
    pub paid_at: Option<OffsetDateTime>,
    /// When the statement first became past due.
    #[serde(with = "time::serde::rfc3339::option")]
    pub past_due_at: Option<OffsetDateTime>,
    /// When the statement was voided.
    #[serde(with = "time::serde::rfc3339::option")]
    pub voided_at: Option<OffsetDateTime>,
    /// Staff's reason for voiding the statement.
    pub void_reason: Option<String>,
    /// Statement this one replaces, when reissued.
    pub replaces_statement_id: Option<Uuid>,
    /// The order whose payment completed the settlement — the dispute link.
    pub paid_order_id: Option<Uuid>,
    /// When the statement row was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One line of a statement — signed `amount_minor`; charges positive,
/// credits negative.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStatementLine {
    /// Local line identifier.
    pub line_id: Uuid,
    /// Statement the line belongs to.
    pub statement_id: Uuid,
    /// What the line bills or credits.
    pub kind: StatementLineKind,
    /// Signed: charges positive, credits negative.
    pub amount_minor: i64,
    /// Customer-facing description of the line.
    pub description: String,
    /// The fact the line came from — a journal line, a refund, an order, a
    /// dispute.
    pub source_ref: Option<Uuid>,
    /// When the line was written.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The detail read: a statement plus its lines.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStatementDetail {
    /// The statement header.
    #[serde(flatten)]
    pub statement: PlanStatement,
    /// The statement's lines in write order.
    pub lines: Vec<PlanStatementLine>,
    /// Immediate committed correction successor, independent of pagination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor_statement_id: Option<Uuid>,
}

/// A cursor-paginated page of plan statements.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStatementPage {
    /// Statements in newest-first order.
    pub statements: Vec<PlanStatement>,
    /// Statement identifier to pass as `after` for the next page; `None` at
    /// the end.
    pub next_cursor: Option<Uuid>,
}

const STATEMENT_COLS: &str = "statement_id,customer_plan_id,organization_id,business_id,\
     period_index,period_start,period_end,journal_id,state,total_minor,paid_minor,currency,\
     issued_at,due_at,paid_at,past_due_at,voided_at,void_reason,replaces_statement_id,\
     paid_order_id,created_at,updated_at";

const LINE_COLS: &str = "line_id,statement_id,kind,amount_minor,description,source_ref,created_at";

fn statement_from_row(row: &PgRow) -> Result<PlanStatement, WalletError> {
    Ok(PlanStatement {
        statement_id: row.try_get("statement_id")?,
        customer_plan_id: row.try_get("customer_plan_id")?,
        organization_id: row.try_get("organization_id")?,
        business_id: row.try_get("business_id")?,
        period_index: row.try_get("period_index")?,
        period_start: row.try_get("period_start")?,
        period_end: row.try_get("period_end")?,
        journal_id: row.try_get("journal_id")?,
        state: PlanStatementState::parse(&row.try_get::<String, _>("state")?)
            .ok_or_else(|| invalid("unknown statement state"))?,
        total_minor: row.try_get("total_minor")?,
        paid_minor: row.try_get("paid_minor")?,
        currency: row.try_get("currency")?,
        issued_at: row.try_get("issued_at")?,
        due_at: row.try_get("due_at")?,
        paid_at: row.try_get("paid_at")?,
        past_due_at: row.try_get("past_due_at")?,
        voided_at: row.try_get("voided_at")?,
        void_reason: row.try_get("void_reason")?,
        replaces_statement_id: row.try_get("replaces_statement_id")?,
        paid_order_id: row.try_get("paid_order_id")?,
        created_at: row.try_get("created_at")?,
    })
}

fn line_from_row(row: &PgRow) -> Result<PlanStatementLine, WalletError> {
    Ok(PlanStatementLine {
        line_id: row.try_get("line_id")?,
        statement_id: row.try_get("statement_id")?,
        kind: StatementLineKind::parse(&row.try_get::<String, _>("kind")?)
            .ok_or_else(|| invalid("unknown statement line kind"))?,
        amount_minor: row.try_get("amount_minor")?,
        description: row.try_get("description")?,
        source_ref: row.try_get("source_ref")?,
        created_at: row.try_get("created_at")?,
    })
}

/// The journal's `kind` in the statement's customer vocabulary.
pub(crate) fn statement_kind(line: &InvoiceJournalLine) -> StatementLineKind {
    match line.kind {
        JournalLineKind::BaseFee | JournalLineKind::Tax => StatementLineKind::PlanFee,
        JournalLineKind::UsageOverage => StatementLineKind::Usage,
        // A `discount` line is a refund's credit note when its `source_ref`
        // names a `credit_notes` row, else a grant's write-off — the
        // statement spells the customer-facing difference.
        JournalLineKind::Discount => StatementLineKind::Discount,
        JournalLineKind::PromoCredit => StatementLineKind::Discount,
    }
}

impl Db {
    /// The statement step of the journal close: writes the `open` statement
    /// the period's just-posted journal wraps, its lines restating the
    /// journal's in customer vocabulary, then walks `open → issued` in the
    /// same transaction — the bill and the customer-visible document are one
    /// commit. `due_at` = `issued_at + net_terms_days` off the plan's terms.
    ///
    /// Idempotent by construction: the `(customer_plan_id, period_index)`
    /// partial unique makes a replayed close a no-op — the caller treats
    /// `None` as "already issued".
    ///
    /// Runs inside the caller's transaction beside the journal row.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub(crate) async fn issue_statement_for_journal_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        journal: &InvoiceJournal,
        period_start: Option<OffsetDateTime>,
        period_end: Option<OffsetDateTime>,
    ) -> Result<Option<Uuid>, WalletError> {
        // Already issued? The partial unique makes the close idempotent.
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT statement_id FROM oxsum.plan_statements \
             WHERE customer_plan_id=$1 AND period_index=$2 AND state <> 'void'",
        )
        .bind(journal.customer_plan_id)
        .bind(journal.period_index)
        .fetch_optional(&mut **tx)
        .await?;
        if existing.is_some() {
            return Ok(None);
        }
        let net_terms: i32 = sqlx::query_scalar(
            "SELECT net_terms_days FROM oxsum.customer_plans WHERE customer_plan_id=$1",
        )
        .bind(journal.customer_plan_id)
        .fetch_one(&mut **tx)
        .await?;
        let lines = crate::journals::lines_for_statement_in(tx, journal.journal_id).await?;
        let total: i64 = lines.iter().map(|l| l.amount_minor).sum();
        // The close applies the journal's settled amount through
        // reconcile_statement_for_journal_in after issuance. Start at zero so
        // that one path records the source-linked payment line, paid amount
        // and state transition together, without counting prepayment twice.
        let statement_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.plan_statements \
                 (statement_id,customer_plan_id,organization_id,business_id,period_index,\
                  period_start,period_end,journal_id,state,total_minor,currency) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'open',$9,$10)",
        )
        .bind(statement_id)
        .bind(journal.customer_plan_id)
        .bind(journal.organization_id)
        .bind(journal.business_id)
        .bind(journal.period_index)
        .bind(period_start)
        .bind(period_end)
        .bind(journal.journal_id)
        .bind(total)
        .bind(&journal.currency)
        .execute(&mut **tx)
        .await?;
        for line in &lines {
            insert_line_in(
                tx,
                statement_id,
                statement_kind(line),
                line.amount_minor,
                &line.description,
                line.source_ref.or(Some(line.line_id)),
            )
            .await?;
        }
        sqlx::query(
            "UPDATE oxsum.plan_statements \
             SET state='issued', issued_at=now(), \
                 due_at = now() + ($2 || ' days')::interval, updated_at=now() \
             WHERE statement_id=$1",
        )
        .bind(statement_id)
        .bind(net_terms.to_string())
        .execute(&mut **tx)
        .await?;
        if total <= 0 {
            sqlx::query("UPDATE oxsum.plan_statements SET state='paid',paid_at=now(),updated_at=now() WHERE statement_id=$1")
                .bind(statement_id).execute(&mut **tx).await?;
        }
        Ok(Some(statement_id))
    }

    /// Reconcile a journal allocation to its customer statement. Every partial
    /// or full allocation appends an order-linked `paymentApplied` line
    /// reducing `total_minor − paid_minor` to zero and walks `issued|past_due
    /// → paid`; a partial one leaves `issued` with `paid_minor > 0`. A
    /// statement that just cleared recomputes dunning from all past-grace debt
    /// in the same transaction. Renewal and manual suspension remain separate.
    ///
    /// Runs inside the caller's transaction beside the journal update. The
    /// caller must already hold the customer plan before the journal/statement;
    /// clearing dunning below must not introduce a statement-to-plan lock edge.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub(crate) async fn reconcile_statement_for_journal_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        journal: &InvoiceJournal,
        order_id: Uuid,
        paid_minor: i64,
    ) -> Result<(), WalletError> {
        let Some(statement) = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements \
             WHERE journal_id=$1 AND state <> 'void' LIMIT 1 FOR UPDATE"
        ))
        .bind(journal.journal_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|r| statement_from_row(&r))
        .transpose()?
        else {
            return Ok(());
        };
        if paid_minor <= 0 {
            return Ok(());
        }
        let applied = paid_minor.min((statement.total_minor - statement.paid_minor).max(0));
        if applied == 0 {
            return Ok(());
        }
        let new_paid = statement.paid_minor + applied;
        let fully = new_paid >= statement.total_minor;
        // Partial prepayment also has a source-linked line. The statement's
        // gross total stays the charge total, while paid_minor and these lines
        // show the allocations that reduced its outstanding receivable.
        insert_line_in(
            tx,
            statement.statement_id,
            StatementLineKind::PaymentApplied,
            -applied,
            "payment reconciled the statement",
            Some(order_id),
        )
        .await?;
        if fully {
            sqlx::query(
                "UPDATE oxsum.plan_statements \
                 SET state='paid', paid_minor=total_minor, paid_at=now(), \
                     paid_order_id=$2, updated_at=now() \
                 WHERE statement_id=$1",
            )
            .bind(statement.statement_id)
            .bind(order_id)
            .execute(&mut **tx)
            .await?;
            // Other past-grace debt, including a retained document-only void,
            // keeps its restriction. Payment does not authorize another period.
            crate::plan_debt::refresh_in(tx, journal.customer_plan_id).await?;
        } else {
            sqlx::query(
                "UPDATE oxsum.plan_statements \
                 SET paid_minor=$2, updated_at=now() WHERE statement_id=$1",
            )
            .bind(statement.statement_id)
            .bind(new_paid)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// Restate a cash refund on the original period statement. Gross charges
    /// and applied payments fall together; unrelated outstanding debt remains.
    pub(crate) async fn refund_statement_for_journal_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        journal: &InvoiceJournal,
        note_id: Uuid,
        credit: i64,
        reason: &str,
    ) -> Result<(), WalletError> {
        let row=sqlx::query(&format!("SELECT {STATEMENT_COLS} FROM oxsum.plan_statements WHERE journal_id=$1 AND period_index=$2 AND state<>'void' ORDER BY created_at LIMIT 1 FOR UPDATE"))
            .bind(journal.journal_id).bind(journal.period_index).fetch_optional(&mut **tx).await?;
        let Some(row) = row else {
            return Err(WalletError::Conflict(
                "refund original statement is missing; finance review required".into(),
            ));
        };
        let statement = statement_from_row(&row)?;
        let old_credit:i64=sqlx::query_scalar("SELECT -COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.plan_statement_lines WHERE statement_id=$1 AND source_ref=$2 AND kind IN ('discount','refund_credit')")
            .bind(statement.statement_id).bind(note_id).fetch_one(&mut **tx).await?;
        let old_reversal:i64=sqlx::query_scalar("SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.plan_statement_lines WHERE statement_id=$1 AND source_ref=$2 AND kind='payment_applied'")
            .bind(statement.statement_id).bind(note_id).fetch_one(&mut **tx).await?;
        let reduction = credit - old_credit;
        let reversal = credit - old_reversal;
        let total = statement
            .total_minor
            .checked_sub(reduction)
            .ok_or_else(|| invalid("statement refund total overflows"))?;
        let paid = statement
            .paid_minor
            .checked_sub(reversal)
            .ok_or_else(|| invalid("statement refund payment overflows"))?;
        if total < 0 || paid < 0 || paid > total {
            return Err(WalletError::Conflict(
                "refund original statement allocations need finance review".into(),
            ));
        }
        if reduction != 0 {
            insert_line_in(
                tx,
                statement.statement_id,
                StatementLineKind::RefundCredit,
                -reduction,
                &format!("confirmed cash refund: {reason}"),
                Some(note_id),
            )
            .await?;
        }
        if reversal != 0 {
            insert_line_in(
                tx,
                statement.statement_id,
                StatementLineKind::PaymentApplied,
                reversal,
                "original payment returned in cash",
                Some(note_id),
            )
            .await?;
        }
        sqlx::query("UPDATE oxsum.plan_statements SET total_minor=$2,paid_minor=$3,updated_at=now() WHERE statement_id=$1")
            .bind(statement.statement_id).bind(total).bind(paid).execute(&mut **tx).await?;
        Ok(())
    }

    /// Validate a one-to-one journal projection before any financial reversal.
    /// A retained legacy document void has zeroed counters but remains linked;
    /// its historical charge/payment lines must still reconstruct the journal.
    pub(crate) async fn validate_journal_void_statements_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        journal: &InvoiceJournal,
    ) -> Result<(), WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements \
             WHERE journal_id=$1 ORDER BY created_at,statement_id FOR UPDATE"
        ))
        .bind(journal.journal_id)
        .fetch_all(&mut **tx)
        .await?;
        if rows.len() > 1 {
            return Err(WalletError::Conflict(
                "multiple journal statements require explicit financial reconciliation".into(),
            ));
        }
        if let Some(row) = rows.first() {
            let statement = statement_from_row(row)?;
            if statement.customer_plan_id != Some(journal.customer_plan_id)
                || statement.organization_id != journal.organization_id
                || statement.business_id != journal.business_id
                || statement.period_index != Some(journal.period_index)
                || statement.state == PlanStatementState::Open
            {
                return Err(WalletError::Conflict("statement does not identify the original journal period; financial reconciliation required".into()));
            }
            let lines = lines_in(tx, statement.statement_id).await?;
            if lines
                .iter()
                .any(|line| line.kind == StatementLineKind::DisputeChargeback)
            {
                return Err(WalletError::Conflict(
                    "mixed chargeback statement requires source-specific financial reconciliation"
                        .into(),
                ));
            }
            let expected = crate::journals::lines_for_statement_in(tx, journal.journal_id).await?;
            // Compare each immutable source and signed amount. Refund credit is
            // the customer vocabulary for a journal discount; promo/tax are
            // likewise grouped only by their documented statement kind.
            let category = |kind: StatementLineKind| match kind {
                StatementLineKind::RefundCredit => StatementLineKind::Discount,
                other => other,
            };
            let mut charges = std::collections::BTreeMap::new();
            let mut actual = std::collections::BTreeMap::new();
            for line in expected {
                let key = (
                    category(statement_kind(&line)).as_str(),
                    line.source_ref.or(Some(line.line_id)),
                );
                let value = charges.entry(key).or_insert(0i64);
                *value = value
                    .checked_add(line.amount_minor)
                    .ok_or_else(|| invalid("journal amount overflows"))?;
            }
            let mut paid = 0i64;
            let mut reversed = std::collections::BTreeSet::new();
            for line in &lines {
                if statement.state == PlanStatementState::Void
                    && let Some(source) = line
                        .source_ref
                        .and_then(|id| lines.iter().find(|source| source.line_id == id))
                {
                    // Retained document-only voids must contain exactly one
                    // opposite line per original; a source link alone is not
                    // proof that this line belongs to the old reversal.
                    if source.kind != line.kind
                        || source.amount_minor.checked_neg() != Some(line.amount_minor)
                        || source
                            .source_ref
                            .is_some_and(|id| lines.iter().any(|candidate| candidate.line_id == id))
                        || !reversed.insert(source.line_id)
                    {
                        return Err(WalletError::Conflict("legacy void reversal differs from its source; financial reconciliation required".into()));
                    }
                    continue;
                }
                if line.kind == StatementLineKind::PaymentApplied {
                    paid = paid
                        .checked_sub(line.amount_minor)
                        .ok_or_else(|| invalid("statement amount overflows"))?;
                } else {
                    let value = actual
                        .entry((category(line.kind).as_str(), line.source_ref))
                        .or_insert(0i64);
                    *value = value
                        .checked_add(line.amount_minor)
                        .ok_or_else(|| invalid("statement amount overflows"))?;
                }
            }
            if (statement.state == PlanStatementState::Void && reversed.len() * 2 != lines.len())
                || actual != charges
                || paid != journal.settled_minor
                || (statement.state != PlanStatementState::Void
                    && (statement.total_minor != journal.total_minor
                        || statement.paid_minor != journal.settled_minor))
            {
                return Err(WalletError::Conflict("statement amounts or sources differ from its journal; financial reconciliation required".into()));
            }
        }
        Ok(())
    }

    /// The statement half of `void_invoice_journal`: a journal the staff voids
    /// is no longer a bill the customer owes or paid — the document that wraps
    /// it follows in the same transaction so the customer-facing statement can
    /// never outlive its receivable as still-owed or paid-in-full. Reversing
    /// lines append so the statement nets to zero and the row walks to `void`,
    /// mirroring the release-review's own `void` arm. A statement already void,
    /// or none wrapped at all, is a no-op.
    ///
    /// Runs inside the caller's transaction beside the journal reversal.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub(crate) async fn void_statement_for_journal_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        journal_id: Uuid,
        reason: &str,
    ) -> Result<(), WalletError> {
        let statement = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements \
             WHERE journal_id=$1 AND state <> 'void' LIMIT 1 FOR UPDATE"
        ))
        .bind(journal_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|r| statement_from_row(&r))
        .transpose()?;
        let Some(statement) = statement else {
            return Ok(());
        };
        self.void_plan_document_in(tx, statement.statement_id, reason)
            .await
    }

    /// Reverse one proven source-exclusive document beside its financial void.
    pub(crate) async fn void_plan_document_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        statement_id: Uuid,
        reason: &str,
    ) -> Result<(), WalletError> {
        let Some(statement) = statement_in(tx, statement_id).await? else {
            return Ok(());
        };
        if statement.state == PlanStatementState::Void {
            return Ok(());
        }
        // Reversing lines append so the statement nets to zero — the void's
        // record is the second set of lines, history rewritten nowhere.
        let lines = lines_in(&mut *tx, statement.statement_id).await?;
        for line in &lines {
            insert_line_in(
                tx,
                statement.statement_id,
                line.kind,
                -line.amount_minor,
                &format!("void reversal of {}", line.description),
                Some(line.line_id),
            )
            .await?;
        }
        sqlx::query(
            "UPDATE oxsum.plan_statements \
             SET state='void', total_minor=0, paid_minor=0, voided_at=now(), \
                 void_reason=$2, updated_at=now() \
             WHERE statement_id=$1",
        )
        .bind(statement.statement_id)
        .bind(reason)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// A lost dispute on a `planPeriod` order restores the receivable on the
    /// journal — `paid → posted`, `disputed_minor` rising (T20's
    /// `recharge_journal_dispute_in`). The statement side surfaces the same
    /// restored debt in customer terms: a `disputeChargeback` line appends
    /// onto the *next* open statement the customer still owes against, or a
    /// fresh `open → issued` statement when none stands, so the restored
    /// receivable is never invisible.
    ///
    /// Runs inside the caller's transaction beside the journal recharge.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub(crate) async fn chargeback_statement_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        journal: &InvoiceJournal,
        dispute_id: Uuid,
        amount_minor: i64,
    ) -> Result<(), WalletError> {
        if amount_minor <= 0 {
            return Ok(());
        }
        // The next open statement this customer plan still owes against —
        // earliest due first, so the restored debt lands on the document the
        // customer reads first. The contested journal's own statement is
        // already `paid` whenever the dispute can recharge it — a lost dispute
        // only re-opens a settled bill — so the earliest open document is the
        // correct surface.
        let open = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements \
             WHERE customer_plan_id=$1 AND state IN ('issued','past_due','open') \
             ORDER BY COALESCE(due_at, created_at) LIMIT 1 FOR UPDATE"
        ))
        .bind(journal.customer_plan_id)
        .fetch_optional(&mut **tx)
        .await?;
        let statement = if let Some(row) = open {
            statement_from_row(&row)?
        } else {
            // No open statement: mint one that carries only the chargeback.
            // `period_index` stays NULL — the period's slot is already taken by
            // the settled statement this dispute reopened, and the
            // `(customer_plan_id, period_index)` partial unique would refuse a
            // second row. The `journal_id` link still lets payment
            // reconciliation find it.
            let net_terms: i32 = sqlx::query_scalar(
                "SELECT COALESCE((SELECT net_terms_days FROM oxsum.customer_plans \
                  WHERE customer_plan_id=$1), 14)",
            )
            .bind(journal.customer_plan_id)
            .fetch_one(&mut **tx)
            .await?;
            let statement_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO oxsum.plan_statements \
                     (statement_id,customer_plan_id,organization_id,business_id,journal_id,\
                      state,total_minor,currency,period_index) \
                 VALUES ($1,$2,$3,$4,$5,'open',0,$6,NULL)",
            )
            .bind(statement_id)
            .bind(journal.customer_plan_id)
            .bind(journal.organization_id)
            .bind(journal.business_id)
            .bind(journal.journal_id)
            .bind(&journal.currency)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "UPDATE oxsum.plan_statements \
                 SET state='issued', issued_at=now(), \
                     due_at = now() + ($2 || ' days')::interval, updated_at=now() \
                 WHERE statement_id=$1",
            )
            .bind(statement_id)
            .bind(net_terms.to_string())
            .execute(&mut **tx)
            .await?;
            let row = sqlx::query(&format!(
                "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements \
                 WHERE statement_id=$1 FOR UPDATE"
            ))
            .bind(statement_id)
            .fetch_one(&mut **tx)
            .await?;
            statement_from_row(&row)?
        };
        insert_line_in(
            tx,
            statement.statement_id,
            StatementLineKind::DisputeChargeback,
            amount_minor,
            &format!("dispute chargeback {dispute_id}"),
            Some(dispute_id),
        )
        .await?;
        sqlx::query(
            "UPDATE oxsum.plan_statements \
             SET total_minor=total_minor+$2, updated_at=now() WHERE statement_id=$1",
        )
        .bind(statement.statement_id)
        .bind(amount_minor)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// The customer's own statements — the billing read, scoped to the
    /// caller's organization like every customer read.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a bad limit; storage failures surface
    /// as [`WalletError`].
    pub async fn plan_statements(
        &self,
        organization_id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<PlanStatementPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let rows = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements \
             WHERE organization_id=$1 AND state <> 'open' \
               AND ($2::uuid IS NULL OR (created_at, statement_id) < \
                    (SELECT created_at, statement_id FROM oxsum.plan_statements WHERE statement_id=$2)) \
             ORDER BY created_at DESC, statement_id DESC LIMIT $3"
        ))
        .bind(organization_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(self.pool())
        .await?;
        let mut statements = rows
            .iter()
            .map(statement_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let more = statements.len() > limit as usize;
        statements.truncate(limit as usize);
        let next_cursor = if more {
            statements.last().map(|s| s.statement_id)
        } else {
            None
        };
        Ok(PlanStatementPage {
            statements,
            next_cursor,
        })
    }

    /// One statement plus its lines, scoped to the owning organization — a
    /// statement of another organization is a not-found.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn plan_statement(
        &self,
        organization_id: Uuid,
        statement_id: Uuid,
    ) -> Result<Option<PlanStatementDetail>, WalletError> {
        let Some(statement) = self.statement_row(statement_id).await? else {
            return Ok(None);
        };
        if statement.organization_id != organization_id
            || statement.state == PlanStatementState::Open
        {
            return Ok(None);
        }
        Ok(Some(PlanStatementDetail {
            successor_statement_id: sqlx::query_scalar("SELECT statement_id FROM oxsum.invoice_corrections WHERE predecessor_statement_id=$1 AND organization_id=$2")
                .bind(statement_id).bind(organization_id).fetch_optional(self.pool()).await?,
            lines: self.plan_statement_lines(statement_id).await?,
            statement,
        }))
    }

    /// The staff cross-organization statement list: `state` narrows to one
    /// lifecycle state (`pastDue` is the dunning work list), `organizationId`
    /// scopes to one customer. `ReportsRead` is required in the transaction.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a bad state or limit; storage
    /// failures surface as [`WalletError`].
    pub async fn business_plan_statements(
        &self,
        token: &str,
        state: Option<&str>,
        organization_id: Option<Uuid>,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<PlanStatementPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        // The filter spells the wire's camelCase; the query binds the stored
        // snake_case the row carries, so `pastDue` reads `past_due` rows.
        let state = state
            .map(|s| {
                PlanStatementState::parse(s)
                    .map(|st| st.as_str())
                    .ok_or_else(|| invalid("invalid statement state filter"))
            })
            .transpose()?;
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
            "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements \
             WHERE business_id=$1 AND state <> 'open' \
               AND ($3::text IS NULL OR state=$3) \
               AND ($4::uuid IS NULL OR organization_id=$4) \
               AND ($2::uuid IS NULL OR (created_at, statement_id) < \
                    (SELECT created_at, statement_id FROM oxsum.plan_statements WHERE statement_id=$2)) \
             ORDER BY created_at DESC, statement_id DESC LIMIT $5"
        ))
        .bind(business)
        .bind(after)
        .bind(state)
        .bind(organization_id)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut statements = rows
            .iter()
            .map(statement_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let more = statements.len() > limit as usize;
        statements.truncate(limit as usize);
        let next_cursor = if more {
            statements.last().map(|s| s.statement_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(PlanStatementPage {
            statements,
            next_cursor,
        })
    }

    /// One statement the business owns — the staff read behind
    /// `release-review`.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn business_plan_statement(
        &self,
        token: &str,
        statement_id: Uuid,
    ) -> Result<Option<PlanStatementDetail>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(
            &mut tx,
            business,
            token,
            crate::BusinessCapability::ReportsRead,
        )
        .await?;
        let Some(statement) = statement_in(&mut tx, statement_id).await? else {
            tx.commit().await?;
            return Ok(None);
        };
        if statement.business_id != business || statement.state == PlanStatementState::Open {
            tx.commit().await?;
            return Ok(None);
        }
        let lines = lines_in(&mut tx, statement_id).await?;
        let successor_statement_id = successor_in(&mut tx, statement_id).await?;
        tx.commit().await?;
        Ok(Some(PlanStatementDetail {
            statement,
            lines,
            successor_statement_id,
        }))
    }

    /// The staff release-review: `extend_grace` walks `past_due → issued`
    /// (restores the issued standing, extends the grace window), `void`
    /// reverses the proven backing journal and statement together, retaining
    /// cash and recognizing collected funds as refund payable. Other debt
    /// past grace retains dunning; renewal and manual restrictions are separate.
    /// Every
    /// release writes a `finance_audit` row naming the staff actor, the
    /// reason and the statement — the same audit family `journal.void` uses.
    /// A replayed `idempotency_key` answers the standing statement.
    ///
    /// `FundsWrite` is required in the transaction.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unknown or open statement;
    /// [`WalletError::Conflict`] for a replayed key or a state the action
    /// cannot leave; [`WalletError::InvalidInput`] for a bad action; storage
    /// failures surface as [`WalletError`].
    pub async fn release_statement_review(
        &self,
        token: &str,
        statement_id: Uuid,
        action: &str,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<PlanStatementDetail, WalletError> {
        authority::validate(idempotency_key, Some(reason))?;
        let action = match action {
            "extendGrace" | "extend_grace" => "extend_grace",
            "void" => "void",
            other => return Err(invalid(&format!("unknown release action {other:?}"))),
        };
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            crate::BusinessCapability::FundsWrite,
        )
        .await?;
        if let Some(prior)=sqlx::query("SELECT target,detail FROM oxsum.finance_audit WHERE business_id=$1 AND action='statement.release' AND idempotency_key=$2")
            .bind(business).bind(idempotency_key).fetch_optional(&mut *tx).await? {
            let detail:serde_json::Value=prior.try_get("detail")?;
            if prior.try_get::<String,_>("target")?!=statement_id.to_string() || detail["releaseAction"]!=action || detail["reason"]!=reason {
                return Err(WalletError::Conflict("statement release key names different request terms".into()));
            }
            let statement=statement_in(&mut tx,statement_id).await?.ok_or_else(||invalid("audited statement disappeared"))?;
            let lines=lines_in(&mut tx,statement_id).await?;
            let successor_statement_id=successor_in(&mut tx,statement_id).await?;
            tx.commit().await?;
            return Ok(PlanStatementDetail{statement,lines,successor_statement_id});
        }
        // Resolve immutable identity, then take owner, journal, ledger and
        // statement in that order. Extend-grace needs only owner/statement.
        let coordinates=sqlx::query("SELECT s.customer_plan_id,s.organization_id,s.journal_id,o.tenant_id FROM oxsum.plan_statements s JOIN oxsum.organizations o USING(organization_id) WHERE s.statement_id=$1 AND s.business_id=$2 AND s.state<>'open'")
            .bind(statement_id).bind(business).fetch_optional(&mut *tx).await?
            .ok_or_else(||WalletError::NotFound("unknown statement".into()))?;
        let plan: Option<Uuid> = coordinates.try_get("customer_plan_id")?;
        if action == "void" {
            crate::statement_payments::lock_owner_in(
                &mut tx,
                coordinates.try_get("organization_id")?,
                self.pool(),
            )
            .await?;
        } else if let Some(plan) = plan {
            sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE")
                .bind(plan).fetch_one(&mut *tx).await?;
        }
        let mut wallet = None;
        let mut journal = None;
        if action == "void" {
            let journal_id: Option<Uuid> = coordinates.try_get("journal_id")?;
            let journal_id = journal_id.ok_or_else(|| {
                WalletError::Conflict(
                    "statement has no backing journal; explicit financial reconciliation required"
                        .into(),
                )
            })?;
            journal = crate::journals::journal_in(&mut tx, journal_id).await?;
            let source = journal
                .as_ref()
                .ok_or_else(|| WalletError::Conflict("backing journal is missing".into()))?;
            if Some(source.customer_plan_id) != plan || source.business_id != business {
                return Err(WalletError::Conflict(
                    "statement and journal ownership differ".into(),
                ));
            }
            let tenant: String = coordinates.try_get("tenant_id")?;
            let source_wallet = crate::Wallet::open_existing(self.pool().clone(), &tenant).await?;
            source_wallet.lock_append_in(&mut tx).await?;
            wallet = Some(source_wallet);
        }
        let Some(statement) = statement_in(&mut tx, statement_id).await? else {
            return Err(WalletError::NotFound("unknown statement".into()));
        };
        if statement.business_id != business || statement.state == PlanStatementState::Open {
            return Err(WalletError::NotFound("unknown statement".into()));
        }
        let previous_state = statement.state.as_str();
        let mut reversal_entry = None;
        match action {
            "extend_grace" => {
                // Restore the issued standing — the grace window re-opens
                // from `now`: `due_at` moves out by the plan's net terms.
                if statement.state != PlanStatementState::PastDue {
                    return Err(WalletError::Conflict(format!(
                        "a {previous_state} statement cannot extend grace"
                    )));
                }
                let net_terms: i32 = sqlx::query_scalar(
                    "SELECT COALESCE((SELECT net_terms_days FROM oxsum.customer_plans \
                      WHERE customer_plan_id=$1), 14)",
                )
                .bind(statement.customer_plan_id)
                .fetch_one(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE oxsum.plan_statements \
                     SET state='issued', past_due_at=NULL, \
                         due_at = now() + ($2 || ' days')::interval, updated_at=now() \
                     WHERE statement_id=$1",
                )
                .bind(statement_id)
                .bind(net_terms.to_string())
                .execute(&mut *tx)
                .await?;
                if let Some(plan) = plan {
                    crate::plan_debt::refresh_in(&mut tx, plan).await?;
                }
            }
            _ => {
                let source = journal
                    .as_ref()
                    .ok_or_else(|| invalid("validated journal missing"))?;
                if source.state == crate::JournalState::Void
                    && statement.state != PlanStatementState::Void
                {
                    return Err(WalletError::Conflict("void journal has a live statement; explicit financial reconciliation required".into()));
                }
                crate::financial_reconciliation::debt_sources::require_exclusive_document_in(
                    &mut tx,
                    statement.organization_id,
                    wallet
                        .as_ref()
                        .ok_or_else(|| invalid("validated wallet missing"))?
                        .tenant_id(),
                    statement_id,
                    source.journal_id,
                )
                .await?;
                reversal_entry = self
                    .void_journal_financial_in(
                        &mut tx,
                        wallet
                            .as_ref()
                            .ok_or_else(|| invalid("validated wallet missing"))?,
                        source,
                        reason,
                        OffsetDateTime::now_utc().date(),
                    )
                    .await?;
            }
        }
        let audit_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.finance_audit\
             (audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) \
             VALUES($1,$2,$3,'statement.release',$4,$5,$6)",
        )
        .bind(audit_id)
        .bind(business)
        .bind(actor.user_id)
        .bind(statement_id.to_string())
        .bind(serde_json::json!({
            "organizationId": statement.organization_id,
            "periodIndex": statement.period_index,
            "releaseAction": action,
            "previousState": previous_state,
            "totalMinor": statement.total_minor,
            "reason": reason,
            "journalId": journal.as_ref().map(|source|source.journal_id),
            "reversalEntryId": reversal_entry,
        }))
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(db) = &e
                && db.constraint() == Some("finance_audit_business_id_action_idempotency_key_key")
            {
                return WalletError::Conflict(
                    "statement release idempotency key already used".into(),
                );
            }
            WalletError::Storage(e.into())
        })?;
        let successor_statement_id = successor_in(&mut tx, statement_id).await?;
        let updated = statement_in(&mut tx, statement_id)
            .await?
            .ok_or_else(|| invalid("the statement did not land"))?;
        let lines = lines_in(&mut tx, statement_id).await?;
        tx.commit().await?;
        Ok(PlanStatementDetail {
            statement: updated,
            lines,
            successor_statement_id,
        })
    }

    /// The credit figure `/org/plan` reports: `creditLimitMinor` the binding's
    /// cap, `outstandingMinor` the remaining statement/journal debt, and
    /// `nextDueAt` its earliest due date. Admission separately includes holds.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn plan_credit_standing(
        &self,
        organization_id: Uuid,
    ) -> Result<Option<PlanCreditStanding>, WalletError> {
        let mut connection = self.pool().acquire().await?;
        let row = sqlx::query(
            "SELECT customer_plan_id,credit_limit_minor FROM oxsum.customer_plans \
             WHERE organization_id=$1 \
               AND state IN ('pending_payment','active','renewal_pending','past_due')",
        )
        .bind(organization_id)
        .fetch_optional(&mut *connection)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let (outstanding_minor, next_due_at) =
            crate::plan_debt::standing_in(&mut connection, row.try_get("customer_plan_id")?)
                .await?;
        Ok(Some(PlanCreditStanding {
            credit_limit_minor: row.try_get("credit_limit_minor")?,
            outstanding_minor,
            next_due_at,
        }))
    }
}

/// The credit standing `/org/plan` surfaces beside the binding.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCreditStanding {
    /// The binding's wallet-side overage cap; `None` is unlimited.
    pub credit_limit_minor: Option<i64>,
    /// Remaining statement debt and posted journals without live statements.
    /// The admission check additionally counts open holds.
    pub outstanding_minor: i64,
    /// The earliest effective debt due date, including retained voids.
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_due_at: Option<OffsetDateTime>,
}

/// The statement row inside the caller's transaction — `FOR UPDATE` so the
/// reconcile, the sweep and the release serialize on it.
pub(crate) async fn statement_in(
    tx: &mut Transaction<'_, Postgres>,
    statement_id: Uuid,
) -> Result<Option<PlanStatement>, WalletError> {
    let row = sqlx::query(&format!(
        "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements WHERE statement_id=$1 FOR UPDATE"
    ))
    .bind(statement_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(statement_from_row).transpose()
}

/// Read one customer statement and its lines within the caller's snapshot.
pub(crate) async fn statement_snapshot_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    statement: Uuid,
) -> Result<Option<PlanStatementDetail>, WalletError> {
    let row=sqlx::query(&format!("SELECT {STATEMENT_COLS} FROM oxsum.plan_statements WHERE statement_id=$1 AND organization_id=$2 AND state<>'open'"))
        .bind(statement).bind(organization).fetch_optional(&mut **tx).await?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some(PlanStatementDetail {
        statement: statement_from_row(&row)?,
        lines: lines_in(tx, statement).await?,
        successor_statement_id: successor_in(tx, statement).await?,
    }))
}

async fn lines_in(
    tx: &mut Transaction<'_, Postgres>,
    statement_id: Uuid,
) -> Result<Vec<PlanStatementLine>, WalletError> {
    let rows = sqlx::query(&format!(
        "SELECT {LINE_COLS} FROM oxsum.plan_statement_lines \
         WHERE statement_id=$1 ORDER BY created_at, line_id"
    ))
    .bind(statement_id)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(line_from_row).collect()
}

/// Inserts one statement line — the only write the lines table accepts.
async fn insert_line_in(
    tx: &mut Transaction<'_, Postgres>,
    statement_id: Uuid,
    kind: StatementLineKind,
    amount_minor: i64,
    description: &str,
    source_ref: Option<Uuid>,
) -> Result<Uuid, WalletError> {
    let line_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO oxsum.plan_statement_lines \
             (line_id, statement_id, kind, amount_minor, description, source_ref) \
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(line_id)
    .bind(statement_id)
    .bind(kind.as_str())
    .bind(amount_minor)
    .bind(description)
    .bind(source_ref)
    .execute(&mut **tx)
    .await?;
    Ok(line_id)
}

impl Db {
    /// Reads one statement by id — the row the detail and staff reads project.
    async fn statement_row(
        &self,
        statement_id: Uuid,
    ) -> Result<Option<PlanStatement>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements WHERE statement_id=$1"
        ))
        .bind(statement_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(statement_from_row).transpose()
    }

    /// A plan statement's lines in write order — the bill's itemization,
    /// named for the document the existing `oxsum.statements` usage read
    /// already spells.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn plan_statement_lines(
        &self,
        statement_id: Uuid,
    ) -> Result<Vec<PlanStatementLine>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {LINE_COLS} FROM oxsum.plan_statement_lines \
             WHERE statement_id=$1 ORDER BY created_at, line_id"
        ))
        .bind(statement_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(line_from_row).collect()
    }

    /// The `statement-past-due` pass: flips `issued` statements whose `due_at`
    /// passed to `past_due`, and past the plan's `grace_days` stamps
    /// `customer_plans.dunning_suspended_at` — the admission gate the hold
    /// path reads, the same shape `organizations.dunning_suspended_at`
    /// already proves. Bounded per pass so a backlog drains over ticks.
    ///
    /// A statement inside grace only slips `past_due`; the suspension lands
    /// when `due_at + grace_days` has passed. A payment clearing the
    /// statement lifts it — the reconcile path's job, not the sweep's.
    ///
    /// Returns what the pass did: how many statements flipped, how many plans
    /// suspended.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a limit outside 1..=1000; storage
    /// failures surface as [`WalletError`].
    pub async fn run_statement_past_due(&self, limit: i64) -> Result<PastDueReport, WalletError> {
        if !(1..=1000).contains(&limit) {
            return Err(invalid("statement pass limit must be 1..1000"));
        }
        let mut report = PastDueReport::default();
        // Candidate reads do not claim rows. The old autocommit FOR UPDATE
        // released its locks before processing and used stale state afterward.
        // Keyset pages pass every locked candidate without letting a fixed
        // oldest prefix starve independent customers. Only actual transitions
        // consume the work bound; skipped rows remain due for the next pass.
        let mut cursor_due: Option<OffsetDateTime> = None;
        let mut cursor_id: Option<Uuid> = None;
        let mut processed = 0;
        while processed < limit {
            let due = sqlx::query(
                "SELECT s.statement_id,s.customer_plan_id,s.due_at \
                 FROM oxsum.plan_statements s \
                 LEFT JOIN oxsum.customer_plans p ON p.customer_plan_id=s.customer_plan_id \
                 WHERE s.state IN ('issued','past_due') AND s.total_minor>s.paid_minor \
                   AND s.due_at<=now() \
                   AND ($1::timestamptz IS NULL OR (s.due_at,s.statement_id)>($1,$2::uuid)) \
                   AND (s.state='issued' OR (p.state='past_due' \
                     AND p.dunning_suspended_at IS NULL \
                     AND s.due_at+(p.grace_days*interval '1 day')<=now())) \
                 ORDER BY s.due_at,s.statement_id LIMIT $3",
            )
            .bind(cursor_due)
            .bind(cursor_id)
            .bind(limit)
            .fetch_all(self.pool())
            .await?;
            if due.is_empty() {
                break;
            }
            for candidate in due {
                if processed >= limit {
                    break;
                }
                let statement_id: Uuid = candidate.try_get("statement_id")?;
                let plan_id: Option<Uuid> = candidate.try_get("customer_plan_id")?;
                cursor_due = Some(candidate.try_get("due_at")?);
                cursor_id = Some(statement_id);
                let mut tx = self.pool().begin().await?;
                let plan = if let Some(plan_id) = plan_id {
                    let row = sqlx::query(
                        "SELECT grace_days,state,dunning_suspended_at \
                         FROM oxsum.customer_plans WHERE customer_plan_id=$1 \
                         FOR UPDATE SKIP LOCKED",
                    )
                    .bind(plan_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    let Some(row) = row else {
                        tx.rollback().await?;
                        continue;
                    };
                    Some(row)
                } else {
                    None
                };
                // Recheck mutable eligibility while both rows stay locked. A
                // payment, void or extension committed since the scan cannot
                // be suspended from its old state, even across a page boundary.
                let current = sqlx::query(
                    "SELECT state,due_at FROM oxsum.plan_statements \
                     WHERE statement_id=$1 AND state IN ('issued','past_due') \
                       AND total_minor>paid_minor AND due_at<=now() \
                     FOR UPDATE SKIP LOCKED",
                )
                .bind(statement_id)
                .fetch_optional(&mut *tx)
                .await?;
                let Some(current) = current else {
                    tx.rollback().await?;
                    continue;
                };
                let mut changed = false;
                if current.try_get::<String, _>("state")? == "issued" {
                    sqlx::query(
                        "UPDATE oxsum.plan_statements \
                         SET state='past_due',past_due_at=now(),updated_at=now() \
                         WHERE statement_id=$1",
                    )
                    .bind(statement_id)
                    .execute(&mut *tx)
                    .await?;
                    report.flipped += 1;
                    changed = true;
                }
                if let Some(plan) = plan {
                    let grace: i32 = plan.try_get("grace_days")?;
                    let past_grace: bool =
                        sqlx::query_scalar("SELECT $1::timestamptz+($2*interval '1 day')<=now()")
                            .bind(current.try_get::<OffsetDateTime, _>("due_at")?)
                            .bind(grace)
                            .fetch_one(&mut *tx)
                            .await?;
                    if past_grace
                        && plan.try_get::<String, _>("state")? == "past_due"
                        && plan
                            .try_get::<Option<OffsetDateTime>, _>("dunning_suspended_at")?
                            .is_none()
                    {
                        sqlx::query(
                            "UPDATE oxsum.customer_plans \
                             SET dunning_suspended_at=now(),updated_at=now() \
                             WHERE customer_plan_id=$1",
                        )
                        .bind(plan_id)
                        .execute(&mut *tx)
                        .await?;
                        report.suspended += 1;
                        changed = true;
                    }
                }
                tx.commit().await?;
                if changed {
                    processed += 1;
                }
            }
        }
        Ok(report)
    }
}

/// What a `statement-past-due` pass did.
#[derive(Debug, Default, Clone, Copy)]
pub struct PastDueReport {
    /// Statements flipped `issued → past_due`.
    pub flipped: i64,
    /// Plans the grace lapse suspended.
    pub suspended: i64,
}

/// Read only the immediate committed correction link inside the caller's scope.
async fn successor_in(
    tx: &mut Transaction<'_, Postgres>,
    statement: Uuid,
) -> Result<Option<Uuid>, WalletError> {
    Ok(sqlx::query_scalar(
        "SELECT statement_id FROM oxsum.invoice_corrections WHERE predecessor_statement_id=$1",
    )
    .bind(statement)
    .fetch_optional(&mut **tx)
    .await?)
}
