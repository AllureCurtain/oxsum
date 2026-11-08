//! Business staff invitation history and at-most-once SMTP submission facts.
//!
//! The mail row claims the one delivery attempt before transmission so an
//! idempotent create replay never resends; plaintext is never stored. States
//! describe SMTP submission only — `submitted` means the transport accepted
//! the message, not that the recipient received it.
use serde::Serialize;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{BusinessCapability, Db, WalletError, business_staff as authority};

/// How far the one allowed SMTP submission got for an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InvitationDeliveryState {
    /// `sendEmail` was not requested; the link is copy-only.
    NotRequested,
    /// `sendEmail` was requested but the attempt was never claimed.
    NotAttempted,
    /// The configured transport accepted the message — not a delivery receipt.
    Submitted,
    /// `sendEmail` was requested but the server holds no mailer configuration.
    Unavailable,
    /// The attempt was claimed but the outcome is unknown (interrupted,
    /// timed-out or refused transport); it must never trigger an automatic
    /// resend.
    Unknown,
}

/// One staff invitation row in the business's history.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaffInvitationSummary {
    /// Invitation identifier.
    pub id: Uuid,
    /// Email the invitation is bound to.
    pub email: String,
    /// Staff role granted on acceptance.
    pub role: String,
    /// pending, accepted, revoked or expired.
    pub state: String,
    /// When the invitation stops being accepted.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// When the invitation was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When it was accepted, if ever.
    #[serde(with = "time::serde::rfc3339::option")]
    pub accepted_at: Option<OffsetDateTime>,
    /// When it was revoked, if ever.
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    /// SMTP submission state; never a delivery guarantee.
    pub mail_state: InvitationDeliveryState,
    /// When the single attempt was claimed, if ever.
    #[serde(with = "time::serde::rfc3339::option")]
    pub mail_attempted_at: Option<OffsetDateTime>,
    /// When the transport accepted the message, if ever.
    #[serde(with = "time::serde::rfc3339::option")]
    pub mail_submitted_at: Option<OffsetDateTime>,
}

/// A page of staff invitation history.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaffInvitationPage {
    /// Invitations in creation order.
    pub invitations: Vec<StaffInvitationSummary>,
    /// Cursor for the next page, when more history exists.
    pub next_cursor: Option<Uuid>,
}

/// The mail target claimed for transmission, returned to the route layer so it
/// can hand the address and link secret to the mailer.
pub struct StaffMailTarget {
    /// Invitation identifier.
    pub invitation_id: Uuid,
    /// Bound recipient address.
    pub email: String,
}

