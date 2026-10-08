//! The admin audit log (roadmap P7-2, issue #160).
//!
//! One append-only row per mutating `/api/v1/admin` call: the action as a
//! dotted name, what it acted on, the request's detail with credentials
//! redacted, and the idempotency key it carried. The route layer writes the
//! row *after* the mutation commits — the log may omit on a crash, but it
//! never describes a change that did not happen.
//!
//! The ledger remains the proof for the money an adjustment or statement
//! moved; this table is the operator's index over every admin write,
//! including the ones no ledger entry describes — a tier written, a discount
//! granted, a channel's upstream repointed.

use serde::Serialize;
use sqlx::Row as _;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// One audit row, as the endpoint answers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub audit_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// Who acted — `"operator"` today; the single admin token is the only
    /// admin identity, and the column is what a multi-identity admin surface
    /// would answer with instead.
    pub actor: String,
    /// The mutating call, dotted: `channel.set`, `discount.create`, …
    pub action: String,
    /// What the action acted on; `None` when the call names nothing.
    pub target: Option<String>,
    /// The request fields safe to keep — never a credential.
    pub detail: serde_json::Value,
    /// The idempotency key the request carried, when it did.
    pub idempotency_key: Option<String>,
}

/// The action names the admin surface writes under. Dotted and bounded — a
/// typo'd name is a 400, not a row that says nothing recognizable.
pub mod action {
    pub const CHANNEL_SET: &str = "channel.set";
    pub const CHANNEL_PRICE_APPEND: &str = "channel.price.append";
    pub const ORGANIZATION_ADJUST: &str = "organization.adjust";
    pub const ORGANIZATION_UPDATE: &str = "organization.update";
    pub const TIER_SET: &str = "tier.set";
    pub const TIER_DELETE: &str = "tier.delete";
    pub const DISCOUNT_CREATE: &str = "discount.create";
    pub const DISCOUNT_END: &str = "discount.end";
    pub const USER_PASSWORD_RESET: &str = "user.password_reset";
    pub const CODES_MINT: &str = "codes.mint";
    pub const CLOSING_CLOSE: &str = "closing.close";
    pub const STATEMENT_GENERATE: &str = "statement.generate";
    pub const STATEMENT_FINALIZE: &str = "statement.finalize";
    pub const STATEMENT_PAYMENT: &str = "statement.payment";
    pub const STATEMENT_SUSPEND: &str = "statement.suspend";

    /// Every action the surface writes — the `action` filter's vocabulary.
    pub const ALL: &[&str] = &[
        CHANNEL_SET,
        CHANNEL_PRICE_APPEND,
        ORGANIZATION_ADJUST,
        ORGANIZATION_UPDATE,
        TIER_SET,
        TIER_DELETE,
        DISCOUNT_CREATE,
        DISCOUNT_END,
        USER_PASSWORD_RESET,
        CODES_MINT,
        CLOSING_CLOSE,
        STATEMENT_GENERATE,
        STATEMENT_FINALIZE,
        STATEMENT_PAYMENT,
        STATEMENT_SUSPEND,
    ];
}

/// One page of the log, newest first.
#[derive(Debug)]
pub struct AuditPage {
    pub rows: Vec<AuditEntry>,
    /// Where the next page resumes; `None` at the log's end.
    pub next_cursor: Option<(OffsetDateTime, Uuid)>,
}

fn entry_from_row(row: &sqlx::postgres::PgRow) -> Result<AuditEntry, WalletError> {
    Ok(AuditEntry {
        audit_id: row.try_get("audit_id")?,
        recorded_at: row.try_get("recorded_at")?,
        actor: row.try_get("actor")?,
        action: row.try_get("action")?,
        target: row.try_get("target")?,
        detail: row.try_get("detail")?,
        idempotency_key: row.try_get("idempotency_key")?,
    })
}

impl Db {
    /// Appends one audit row. Called by the admin route after the mutation it
    /// describes has committed — never before, so the log cannot describe a
    /// change that failed to happen.
    ///
    /// A keyed write's exact replay is the same logical event: the
    /// `(action, idempotency_key)` unique index absorbs it, and the call answers
    /// `None` rather than a second row.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_audit(
        &self,
        action: &str,
        target: Option<&str>,
        detail: serde_json::Value,
        idempotency_key: Option<&str>,
    ) -> Result<Option<AuditEntry>, WalletError> {
        let row = sqlx::query(
            "INSERT INTO oxsum.admin_audit (action, target, detail, idempotency_key) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT DO NOTHING \
             RETURNING audit_id, recorded_at, actor, action, target, detail, idempotency_key",
        )
        .bind(action)
        .bind(target)
        .bind(detail)
        .bind(idempotency_key)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(entry_from_row).transpose()
    }

    /// One page of the log, newest first — `after` is the cursor position the
    /// previous page ended at, and `action` narrows to one action's rows.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn audit_page(
        &self,
        after: Option<(OffsetDateTime, Uuid)>,
        action: Option<&str>,
        limit: usize,
    ) -> Result<AuditPage, WalletError> {
        // One row over the page size is the "there is more" signal — the
        // cursor names the page's last row, not the peeked one.
        let limit = limit.clamp(1, 100) as i64;
        let rows =
            match (after, action) {
                (Some((at, id)), Some(action)) => sqlx::query(
                    "SELECT audit_id, recorded_at, actor, action, target, detail, idempotency_key \
                     FROM oxsum.admin_audit \
                     WHERE action = $1 AND (recorded_at, audit_id) < ($2, $3) \
                     ORDER BY recorded_at DESC, audit_id DESC LIMIT $4",
                )
                .bind(action)
                .bind(at)
                .bind(id)
                .bind(limit + 1)
                .fetch_all(self.pool())
                .await?,
                (Some((at, id)), None) => sqlx::query(
                    "SELECT audit_id, recorded_at, actor, action, target, detail, idempotency_key \
                     FROM oxsum.admin_audit \
                     WHERE (recorded_at, audit_id) < ($1, $2) \
                     ORDER BY recorded_at DESC, audit_id DESC LIMIT $3",
                )
                .bind(at)
                .bind(id)
                .bind(limit + 1)
                .fetch_all(self.pool())
                .await?,
                (None, Some(action)) => sqlx::query(
                    "SELECT audit_id, recorded_at, actor, action, target, detail, idempotency_key \
                     FROM oxsum.admin_audit \
                     WHERE action = $1 \
                     ORDER BY recorded_at DESC, audit_id DESC LIMIT $2",
                )
                .bind(action)
                .bind(limit + 1)
                .fetch_all(self.pool())
                .await?,
                (None, None) => sqlx::query(
                    "SELECT audit_id, recorded_at, actor, action, target, detail, idempotency_key \
                     FROM oxsum.admin_audit \
                     ORDER BY recorded_at DESC, audit_id DESC LIMIT $1",
                )
                .bind(limit + 1)
                .fetch_all(self.pool())
                .await?,
            };
        let more = rows.len() as i64 > limit;
        let entries = rows
            .iter()
            .take(limit as usize)
            .map(entry_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = more
            .then(|| entries.last())
            .flatten()
            .map(|entry| (entry.recorded_at, entry.audit_id));
        Ok(AuditPage {
            rows: entries,
            next_cursor,
        })
    }
}
