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

use doubleentry::Hash;
use serde::Serialize;
use sqlx::Row;
use time::{Date, Duration, Month, OffsetDateTime};
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::heads::{
    StatementRollupLine, StatementRollupV1, canonical_statement_rollup, statement_rollup_hash,
    statement_window_hash,
};
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

    /// The lifecycle's stored spelling — the lowercase serde form. The PDF
    /// export prints it.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Finalized => "finalized",
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

    /// The standing's stored spelling — the lowercase serde form.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Paid => "paid",
            Self::Overdue => "overdue",
            Self::Suspended => "suspended",
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
    /// The marker entry's id — the anchor appended to the tenant ledger at
    /// finalization (issue #221), `None` on a draft or an anchor-less legacy row.
    pub anchor_entry_id: Option<Uuid>,
    /// Where the marker landed in the log — the head that follows it proves the
    /// anchor's inclusion.
    pub anchor_log_index: Option<i64>,
    /// The window digest the anchor's document hash carries — the fold of the
    /// covered entries' content hashes in log order, lowercase hex.
    pub window_hash: Option<String>,
    /// The rollup digest the anchor's description commits — the canonical form
    /// of the statement's own figures and lines, lowercase hex.
    pub rollup_hash: Option<String>,
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
    /// The window digest over the covered entries' content hashes in log order —
    /// what the anchor's document hash commits (issue #221).
    pub window_hash: Hash,
    /// The rollup digest over the draft's own canonical form — what the anchor's
    /// description commits.
    pub rollup_hash: Hash,
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
        anchor_entry_id: row.try_get("anchor_entry_id")?,
        anchor_log_index: row.try_get("anchor_log_index")?,
        window_hash: row.try_get("window_hash")?,
        rollup_hash: row.try_get("rollup_hash")?,
        created_at: row.try_get("created_at")?,
    })
}

