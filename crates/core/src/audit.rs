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
    /// Audit row identifier.
    pub audit_id: Uuid,
    /// When the row was written.
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
    /// A team invitation was created.
    pub const INVITATION_TEAM_CREATE: &str = "invitation.team.create";
    /// A customer invitation was created.
    pub const INVITATION_CUSTOMER_CREATE: &str = "business.customer.invitation.create";
    /// An invitation was accepted.
    pub const INVITATION_ACCEPT: &str = "invitation.accept";
    /// An invitation was revoked.
    pub const INVITATION_REVOKE: &str = "invitation.revoke";
    /// A session switched its authority context.
    pub const SESSION_CONTEXT: &str = "session.context";
    /// A business customer's profile was updated.
    pub const BUSINESS_CUSTOMER_UPDATE: &str = "business.customer.update";
    /// A business customer's lifecycle changed.
    pub const BUSINESS_CUSTOMER_LIFECYCLE: &str = "business.customer.lifecycle";
    /// A business customer was suspended or restored.
    pub const BUSINESS_CUSTOMER_SUSPENSION: &str = "business.customer.suspension";
    /// A business staff role was set.
    pub const BUSINESS_STAFF_SET: &str = "business.staff.set";
    /// A business integration was created.
    pub const BUSINESS_INTEGRATION_CREATE: &str = "business.integration.create";
    /// A business integration was revoked.
    pub const BUSINESS_INTEGRATION_REVOKE: &str = "business.integration.revoke";
    /// A business staff seat was revoked.
    pub const BUSINESS_STAFF_REVOKE: &str = "business.staff.revoke";
    /// A staff member's account was deleted.
    pub const BUSINESS_STAFF_ACCOUNT_DELETED: &str = "business.staff.account_deleted";
    /// Business ownership was transferred.
    pub const BUSINESS_OWNERSHIP_TRANSFER: &str = "business.ownership.transfer";
    /// The business registration mode was set.
    pub const BUSINESS_REGISTRATION_SET: &str = "business.registration.set";
    /// The business was initialized.
    pub const BUSINESS_INITIALIZED: &str = "business.initialized";
    /// A business setup step was authorized.
    pub const BUSINESS_SETUP_AUTHORIZED: &str = "business.setup.authorized";
    /// A channel connection was written.
    pub const CHANNEL_SET: &str = "channel.set";
    /// A channel price version was appended.
    pub const CHANNEL_PRICE_APPEND: &str = "channel.price.append";
    /// A channel alias was set.
    pub const CHANNEL_ALIAS_SET: &str = "channel.alias.set";
    /// A channel alias was deleted.
    pub const CHANNEL_ALIAS_DELETE: &str = "channel.alias.delete";
    /// A channel credential was added.
    pub const CHANNEL_CREDENTIAL_ADD: &str = "channel.credential.add";
    /// A channel credential's status changed.
    pub const CHANNEL_CREDENTIAL_STATUS: &str = "channel.credential.status";
    /// A channel credential was dropped.
    pub const CHANNEL_CREDENTIAL_DROP: &str = "channel.credential.drop";
    /// A manual receipt was authorized.
    pub const MANUAL_RECEIPT_AUTHORIZED: &str = "manual_receipt.authorized";
    /// An organization balance was adjusted.
    pub const ORGANIZATION_ADJUST: &str = "organization.adjust";
    /// A credit note was issued.
    pub const CREDIT_NOTE_ISSUE: &str = "credit_note.issue";
    /// An organization was updated.
    pub const ORGANIZATION_UPDATE: &str = "organization.update";
    /// A pricing tier was set.
    pub const TIER_SET: &str = "tier.set";
    /// A pricing tier was deleted.
    pub const TIER_DELETE: &str = "tier.delete";
    /// A discount was created.
    pub const DISCOUNT_CREATE: &str = "discount.create";
    /// A discount was ended.
    pub const DISCOUNT_END: &str = "discount.end";
    /// A user's password was reset.
    pub const USER_PASSWORD_RESET: &str = "user.password_reset";
    /// Redemption codes were minted.
    pub const CODES_MINT: &str = "codes.mint";
    /// A deposit was reversed.
    pub const DEPOSIT_REVERSE: &str = "deposit.reverse";
    /// A period closing was closed.
    pub const CLOSING_CLOSE: &str = "closing.close";
    /// A statement was generated.
    pub const STATEMENT_GENERATE: &str = "statement.generate";
    /// A statement was finalized.
    pub const STATEMENT_FINALIZE: &str = "statement.finalize";
    /// A statement payment was recorded.
    pub const STATEMENT_PAYMENT: &str = "statement.payment";
    /// A statement was suspended.
    pub const STATEMENT_SUSPEND: &str = "statement.suspend";
    /// A service credential was minted.
    pub const SERVICE_CREDENTIAL_MINT: &str = "service_credential.mint";
    /// A service credential was revoked.
    pub const SERVICE_CREDENTIAL_REVOKE: &str = "service_credential.revoke";
    /// A subscription plan was set.
    pub const SUBSCRIPTION_PLAN_SET: &str = "subscription.plan.set";
    /// A subscription was bound.
    pub const SUBSCRIPTION_BIND: &str = "subscription.bind";
    /// A subscription was cancelled.
    pub const SUBSCRIPTION_CANCEL: &str = "subscription.cancel";
    /// A guardrail policy was set.
    pub const GUARDRAIL_POLICY_SET: &str = "guardrail.policy.set";
    /// A guardrail policy was deleted.
    pub const GUARDRAIL_POLICY_DELETE: &str = "guardrail.policy.delete";
    /// A guardrail binding was set.
    pub const GUARDRAIL_BINDING_SET: &str = "guardrail.binding.set";
    /// A guardrail binding was deleted.
    pub const GUARDRAIL_BINDING_DELETE: &str = "guardrail.binding.delete";
    /// A model fallback list was set.
    pub const MODEL_FALLBACK_SET: &str = "model_fallback.set";
    /// A model fallback list was deleted.
    pub const MODEL_FALLBACK_DELETE: &str = "model_fallback.delete";
    /// A group was set.
    pub const GROUP_SET: &str = "group.set";
    /// A group was deleted.
    pub const GROUP_DELETE: &str = "group.delete";

    /// Every action the surface writes — the `action` filter's vocabulary.
    pub const ALL: &[&str] = &[
        BUSINESS_INTEGRATION_CREATE,
        BUSINESS_INTEGRATION_REVOKE,
        INVITATION_TEAM_CREATE,
        INVITATION_CUSTOMER_CREATE,
        INVITATION_ACCEPT,
        INVITATION_REVOKE,
        SESSION_CONTEXT,
        BUSINESS_CUSTOMER_UPDATE,
        BUSINESS_CUSTOMER_LIFECYCLE,
        BUSINESS_CUSTOMER_SUSPENSION,
        BUSINESS_STAFF_SET,
        BUSINESS_STAFF_REVOKE,
        BUSINESS_STAFF_ACCOUNT_DELETED,
        BUSINESS_OWNERSHIP_TRANSFER,
        BUSINESS_REGISTRATION_SET,
        BUSINESS_INITIALIZED,
        BUSINESS_SETUP_AUTHORIZED,
        CHANNEL_SET,
        CHANNEL_PRICE_APPEND,
        CHANNEL_ALIAS_SET,
        CHANNEL_ALIAS_DELETE,
        CHANNEL_CREDENTIAL_ADD,
        CHANNEL_CREDENTIAL_STATUS,
        CHANNEL_CREDENTIAL_DROP,
        MANUAL_RECEIPT_AUTHORIZED,
        ORGANIZATION_ADJUST,
        ORGANIZATION_UPDATE,
        TIER_SET,
        TIER_DELETE,
        DISCOUNT_CREATE,
        DISCOUNT_END,
        USER_PASSWORD_RESET,
        CODES_MINT,
        DEPOSIT_REVERSE,
        CLOSING_CLOSE,
        STATEMENT_GENERATE,
        STATEMENT_FINALIZE,
        STATEMENT_PAYMENT,
        STATEMENT_SUSPEND,
        SERVICE_CREDENTIAL_MINT,
        SERVICE_CREDENTIAL_REVOKE,
        SUBSCRIPTION_PLAN_SET,
        SUBSCRIPTION_BIND,
        SUBSCRIPTION_CANCEL,
        GUARDRAIL_POLICY_SET,
        GUARDRAIL_POLICY_DELETE,
        GUARDRAIL_BINDING_SET,
        GUARDRAIL_BINDING_DELETE,
        MODEL_FALLBACK_SET,
        MODEL_FALLBACK_DELETE,
        GROUP_SET,
        GROUP_DELETE,
    ];
}

/// One page of the log, newest first.
#[derive(Debug)]
pub struct AuditPage {
    /// Audit rows in this page, newest first.
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
