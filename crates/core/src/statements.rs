//! Statements: the monthly billing document (issue #124, roadmap P3-2b).
//!
//! One row on `oxsum.statements` per organization per closed UTC month, itemizing
//! what the period's settled usage was charged — the document an organization owes
//! against when its billing is "borrow first, settle monthly".
//!
//! The document lifecycle is `status`: draft → finalized. A draft is the operator's
//! working copy — regenerating rebuilds it from the usage rows; finalizing locks the
//! lines, snapshots the organization's payment terms into the due date, and pins the
//! ledger window the lines prove (`log_from_index`/`log_to_index` over the covered
//! settlement entries).
//!
//! The payment lifecycle is `payment_status`: pending → paid, with overdue and
//! suspended as the unpaid standings. What a statement is owed is
//! `credit_drawn_minor` — the part of the period's charges the credit line carried —
//! because usage drawn from bonus and purchased pools was paid for already.
//! `paid_minor` is derived bookkeeping, not an allocation event log: credit-line
//! repayments settle the line's draws oldest first, so a statement is paid in the
//! measure that all-time repaid exceeds the draws booked before its period.
//! [`Db::reconcile_statements`] rewrites the columns whenever the ledger may have
//! moved — after a top-up, a redemption, a recorded payment, or on read — so a
//! self-serve top-up pays a statement without anyone recording it.
//!
//! `overdue_at` is the timestamp of first answering past due — the flip is lazy:
//! reads and reconcile paths persist it, no scheduler is required for a document
//! that only needs to be right when looked at.

use serde::Serialize;
use sqlx::Row;
use time::{Date, Duration, Month, OffsetDateTime};
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::orgs::malformed;
use crate::wallet::Wallet;

// `due_date` serializes as a bare `YYYY-MM-DD` — the contract's `format: date`.
time::serde::format_description!(due_date_format, Date, "[year]-[month]-[day]");

/// A statement's document standing: draft is the operator's working copy — rebuilt on
/// regeneration; finalized is issued, its lines and totals locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatementStatus {
    Draft,
    Finalized,
}

impl StatementStatus {
    fn parse(raw: &str) -> Result<Self, WalletError> {
        match raw {
            "draft" => Ok(Self::Draft),
            "finalized" => Ok(Self::Finalized),
            other => Err(malformed("statements.status", other)),
        }
    }
}

/// Where payment stands on a finalized statement: pending while the drawn part is
/// unpaid, overdue once the due date has passed, suspended when the operator marks
/// it, paid once repayments cover `credit_drawn_minor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentStatus {
    Pending,
    Paid,
    Overdue,
    Suspended,
}

impl PaymentStatus {
    fn parse(raw: &str) -> Result<Self, WalletError> {
        match raw {
            "pending" => Ok(Self::Pending),
            "paid" => Ok(Self::Paid),
            "overdue" => Ok(Self::Overdue),
            "suspended" => Ok(Self::Suspended),
            other => Err(malformed("statements.payment_status", other)),
        }
    }
}

/// One line of a statement: the period's settled usage aggregated per channel and
/// model — what the line's requests were charged and the token totals behind it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementLine {
    pub channel: String,
    pub model: String,
    /// Settled requests the line aggregates.
    pub turns: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    /// What the line's usage was charged.
    pub amount_minor: i64,
}

/// A billing statement as the API presents it — the document, its money figures,
/// its payment standing, and the ledger window the lines prove.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    pub id: Uuid,
    pub organization_id: Uuid,
    /// The billed month, `YYYY-MM`.
    pub period: String,
    pub status: StatementStatus,
    pub payment_status: PaymentStatus,
    /// The period's settled usage charges, own funds and credit alike.
    pub total_minor: i64,
    /// The part of the period's charges the credit line carried — what the
    /// statement is owed.
    pub credit_drawn_minor: i64,
    /// Repayments allocated to the statement so far.
    pub paid_minor: i64,
    /// `credit_drawn_minor` minus `paid_minor` — what is still owed.
    pub outstanding_minor: i64,
    /// The organization's payment terms at finalization, snapshotted.
    pub payment_terms_days: i32,
    /// When payment falls due — set at finalization, `YYYY-MM-DD`.
    #[serde(with = "due_date_format::option")]
    pub due_date: Option<Date>,
    /// The first covered ledger entry's log index — the proof window's bounds,
    /// set at finalization.
    pub log_from_index: Option<i64>,
    pub log_to_index: Option<i64>,
    /// Usage rows the statement itemizes.
    pub entry_count: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finalized_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub paid_at: Option<OffsetDateTime>,
    /// When the statement first answered past due.
    #[serde(with = "time::serde::rfc3339::option")]
    pub overdue_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub suspended_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The shape `generate_statement` fills: the period's totals and lines read from the
