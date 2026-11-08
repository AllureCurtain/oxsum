//! Plan fee journals and revenue allocation (issue #611, T20).
//!
//! T19 made a plan period exist only once its authorization does; this module
//! records what each period *cost* once it closes. A journal is the per-period
//! invoice: `draft → posted → paid | void`, written inside the same
//! transaction that closes the `plan_periods` row — the boundary rollover in
//! `crate::plans::Db::plan_rollover_in`, the renewal arm of
//! `activate_plan_period`, and the boundary-cancel path all share the one
//! writer, [`Db::journal_closing_period_in`].
//!
//! The bill is *in arrears*: a `planPeriod` order prepays the period it
//! authorizes (`debit Cash / credit Liabilities:UnearnedPlanFees`), the close
//! recognizes revenue (`debit Receivables / credit Income:PlanFees`) and
//! reclassifies the prepayment into the receivable. What the prepayment did
//! not cover — overage beyond the quota, or a fee that rose under a change —
//! stays `posted` as the open receivable a later order settles, and the plan's
//! `past_due` state and the posted journal are the same fact read two ways.
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
    /// An overage-enabled plan's wallet-billed overflow inside the period,
    /// charged at par — `sold_plans` carries no separate overage rate, so the
    /// settled charge is its own price.
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

const JOURNAL_COLS: &str = "journal_id,customer_plan_id,organization_id,business_id,\
     plan_period_id,period_index,state,total_minor,settled_minor,currency,\
     paid_order_id,disputed_minor,replaces_journal_id,void_reason,\
     posted_at,paid_at,voided_at,created_at,updated_at";

const LINE_COLS: &str = "line_id,journal_id,kind,amount_minor,description,source_ref,created_at";

fn journal_from_row(row: &PgRow) -> Result<InvoiceJournal, WalletError> {
    Ok(InvoiceJournal {
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
    /// [`WalletError::Conflict`] for an already-void journal (replay answers
    /// it instead); storage failures surface as [`WalletError`].
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
        let Some(journal) = journal_in(&mut tx, journal_id).await? else {
            return Err(WalletError::NotFound("unknown journal".into()));
        };
        if journal.business_id != business || journal.state == JournalState::Draft {
            return Err(WalletError::NotFound("unknown journal".into()));
        }
        if journal.state == JournalState::Void {
            let lines = lines_in(&mut tx, journal_id).await?;
            tx.commit().await?;
            return Ok(InvoiceJournalDetail { journal, lines });
        }
        let lines = lines_in(&mut tx, journal_id).await?;
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
                &mut tx,
                journal_id,
                line.kind,
                -line.amount_minor,
                &format!("void reversal of {}", line.description),
                Some(line.line_id),
            )
            .await?;
        }
        let receipt = wallet
            .plan_journal_void_in(
                &mut tx,
                &journal_id,
                charges,
                discount,
                promo,
                journal.settled_minor,
                journal.state == JournalState::Paid,
                on,
            )
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
            "reversalEntryId": receipt.map(|r| *r.entry_id.as_uuid()),
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
        sqlx::query(
            "UPDATE oxsum.invoice_journals \
             SET state='void', voided_at=now(), void_reason=$2, updated_at=now() \
             WHERE journal_id=$1",
        )
        .bind(journal_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        // The customer-facing statement wraps this journal — it follows the
        // void in the same transaction so the document never outlives its
        // receivable as still-owed or paid-in-full (issue #779).
        self.void_statement_for_journal_in(&mut tx, journal_id, reason)
            .await?;
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
}

/// The journal row inside the caller's transaction — `FOR UPDATE` so the
/// settlement and correction writers serialize on it.
async fn journal_in(
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

    /// Writes the journal a closing plan period bills — the one writer every
    /// close path shares (`plan_rollover_in`'s cancel/change/past-due arms and
    /// `activate_plan_period`'s renewal arm). The journal lands `draft` and
    /// walks to `posted` — or straight to `paid` when the bill is already
    /// covered — inside this transaction, beside the accrual entry the same
    /// transaction commits.
    ///
    /// Lines: `base_fee` the sold version's price; `usage_overage` the
    /// overage-enabled plan's wallet-billed overflow inside the period at par
    /// (`sold_plans` carries no overage rate — the settled charge is the
    /// price); `discount` the credit notes T17 applied to the order that
    /// authorized the period; `promo_credit` promo covering the fee — a
    /// complimentary grant's full coverage, or a payment's bundle promo leg,
    /// clamped to what the promo pool settles. `total_minor` is their sum.
    ///
    /// Settlement: a `payment_order`-authorized period's prepayment
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
        // Already billed? The partial unique makes the close idempotent — a
        // replayed rollover or a second writer finds the standing journal.
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

        // Usage overage: an overage-enabled, metered plan's wallet-billed
        // overflow inside the period — the turns the quota could not cover
        // charged the wallet at par (the settled charge is the price;
        // `sold_plans` carries no separate overage rate). Tenant-scoped
        // `usage_records` are keyed by the organization's tenant id.
        if sold.overage_enabled && sold.quota_minor.is_some() {
            let tenant: Option<String> = sqlx::query_scalar(
                "SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1",
            )
            .bind(organization_id)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some(tenant) = tenant {
                let overage: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(SUM(charged_minor),0)::bigint FROM oxsum.usage_records \
                     WHERE tenant_id=$1 AND settled_at >= $2 AND settled_at < $3",
                )
                .bind(tenant)
                .bind(period.period_start)
                .bind(period.period_end)
                .fetch_one(&mut **tx)
                .await?;
                if overage > 0 {
                    lines.push(PendingLine {
                        kind: JournalLineKind::UsageOverage,
                        amount_minor: overage,
                        description: format!("usage overage, period {}", period.period_index),
                        source_ref: Some(period.plan_period_id),
                    });
                }
            }
        }

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
        let mut receivable = 0;
        let mut payable = 0;
        if journal.state == JournalState::Void {
            let original = wallet
                .plan_source_credit_in(
                    tx,
                    &format!("plan-journal/{}/void", journal.journal_id),
                    crate::wallet::REFUND_PAYABLE,
                )
                .await?;
            let refunded:i64=sqlx::query_scalar("SELECT COALESCE(SUM(payable_minor),0)::bigint FROM oxsum.plan_refund_effects WHERE journal_id=$1")
                .bind(journal.journal_id).fetch_one(&mut **tx).await?;
            let ids: Vec<Uuid> =
                sqlx::query_scalar("SELECT dispute_id FROM oxsum.disputes WHERE order_id=$1")
                    .bind(order_id)
                    .fetch_all(&mut **tx)
                    .await?;
            let mut debited = 0i64;
            for id in ids {
                debited = debited
                    .checked_add(
                        wallet
                            .plan_source_debit_in(
                                tx,
                                &format!("dispute-recharge/{id}"),
                                crate::wallet::REFUND_PAYABLE,
                            )
                            .await?,
                    )
                    .ok_or_else(|| invalid("chargeback total overflows"))?;
            }
            // The void entry may contain payable credit and debit legs. Its
            // net source, not the tenant's aggregate payable, is the limit.
            let void_debit = wallet
                .plan_source_debit_in(
                    tx,
                    &format!("plan-journal/{}/void", journal.journal_id),
                    crate::wallet::REFUND_PAYABLE,
                )
                .await?;
            payable = amount_minor.min(
                original
                    .saturating_sub(void_debit)
                    .saturating_sub(refunded)
                    .saturating_sub(debited)
                    .max(0),
            );
        } else {
            receivable = amount_minor.min(journal.settled_minor);
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