impl Db {
    /// Lists the business's staff invitations, newest first, with their mail
    /// submission facts. Never returns token material.
    pub async fn staff_invitations(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<StaffInvitationPage, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::StaffManage)
            .await?;
        let limit = limit.clamp(1, 100);
        let rows = sqlx::query(
            "SELECT i.invitation_id,i.intended_email,i.staff_role,i.accepted_at,i.revoked_at,i.expires_at,i.created_at,\
                    m.attempted_at,m.submitted_at \
             FROM oxsum.invitations i \
             LEFT JOIN oxsum.staff_invitation_mail m ON m.invitation_id=i.invitation_id \
             WHERE i.business_id=$1 AND i.kind='staff' AND ($2::uuid IS NULL OR i.invitation_id>$2) \
             ORDER BY i.invitation_id LIMIT $3",
        )
        .bind(business)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut invitations = Vec::with_capacity(rows.len());
        for row in rows.iter().take(limit as usize) {
            let accepted_at = row.try_get::<Option<OffsetDateTime>, _>("accepted_at")?;
            let revoked_at = row.try_get::<Option<OffsetDateTime>, _>("revoked_at")?;
            let expires_at: OffsetDateTime = row.try_get("expires_at")?;
            let live = expires_at > OffsetDateTime::now_utc();
            let state = if accepted_at.is_some() {
                "accepted"
            } else if revoked_at.is_some() {
                "revoked"
            } else if live {
                "pending"
            } else {
                "expired"
            };
            let attempted_at = row.try_get::<Option<OffsetDateTime>, _>("attempted_at")?;
            let submitted_at = row.try_get::<Option<OffsetDateTime>, _>("submitted_at")?;
            let mail_state = if submitted_at.is_some() {
                InvitationDeliveryState::Submitted
            } else if attempted_at.is_some() {
                InvitationDeliveryState::Unknown
            } else {
                InvitationDeliveryState::NotAttempted
            };
            invitations.push(StaffInvitationSummary {
                id: row.try_get("invitation_id")?,
                email: row
                    .try_get::<Option<String>, _>("intended_email")?
                    .unwrap_or_default(),
                role: row
                    .try_get::<Option<String>, _>("staff_role")?
                    .unwrap_or_default(),
                state: state.into(),
                expires_at,
                created_at: row.try_get("created_at")?,
                accepted_at,
                revoked_at,
                mail_state,
                mail_attempted_at: attempted_at,
                mail_submitted_at: submitted_at,
            });
        }
        let next_cursor = (rows.len() as i64 > limit)
            .then(|| invitations.last().map(|i| i.id))
            .flatten();
        tx.commit().await?;
        Ok(StaffInvitationPage {
            invitations,
            next_cursor,
        })
    }

    /// Claims the single SMTP attempt for a staff invitation before
    /// transmission. Returns the target only when this call is the first claim
    /// for a live pending invitation; a replay, a non-staff invitation or an
    /// already-claimed attempt returns `None` so the caller never resends.
    /// When `mail_configured` is false the attempt is still claimed so the
    /// standing invitation reports `unavailable` rather than `notAttempted`.
    pub async fn begin_staff_invitation_mail(
        &self,
        token: &str,
        invitation_id: Uuid,
        secret: &str,
        mail_configured: bool,
    ) -> Result<Option<StaffMailTarget>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::StaffManage)
            .await?;
        let row = sqlx::query(
            "SELECT i.intended_email FROM oxsum.invitations i WHERE i.invitation_id=$1 \
             AND i.business_id=$2 AND i.kind='staff' AND i.token_hash=$3::bytea \
             AND i.accepted_at IS NULL AND i.revoked_at IS NULL AND i.expires_at>clock_timestamp() \
             FOR UPDATE",
        )
        .bind(invitation_id)
        .bind(business)
        .bind(super::invitations::hash_token_pub(secret))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        // Claim the attempt. `ON CONFLICT DO NOTHING` keeps a replay or a
        // concurrent create from claiming a second send.
        let claimed = sqlx::query(
            "INSERT INTO oxsum.staff_invitation_mail (invitation_id,attempted_at,submitted_at) \
             VALUES ($1,CASE WHEN $2 THEN clock_timestamp() ELSE NULL END,NULL) \
             ON CONFLICT (invitation_id) DO NOTHING",
        )
        .bind(invitation_id)
        .bind(mail_configured)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 || !mail_configured {
            tx.commit().await?;
            return Ok(None);
        }
        tx.commit().await?;
        Ok(Some(StaffMailTarget {
            invitation_id,
            email: row
                .try_get::<Option<String>, _>("intended_email")?
                .unwrap_or_default(),
        }))
    }

    /// Marks a claimed submission accepted by the transport. Only the claimed
    /// attempt can complete; an absent row means the claim never happened.
    pub async fn complete_staff_invitation_mail(
        &self,
        invitation_id: Uuid,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.staff_invitation_mail SET submitted_at=clock_timestamp() \
             WHERE invitation_id=$1 AND attempted_at IS NOT NULL",
        )
        .bind(invitation_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Reports whether the secret names a live pending staff invitation. Used
    /// by the redeem route to dispatch a staff token to the staff-only path
    /// without consuming or mutating the invitation.
    pub async fn staff_invitation_pending(&self, token: &str) -> Result<bool, WalletError> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.invitations WHERE token_hash=$1::bytea AND kind='staff' \
             AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>clock_timestamp())",
        )
        .bind(super::invitations::hash_token_pub(token))
        .fetch_one(self.pool())
        .await
        .map_err(crate::WalletError::from)
    }

    /// Reports the standing mail state of one staff invitation so a create
    /// replay answers the same fact without resending.
    pub async fn staff_invitation_mail_state(
        &self,
        token: &str,
        invitation_id: Uuid,
        send_email_requested: bool,
    ) -> Result<InvitationDeliveryState, WalletError> {
        if !send_email_requested {
            return Ok(InvitationDeliveryState::NotRequested);
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::StaffManage)
            .await?;
        let row = sqlx::query(
            "SELECT attempted_at,submitted_at FROM oxsum.staff_invitation_mail WHERE invitation_id=$1",
        )
        .bind(invitation_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(match row {
            Some(r)
                if r.try_get::<Option<OffsetDateTime>, _>("submitted_at")?
                    .is_some() =>
            {
                InvitationDeliveryState::Submitted
            }
            Some(_) => InvitationDeliveryState::Unknown,
            None => InvitationDeliveryState::Unavailable,
        })
    }
}