/// usage rows before they are persisted.
pub struct DraftStatement {
    pub total_minor: i64,
    pub credit_drawn_minor: i64,
    pub entry_count: i64,
    pub log_from_index: Option<i64>,
    pub log_to_index: Option<i64>,
    pub lines: Vec<StatementLine>,
}

/// A `YYYY-MM` billing month, parsed: the period the statement covers and the
/// `[first, last]` booking-date window its entries fall in.
#[derive(Debug, Clone, Copy)]
pub struct StatementPeriod {
    pub first: Date,
    pub last: Date,
}

impl StatementPeriod {
    /// The month as `YYYY-MM` — the form statements and the contract speak in.
    pub fn label(&self) -> String {
        format!(
            "{:04}-{:02}",
            self.first.year(),
            u8::from(self.first.month())
        )
    }
}

/// `YYYY-MM` as the month's first and last day. A malformed month is a validation
/// failure; so is a month that has not fully ended — the running month is still
/// accumulating the usage a statement would freeze mid-flight.
pub fn statement_period(period: &str) -> Result<StatementPeriod, WalletError> {
    let bounds = period_bounds(period)?;
    if bounds.last >= OffsetDateTime::now_utc().date() {
        return Err(WalletError::InvalidInput(
            "only a month that has fully ended can be billed".into(),
        ));
    }
    Ok(bounds)
}

/// One `statements` row out of the shared `SELECT` the statement reads run.
fn statement_from_row(row: &sqlx::postgres::PgRow) -> Result<Statement, WalletError> {
    let credit_drawn: i64 = row.try_get("credit_drawn_minor")?;
    let paid: i64 = row.try_get("paid_minor")?;
    Ok(Statement {
        id: row.try_get("statement_id")?,
        organization_id: row.try_get("organization_id")?,
        period: row.try_get("period")?,
        status: StatementStatus::parse(&row.try_get::<String, _>("status")?)?,
        payment_status: PaymentStatus::parse(&row.try_get::<String, _>("payment_status")?)?,
        total_minor: row.try_get("total_minor")?,
        credit_drawn_minor: credit_drawn,
        paid_minor: paid,
        outstanding_minor: credit_drawn - paid,
        payment_terms_days: row.try_get("payment_terms_days")?,
        due_date: row.try_get("due_date")?,
        log_from_index: row.try_get("log_from_index")?,
        log_to_index: row.try_get("log_to_index")?,
        entry_count: row.try_get("entry_count")?,
        finalized_at: row.try_get("finalized_at")?,
        paid_at: row.try_get("paid_at")?,
        overdue_at: row.try_get("overdue_at")?,
        suspended_at: row.try_get("suspended_at")?,
        created_at: row.try_get("created_at")?,
    })
}

const STATEMENT_COLS: &str = "statement_id, organization_id, period, status, \
     payment_status, total_minor, credit_drawn_minor, paid_minor, payment_terms_days, \
     due_date, log_from_index, log_to_index, entry_count, finalized_at, paid_at, \
     overdue_at, suspended_at, created_at";

/// The FROM clause the ledger-window and aggregate reads share: the usage rows whose
/// settlement entries booked into the period. The join is what ties a statement to
/// ledger time — `booking_date`, the same basis a month closes on — rather than to
/// `settled_at`, the instant the row landed. The schema name is assembled from the
/// tenant id, which `Wallet::open` already validated to `[a-z0-9_]`.
fn period_join(tenant_id: &str) -> String {
    // The tenant filter on `u` is load-bearing: an entry id is derived from the
    // hold key, and the same key names the same id in every tenant's ledger —
    // without it, another tenant's usage rows join this ledger's entries.
    format!(
        "FROM oxsum.usage_records u \
         JOIN ledger_{tenant_id}.entries e ON e.entry_id = u.entry_id \
         WHERE u.tenant_id = $3 \
           AND e.booking_date >= $1 AND e.booking_date <= $2"
    )
}