const STATEMENT_COLS: &str = "statement_id, organization_id, period, status, \
     payment_status, total_minor, credit_drawn_minor, paid_minor, payment_terms_days, \
     due_date, log_from_index, log_to_index, entry_count, finalized_at, paid_at, \
     overdue_at, suspended_at, anchor_entry_id, anchor_log_index, window_hash, \
     rollup_hash, created_at";

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
             o.payment_terms_days, o.tier, o.suspended_at, \
             o.dunning_grace_days, o.dunning_suspended_at, count(m.user_id) AS members \
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
        let entry_count: i64 = totals.try_get("entry_count")?;
        let total_minor: i64 = totals.try_get("total_minor")?;
        let credit_drawn_minor = wallet
            .credit_drawn_between(period.first, period.last)
            .await?;
        let log_from_index = totals.try_get("log_from")?;
        let log_to_index = totals.try_get("log_to")?;
        // The window fold: every covered entry's content hash in log order, the
        // order the digest pins (issue #221). `log_index IS NOT NULL` is implied —
        // a usage row only exists for a settlement whose entry sequenced.
        let hashes_sql = format!(
            "SELECT e.content_hash {} ORDER BY e.log_index",
            period_join(tenant_id)
        );
        let hash_rows = sqlx::query(&hashes_sql)
            .bind(period.first)
            .bind(period.last)
            .bind(tenant_id)
            .fetch_all(self.pool())
            .await?;
        let mut hashes = Vec::with_capacity(hash_rows.len());
        for row in &hash_rows {
            let bytes: Vec<u8> = row.try_get("content_hash")?;
            let raw: [u8; 32] = bytes.try_into().map_err(|_| {
                WalletError::InvalidInput("a covered entry's content hash is malformed".into())
            })?;
            hashes.push(Hash::from_bytes(raw));
        }
        let window_hash = statement_window_hash(&hashes);
        // The rollup: the statement's own figures and lines in the canonical form
        // the verifier re-serializes — the document the rollup digest commits.
        let rollup_lines: Vec<StatementRollupLine<'_>> = lines
            .iter()
            .map(|line| StatementRollupLine {
                channel: &line.channel,
                model: &line.model,
                turns: line.turns,
                input_tokens: line.input_tokens,
                output_tokens: line.output_tokens,
                cached_tokens: line.cached_tokens,
                reasoning_tokens: line.reasoning_tokens,
                amount_minor: line.amount_minor,
            })
            .collect();
        let canonical = canonical_statement_rollup(&StatementRollupV1 {
            v: 1,
            period: &period.label(),
            log_from_index,
            log_to_index,
            entry_count,
            total_minor,
            credit_drawn_minor,
            window_hash: &window_hash.to_hex(),
            lines: &rollup_lines,
        })?;
        let rollup_hash = statement_rollup_hash(&canonical);
        Ok(DraftStatement {
            total_minor,
            credit_drawn_minor,
            entry_count,
            log_from_index,
            log_to_index,
            window_hash,
            rollup_hash,
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
        // The anchor: one marker entry appended to the tenant ledger, committing
        // the window and rollup digests (issue #221). It precedes the draft-only
        // update so a losing finalize never records an anchor it didn't issue —
        // the idempotency key makes the append itself replay-safe.
        let description = serde_json::json!({
            "v": 1,
            "period": existing.period,
            "logFromIndex": draft.log_from_index,
            "logToIndex": draft.log_to_index,
            "entryCount": draft.entry_count,
            "totalMinor": draft.total_minor,
            "creditDrawnMinor": draft.credit_drawn_minor,
            "windowHash": draft.window_hash.to_hex(),
            "rollupHash": draft.rollup_hash.to_hex(),
        })
        .to_string();
        let (anchor_entry_id, anchor_log_index) = wallet
            .anchor_statement(statement_id, &description, draft.window_hash)
            .await?;
        let mut tx = self.pool().begin().await?;
        // Draft-only update: a racing finalize loses the WHERE and re-reads the
        // standing document below — the same idempotent answer a repeat gives.
        let updated = sqlx::query(
            "UPDATE oxsum.statements SET status = 'finalized', \
                 total_minor = $2, credit_drawn_minor = $3, paid_minor = $4, \
                 payment_terms_days = $5, due_date = $6, log_from_index = $7, \
                 log_to_index = $8, entry_count = $9, finalized_at = now(), \
                 anchor_entry_id = $10, anchor_log_index = $11, window_hash = $12, \
                 rollup_hash = $13, \
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
        .bind(*anchor_entry_id.as_uuid())
        .bind(anchor_log_index as i64)
        .bind(draft.window_hash.to_hex())
        .bind(draft.rollup_hash.to_hex())
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
            // The statement issued: subscribers hear `statement.finalized` in the
            // transaction that locked the document (issue #200).
            Self::enqueue_event(
                &mut tx,
                existing.organization_id,
                crate::webhooks::STATEMENT_FINALIZED,
                serde_json::json!({
                    "statementId": statement_id,
                    "period": existing.period,
                    "totalMinor": draft.total_minor,
                    "creditDrawnMinor": draft.credit_drawn_minor,
                    "dueDate": due_date.to_string(),
                }),
            )
            .await?;
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
        let mut tx = self.pool().begin().await?;
        let moved = sqlx::query(
            "UPDATE oxsum.statements SET payment_status = 'suspended', \
                 suspended_at = now(), updated_at = now() \
             WHERE statement_id = $1 AND payment_status IN ('pending', 'overdue')",
        )
        .bind(statement_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if moved > 0 {
            Self::enqueue_event(
                &mut tx,
                existing.organization_id,
                crate::webhooks::STATEMENT_STATUS_UPDATED,
                serde_json::json!({
                    "statementId": statement_id,
                    "period": existing.period,
                    "paymentStatus": "suspended",
                }),
            )
            .await?;
        }
        tx.commit().await?;
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
            // The standing move and its announcement share a transaction: a
            // statement that flips `overdue` or `paid` enqueues the event that
            // names the new standing, and a reconcile that changes nothing
            // enqueues nothing (issue #200).
            let mut tx = self.pool().begin().await?;
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
            .execute(&mut *tx)
            .await?;
            let standing_changed = status != statement.payment_status.as_str();
            if standing_changed {
                // `payment_overdue` is the specific flip a subscriber asks first;
                // the generic `payment_status_updated` covers it and every other
                // transition (to `paid`, back off `overdue`).
                if status == "overdue" {
                    Self::enqueue_event(
                        &mut tx,
                        organization_id,
                        crate::webhooks::STATEMENT_OVERDUE,
                        serde_json::json!({
                            "statementId": statement.id,
                            "period": statement.period,
                            "dueDate": statement.due_date.map(|d| d.to_string()),
                            "creditDrawnMinor": statement.credit_drawn_minor,
                            "paidMinor": paid,
                        }),
                    )
                    .await?;
                }
                Self::enqueue_event(
                    &mut tx,
                    organization_id,
                    crate::webhooks::STATEMENT_STATUS_UPDATED,
                    serde_json::json!({
                        "statementId": statement.id,
                        "period": statement.period,
                        "paymentStatus": status,
                    }),
                )
                .await?;
            }
            tx.commit().await?;
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

    /// Sets the grace the dunning sweep gives an organization past a finalized
    /// statement's due date before it suspends the spend (issue #258). `None`
    /// disables the automation for the organization — the column's stored
    /// meaning. Like `payment_terms_days` this is a column write: it moves the
    /// sweep's verdict on statements already overdue too, since the sweep
    /// re-derives the deadline from each statement's own `due_date`.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a negative count; [`WalletError::NotFound`]
    /// for an unknown organization; storage failures surface as [`WalletError`].
    pub async fn set_dunning_grace(
        &self,
        organization_id: Uuid,
        days: Option<i32>,
    ) -> Result<(), WalletError> {
        if days.is_some_and(|d| d < 0) {
            return Err(WalletError::InvalidInput(
                "dunning grace cannot be negative".into(),
            ));
        }
        let updated = sqlx::query(
            "UPDATE oxsum.organizations SET dunning_grace_days = $2 \
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

    /// The dunning sweep (issue #258): the statements job's enforcement half.
    ///
    /// Suspends every organization whose grace ran out — a finalized statement
    /// in `pending`/`overdue`/`suspended` standing whose `due_date` plus the
    /// organization's `dunning_grace_days` is before `today`, on an organization
    /// that has the automation on (`dunning_grace_days` set) and is not already
    /// suspended. Each suspension marks itself `dunning_suspended_at` so the
    /// other half of the sweep can tell it apart from an operator's manual
    /// `set_suspended`, suspends the outstanding statements, and fans out the
    /// `org.suspended` webhook — all inside the org-admission advisory lock, so
    /// the flag lands atomically against hold admission like `set_suspended`'s
    /// own transaction does.
    ///
    /// Reinstates every organization the sweep suspended (the marker set) once
    /// nothing unpaid stands past `due_date + grace`: the standing lifted is
    /// the sweep's own, never an operator's. A suspended statement that gets
    /// repaid through `reconcile_statements` turns `paid`, so the reinstatement
    /// follows the same figure the statements do — no separate bookkeeping.
    ///
    /// Answers `(suspended, reinstated)` for the run report.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn sweep_dunning(&self, today: Date) -> Result<(i64, i64), WalletError> {
        // The candidate rows: every grace-lapsed statement on an opted-in,
        // not-yet-suspended organization. One org may owe several; each is
        // suspended inside the same transaction that flags the org.
        let lapsing = sqlx::query(
            "SELECT s.statement_id, s.organization_id, s.period \
             FROM oxsum.statements s \
             JOIN oxsum.organizations o ON o.organization_id = s.organization_id \
             WHERE s.status = 'finalized' \
               AND s.payment_status IN ('pending', 'overdue', 'suspended') \
               AND s.due_date IS NOT NULL \
               AND o.dunning_grace_days IS NOT NULL \
               AND s.due_date + o.dunning_grace_days < $1 \
               AND o.suspended_at IS NULL \
             ORDER BY s.organization_id, s.due_date",
        )
        .bind(today)
        .fetch_all(self.pool())
        .await?;
        let mut suspended_orgs = std::collections::BTreeSet::new();
        let mut suspended = 0i64;
        for row in &lapsing {
            use sqlx::Row;
            let organization_id: Uuid = row.try_get("organization_id")?;
            let statement_id: Uuid = row.try_get("statement_id")?;
            // The flag write serializes against hold admission through the same
            // advisory lock `set_suspended` takes; `suspended_at` guarded by the
            // predicate makes the second statement of a lapsed pair a no-op on
            // the flag while its own row still moves to `suspended`.
            let mut tx = self.pool().begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || $2::text, 0))")
                .bind(crate::wallet::ORG_ADMISSION_LOCK)
                .bind(organization_id.to_string())
                .execute(&mut *tx)
                .await?;
            let flagged = sqlx::query(
                "UPDATE oxsum.organizations SET suspended_at = now(), \
                     dunning_suspended_at = now() \
                 WHERE organization_id = $1 AND suspended_at IS NULL \
                 RETURNING organization_id",
            )
            .bind(organization_id)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            if flagged {
                Self::enqueue_event(
                    &mut tx,
                    organization_id,
                    crate::webhooks::ORG_SUSPENDED,
                    serde_json::json!({
                        "organizationId": organization_id,
                        "reason": "dunning",
                        "statementId": statement_id,
                    }),
                )
                .await?;
            }
            let moved = sqlx::query(
                "UPDATE oxsum.statements SET payment_status = 'suspended', \
                     suspended_at = COALESCE(suspended_at, now()), updated_at = now() \
                 WHERE statement_id = $1 AND payment_status IN ('pending', 'overdue')",
            )
            .bind(statement_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if moved > 0 {
                Self::enqueue_event(
                    &mut tx,
                    organization_id,
                    crate::webhooks::STATEMENT_STATUS_UPDATED,
                    serde_json::json!({
                        "statementId": statement_id,
                        "period": row.try_get::<String, _>("period")?,
                        "paymentStatus": "suspended",
                    }),
                )
                .await?;
            }
            tx.commit().await?;
            if flagged && suspended_orgs.insert(organization_id) {
                suspended += 1;
            }
        }

        // Reinstatement: only a suspension the sweep itself placed lifts, and
        // only once nothing unpaid stands past its grace — or when the admin
        // switched the automation off (`dunning_grace_days` NULL), which is the
        // operator saying this organization is managed by hand again. A manual
        // suspension (`dunning_suspended_at` NULL) is never the sweep's to undo;
        // a sweep suspension whose org still owes past grace keeps standing.
        let reinstating = sqlx::query(
            "SELECT o.organization_id \
             FROM oxsum.organizations o \
             WHERE o.dunning_suspended_at IS NOT NULL \
               AND (o.dunning_grace_days IS NULL OR NOT EXISTS ( \
                   SELECT 1 FROM oxsum.statements s \
                   WHERE s.organization_id = o.organization_id \
                     AND s.status = 'finalized' \
                     AND s.payment_status IN ('pending', 'overdue', 'suspended') \
                     AND s.due_date IS NOT NULL \
                     AND s.due_date + o.dunning_grace_days < $1))",
        )
        .bind(today)
        .fetch_all(self.pool())
        .await?;
        let mut reinstated = 0i64;
        for row in &reinstating {
            use sqlx::Row;
            let organization_id: Uuid = row.try_get("organization_id")?;
            let mut tx = self.pool().begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || $2::text, 0))")
                .bind(crate::wallet::ORG_ADMISSION_LOCK)
                .bind(organization_id.to_string())
                .execute(&mut *tx)
                .await?;
            let lifted = sqlx::query(
                "UPDATE oxsum.organizations SET suspended_at = NULL, \
                     dunning_suspended_at = NULL \
                 WHERE organization_id = $1 AND dunning_suspended_at IS NOT NULL \
                 RETURNING organization_id",
            )
            .bind(organization_id)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            tx.commit().await?;
            if lifted {
                reinstated += 1;
            }
        }
        Ok((suspended, reinstated))
    }

    /// Flips pending finalized statements whose due date has passed to overdue —
    /// the lazy standing change every read runs first, so a document is right when
    /// looked at without a scheduler. `overdue_at` first-sets: the timestamp of the
    /// answer, not the check.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn flip_overdue(&self, today: Date) -> Result<i64, WalletError> {
        let changed = sqlx::query(
            "UPDATE oxsum.statements SET payment_status = 'overdue', \
                 overdue_at = COALESCE(overdue_at, now()), updated_at = now() \
             WHERE status = 'finalized' AND payment_status = 'pending' \
               AND due_date IS NOT NULL AND due_date < $1",
        )
        .bind(today)
        .execute(self.pool())
        .await?;
        Ok(changed.rows_affected() as i64)
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