impl Db {
    /// Every organization that has ever recorded usage — the candidate set bulk
    /// generation walks. The period itself is not a filter here: a statement
    /// periodizes by the settlement entry's `booking_date`, while a usage row's
    /// `settled_at` is the instant it was written — usually the same second, but
    /// a settlement sealed beside midnight can write its row the next day.
    /// `generate_statement` decides precisely per organization, so a candidate
    /// whose period is empty simply produces an empty draft.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn billable_organizations(
        &self,
    ) -> Result<Vec<crate::orgs::AdminOrganization>, WalletError> {
        let rows = sqlx::query(
            "SELECT o.organization_id, o.name, o.tenant_id, o.kind, o.created_at, \
             o.payment_terms_days, count(m.user_id) AS members \
             FROM oxsum.organizations o \
             LEFT JOIN oxsum.memberships m USING (organization_id) \
             WHERE EXISTS (SELECT 1 FROM oxsum.usage_records u \
                           WHERE u.tenant_id = o.tenant_id) \
             GROUP BY o.organization_id ORDER BY o.created_at",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(crate::orgs::admin_organization).collect()
    }

    /// The period's totals and lines for one organization, read from the usage rows
    /// and the ledger — `credit_drawn_minor` is the period's settled draws on the
    /// credit line, what the statement is owed, and the log-index window bounds the
    /// entries the lines prove.
    ///
    /// An organization whose period has no usage answers a draft of zeros and no
    /// lines; the caller decides whether an empty statement is worth issuing.
    async fn draft_of(
        &self,
        tenant_id: &str,
        wallet: &Wallet,
        period: &StatementPeriod,
    ) -> Result<DraftStatement, WalletError> {
        let totals_sql = format!(
            "SELECT count(*)::bigint AS entry_count, \
             COALESCE(sum(u.charged_minor), 0)::bigint AS total_minor, \
             min(e.log_index)::bigint AS log_from, max(e.log_index)::bigint AS log_to \
             {}",
            period_join(tenant_id)
        );
        let totals = sqlx::query(&totals_sql)
            .bind(period.first)
            .bind(period.last)
            .bind(tenant_id)
            .fetch_one(self.pool())
            .await?;
        let lines_sql = format!(
            "SELECT u.channel, u.model, \
             count(*)::bigint AS turns, \
             COALESCE(sum(u.input_tokens), 0)::bigint AS input_tokens, \
             COALESCE(sum(u.output_tokens), 0)::bigint AS output_tokens, \
             COALESCE(sum(u.cached_tokens), 0)::bigint AS cached_tokens, \
             COALESCE(sum(u.reasoning_tokens), 0)::bigint AS reasoning_tokens, \
             COALESCE(sum(u.charged_minor), 0)::bigint AS amount_minor \
             {} \
             GROUP BY u.channel, u.model ORDER BY u.channel, u.model",
            period_join(tenant_id)
        );
        let line_rows = sqlx::query(&lines_sql)
            .bind(period.first)
            .bind(period.last)
            .bind(tenant_id)
            .fetch_all(self.pool())
            .await?;
        let lines = line_rows
            .iter()
            .map(|row| {
                Ok(StatementLine {
                    channel: row.try_get("channel")?,
                    model: row.try_get("model")?,
                    turns: row.try_get("turns")?,
                    input_tokens: row.try_get("input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    cached_tokens: row.try_get("cached_tokens")?,
                    reasoning_tokens: row.try_get("reasoning_tokens")?,
                    amount_minor: row.try_get("amount_minor")?,
                })
            })
            .collect::<Result<Vec<_>, WalletError>>()?;
        Ok(DraftStatement {
            total_minor: totals.try_get("total_minor")?,
            credit_drawn_minor: wallet
                .credit_drawn_between(period.first, period.last)
                .await?,
            entry_count: totals.try_get("entry_count")?,
            log_from_index: totals.try_get("log_from")?,
            log_to_index: totals.try_get("log_to")?,
            lines,
        })
    }

    /// Generates or regenerates the organization's draft statement for a period:
    /// the totals and lines the usage rows and the ledger currently show. A draft
    /// is rebuilt each call, so generation is idempotent and a late-landing usage
    /// row is picked up by regenerating; a statement already finalized stands —
    /// the committed document is the answer, not a rebuilt draft.
    ///
    /// `None` answers a period the organization has no usage in and no standing
    /// statement for: a month with nothing to bill is no document at all.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a malformed or still-running period;
    /// storage failures surface as [`WalletError`].
    pub async fn generate_statement(
        &self,
        organization: &crate::orgs::AdminOrganization,
        wallet: &Wallet,
        period: &StatementPeriod,
    ) -> Result<Option<Statement>, WalletError> {
        let draft = self
            .draft_of(&organization.tenant_id, wallet, period)
            .await?;
        if draft.entry_count == 0 {
            // Nothing billed that month — but a standing row still answers: a
            // draft rebuilds to empty, a finalized one is returned untouched.
            return self.statement_at(organization.id, &period.label()).await;
        }
        let mut tx = self.pool().begin().await?;
        // Draft-only upsert: an existing finalized row fails the WHERE, the RETURNING
        // comes back empty, and the standing document is fetched and answered instead.
        let row = sqlx::query(
            "INSERT INTO oxsum.statements \
                 (statement_id, organization_id, period, status, payment_status, \
                  total_minor, credit_drawn_minor, payment_terms_days, entry_count) \
             VALUES ($1, $2, $3, 'draft', 'pending', $4, $5, $6, $7) \
             ON CONFLICT (organization_id, period) DO UPDATE \
             SET total_minor = EXCLUDED.total_minor, \
                 credit_drawn_minor = EXCLUDED.credit_drawn_minor, \
                 entry_count = EXCLUDED.entry_count, \
                 payment_terms_days = EXCLUDED.payment_terms_days, \
                 updated_at = now() \
             WHERE oxsum.statements.status = 'draft' \
             RETURNING statement_id, status",
        )
        .bind(Uuid::new_v4())
        .bind(organization.id)
        .bind(period.label())
        .bind(draft.total_minor)
        .bind(draft.credit_drawn_minor)
        .bind(organization.payment_terms_days)
        .bind(draft.entry_count)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return self.statement_at(organization.id, &period.label()).await;
        };
        let statement_id: Uuid = row.try_get("statement_id")?;
        // The lines are the draft's working copy: rebuilt whole each generation.
        sqlx::query("DELETE FROM oxsum.statement_lines WHERE statement_id = $1")
            .bind(statement_id)
            .execute(&mut *tx)
            .await?;
        for (line_no, line) in draft.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO oxsum.statement_lines \
                     (statement_id, line_no, channel, model, turns, input_tokens, \
                      output_tokens, cached_tokens, reasoning_tokens, amount_minor) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(statement_id)
            .bind(line_no as i32 + 1)
            .bind(&line.channel)
            .bind(&line.model)
            .bind(line.turns)
            .bind(line.input_tokens)
            .bind(line.output_tokens)
            .bind(line.cached_tokens)
            .bind(line.reasoning_tokens)
            .bind(line.amount_minor)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.statement_by_id(statement_id).await
    }

    /// One statement by id, whichever standing it is in — `None` when the id is
    /// unknown.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn statement_by_id(&self, id: Uuid) -> Result<Option<Statement>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.statements WHERE statement_id = $1"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(statement_from_row).transpose()
    }

    /// The organization's statement for the period, `None` when it was never
    /// generated.
    async fn statement_at(
        &self,
        organization_id: Uuid,
        period: &str,
    ) -> Result<Option<Statement>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.statements \
             WHERE organization_id = $1 AND period = $2"
        ))
        .bind(organization_id)
        .bind(period)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(statement_from_row).transpose()
    }

    /// The lines a statement itemizes, in their stored order.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn statement_lines(
        &self,
        statement_id: Uuid,
    ) -> Result<Vec<StatementLine>, WalletError> {
        let rows = sqlx::query(
            "SELECT channel, model, turns, input_tokens, output_tokens, cached_tokens, \
             reasoning_tokens, amount_minor \
             FROM oxsum.statement_lines WHERE statement_id = $1 ORDER BY line_no",
        )
        .bind(statement_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(StatementLine {
                    channel: row.try_get("channel")?,
                    model: row.try_get("model")?,
                    turns: row.try_get("turns")?,
                    input_tokens: row.try_get("input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    cached_tokens: row.try_get("cached_tokens")?,
                    reasoning_tokens: row.try_get("reasoning_tokens")?,
                    amount_minor: row.try_get("amount_minor")?,
                })
            })
            .collect()
    }

    /// Every statement, newest period first — the admin list, optionally narrowed by
    /// period or organization.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn statements(
        &self,
        organization_id: Option<Uuid>,
        period: Option<&str>,
    ) -> Result<Vec<Statement>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.statements \
             WHERE ($1::uuid IS NULL OR organization_id = $1) \
               AND ($2::text IS NULL OR period = $2) \
             ORDER BY period DESC, created_at DESC"
        ))
        .bind(organization_id)
        .bind(period)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(statement_from_row).collect()
    }

    /// The organization's finalized statements, newest period first — what the
    /// organization's own statement list shows. Drafts are the operator's working
    /// copy and never appear here.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn organization_statements(
        &self,
        organization_id: Uuid,
    ) -> Result<Vec<Statement>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.statements \
             WHERE organization_id = $1 AND status = 'finalized' \
             ORDER BY period DESC"
        ))
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(statement_from_row).collect()
    }

    /// Finalizes a draft: recomputes the totals from the usage rows one last time,
    /// snapshots the organization's payment terms into the due date, pins the
    /// log-index window the covered entries sit in, and locks the document. The
    /// call is idempotent — an already-final statement answers itself, so a repeat
    /// click and a retry say the same thing.
    ///
    /// A statement that owes nothing — `credit_drawn_minor` zero — is paid at
    /// issue: there is nothing to collect.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unknown id; [`WalletError::InvalidInput`]
    /// for a statement that is not a draft; storage failures surface as
    /// [`WalletError`].
    pub async fn finalize_statement(
        &self,
        statement_id: Uuid,
        wallet: &Wallet,
        on: Date,
    ) -> Result<Statement, WalletError> {
        let existing = self
            .statement_by_id(statement_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("the statement is not known".into()))?;
        if existing.status == StatementStatus::Finalized {
            return Ok(existing);
        }
        let organization = self.organization_by_id(existing.organization_id).await?;
        // The period a draft covers is closed by construction, so this parse cannot
        // fail the ended check; it exists for the date bounds.
        let period = period_bounds(&existing.period)?;
        let draft = self
            .draft_of(&organization.tenant_id, wallet, &period)
            .await?;
        let due_date = on + Duration::days(i64::from(organization.payment_terms_days));
        // The line may already have been repaid while the statement stood in draft:
        // the same derivation reconcile runs, evaluated at issue.
        let repaid = wallet.credit_repaid().await?;
        let prior_draws =
            wallet.credit_drawn_through(period.last).await? - draft.credit_drawn_minor;
        let paid_minor = (repaid - prior_draws).clamp(0, draft.credit_drawn_minor);
        let mut tx = self.pool().begin().await?;
        // Draft-only update: a racing finalize loses the WHERE and re-reads the
        // standing document below — the same idempotent answer a repeat gives.
        let updated = sqlx::query(
            "UPDATE oxsum.statements SET status = 'finalized', \
                 total_minor = $2, credit_drawn_minor = $3, paid_minor = $4, \
                 payment_terms_days = $5, due_date = $6, log_from_index = $7, \
                 log_to_index = $8, entry_count = $9, finalized_at = now(), \
                 payment_status = CASE WHEN $4 >= $3 THEN 'paid' ELSE 'pending' END, \
                 paid_at = CASE WHEN $4 >= $3 THEN now() ELSE NULL END, \
                 updated_at = now() \
             WHERE statement_id = $1 AND status = 'draft' \
             RETURNING statement_id",
        )
        .bind(statement_id)
        .bind(draft.total_minor)
        .bind(draft.credit_drawn_minor)
        .bind(paid_minor)
        .bind(organization.payment_terms_days)
        .bind(due_date)
        .bind(draft.log_from_index)
        .bind(draft.log_to_index)
        .bind(draft.entry_count)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_some() {
            sqlx::query("DELETE FROM oxsum.statement_lines WHERE statement_id = $1")
                .bind(statement_id)
                .execute(&mut *tx)
                .await?;
            for (line_no, line) in draft.lines.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO oxsum.statement_lines \
                         (statement_id, line_no, channel, model, turns, input_tokens, \
                          output_tokens, cached_tokens, reasoning_tokens, amount_minor) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                )
                .bind(statement_id)
                .bind(line_no as i32 + 1)
                .bind(&line.channel)
                .bind(&line.model)
                .bind(line.turns)
                .bind(line.input_tokens)
                .bind(line.output_tokens)
                .bind(line.cached_tokens)
                .bind(line.reasoning_tokens)
                .bind(line.amount_minor)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        self.statement_by_id(statement_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("the statement is not known".into()))
    }

    /// Marks a statement suspended — the standing the platform puts an unpaid bill
    /// in when the grace has run out. Only a pending or overdue statement suspends:
    /// a draft is not issued and a paid one is settled. The transition is
    /// idempotent on its own — suspending an already-suspended statement answers
    /// the standing document.
    ///
    /// A suspended statement still accrues payments: [`reconcile_statements`]
    /// keeps its `paid_minor` honest, and enough repaid turns it `paid`.
    ///
    /// [`reconcile_statements`]: Self::reconcile_statements
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unknown id; [`WalletError::InvalidInput`]
    /// for a draft or a paid statement; storage failures surface as [`WalletError`].
    pub async fn suspend_statement(&self, statement_id: Uuid) -> Result<Statement, WalletError> {
        let existing = self
            .statement_by_id(statement_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("the statement is not known".into()))?;
        match existing.payment_status {
            PaymentStatus::Suspended => return Ok(existing),
            PaymentStatus::Pending | PaymentStatus::Overdue => {}
            PaymentStatus::Paid => {
                return Err(WalletError::InvalidInput(
                    "a paid statement cannot be suspended".into(),
                ));
            }
        }
        if existing.status != StatementStatus::Finalized {
            return Err(WalletError::InvalidInput(
                "a draft statement is not issued and cannot be suspended".into(),
            ));
        }
        sqlx::query(
            "UPDATE oxsum.statements SET payment_status = 'suspended', \
                 suspended_at = now(), updated_at = now() \
             WHERE statement_id = $1 AND payment_status IN ('pending', 'overdue')",
        )
        .bind(statement_id)
        .execute(self.pool())
        .await?;
        self.statement_by_id(statement_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("the statement is not known".into()))
    }

    /// Rewrites the payment bookkeeping of an organization's open statements from
    /// the ledger — the lazy half of "settle monthly" (issue #124).
    ///
    /// A statement owes `credit_drawn_minor`, and credit-line repayments settle the
    /// line's draws oldest first: for a statement whose period ended `last`, the
    /// draws ahead of it in the queue are everything the line drew through the end
    /// of the prior period — `credit_drawn_through(last) - credit_drawn_minor` — so
    /// what repayments have reached it is `repaid` minus that earlier debt, clamped
    /// to the statement's own due. The derivation makes `paid_minor` a fact of the
    /// ledger rather than an allocation event: any path that repays the line —
    /// a top-up, a redemption, a recorded statement payment — reconciles the
    /// statements without naming them.
    ///
    /// Status follows the figure: a statement repaid in full turns `paid`
    /// (`paid_at` first-sets); a pending one whose due date has passed turns
    /// `overdue`; suspended stands until paid — a suspension is a standing, not a
    /// refusal of money.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub async fn reconcile_statements(
        &self,
        organization_id: Uuid,
        wallet: &Wallet,
        today: Date,
    ) -> Result<(), WalletError> {
        let open = sqlx::query(&format!(
            "SELECT {STATEMENT_COLS} FROM oxsum.statements \
             WHERE organization_id = $1 AND status = 'finalized' \
               AND payment_status <> 'paid' \
             ORDER BY period"
        ))
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        if open.is_empty() {
            return Ok(());
        }
        let repaid = wallet.credit_repaid().await?;
        for row in &open {
            let statement = statement_from_row(row)?;
            let last = period_bounds(&statement.period)?.last;
            let prior_draws =
                wallet.credit_drawn_through(last).await? - statement.credit_drawn_minor;
            let paid = (repaid - prior_draws).clamp(0, statement.credit_drawn_minor);
            // Standing after the new figure: paid once covered, suspended holds,
            // otherwise the due date decides pending from overdue.
            let status = if paid >= statement.credit_drawn_minor {
                "paid"
            } else if statement.payment_status == PaymentStatus::Suspended {
                "suspended"
            } else if statement.due_date.is_some_and(|d| d < today) {
                "overdue"
            } else {
                "pending"
            };
            sqlx::query(
                "UPDATE oxsum.statements SET paid_minor = $2, payment_status = $3, \
                     paid_at = CASE WHEN $3 = 'paid' THEN COALESCE(paid_at, now()) ELSE paid_at END, \
                     overdue_at = CASE WHEN $3 = 'overdue' THEN COALESCE(overdue_at, now()) ELSE overdue_at END, \
                     suspended_at = CASE WHEN $3 = 'suspended' THEN COALESCE(suspended_at, now()) ELSE suspended_at END, \
                     updated_at = now() \
                 WHERE statement_id = $1",
            )
            .bind(statement.id)
            .bind(paid)
            .bind(status)
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }

    /// The credit-line debt still outstanding through a statement's period:
    /// everything the line had drawn by the period's close minus what has been
    /// repaid since. Repayments settle draws oldest-first (see
    /// [`reconcile_statements`]), so this — not just the billed statements'
    /// outstanding sum — is what a payment "on" the statement can still cover:
    /// an earlier period's draws take their share even when that period was
    /// never billed.
    ///
    /// [`reconcile_statements`]: Self::reconcile_statements
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub async fn statement_debt_through(
        &self,
        wallet: &Wallet,
        period: &str,
    ) -> Result<i64, WalletError> {
        let last = period_bounds(period)?.last;
        let drawn = wallet.credit_drawn_through(last).await?;
        let repaid = wallet.credit_repaid().await?;
        Ok((drawn - repaid).max(0))
    }

    /// Sets how long a finalized statement's payment has before it falls due. The
    /// count is snapshotted onto each statement at finalization, so changing it
    /// affects only statements finalized after — a column write, not a ledger entry.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a negative count; [`WalletError::NotFound`]
    /// for an unknown organization; storage failures surface as [`WalletError`].
    pub async fn set_payment_terms(
        &self,
        organization_id: Uuid,
        days: i32,
    ) -> Result<(), WalletError> {
        if days < 0 {
            return Err(WalletError::InvalidInput(
                "payment terms cannot be negative".into(),
            ));
        }
        let updated = sqlx::query(
            "UPDATE oxsum.organizations SET payment_terms_days = $2 \
             WHERE organization_id = $1",
        )
        .bind(organization_id)
        .bind(days)
        .execute(self.pool())
        .await?;
        if updated.rows_affected() == 0 {
            return Err(WalletError::NotFound(
                "the organization is not known".into(),
            ));
        }
        Ok(())
    }

    /// Flips pending finalized statements whose due date has passed to overdue —
    /// the lazy standing change every read runs first, so a document is right when
    /// looked at without a scheduler. `overdue_at` first-sets: the timestamp of the
    /// answer, not the check.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn flip_overdue(&self, today: Date) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.statements SET payment_status = 'overdue', \
                 overdue_at = COALESCE(overdue_at, now()), updated_at = now() \
             WHERE status = 'finalized' AND payment_status = 'pending' \
               AND due_date IS NOT NULL AND due_date < $1",
        )
        .bind(today)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

/// `YYYY-MM` as the month's first and last day, without the ended check —
/// [`statement_period`] for callers that create statements; this one for the
/// reconcile path that re-reads a period a stored statement already validated.
fn period_bounds(period: &str) -> Result<StatementPeriod, WalletError> {
    let bad = || WalletError::InvalidInput("period must be YYYY-MM".into());
    let (year, month) = period.split_once('-').ok_or_else(bad)?;
    let year = year.parse::<i32>().map_err(|_| bad())?;
    let month = Month::try_from(month.parse::<u8>().map_err(|_| bad())?).map_err(|_| bad())?;
    let first = Date::from_calendar_date(year, month, 1).map_err(|_| bad())?;
    let last = first
        .replace_day(first.month().length(first.year()))
        .map_err(crate::error::invalid)?;
    Ok(StatementPeriod { first, last })
}
