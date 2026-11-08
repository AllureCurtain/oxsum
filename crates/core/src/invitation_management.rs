//! Business invitation history and at-most-once SMTP submission facts.

use crate::{BusinessCapability, Db, WalletError, business_staff as authority};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// Local SMTP submission fact; it never claims receipt by the intended person.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InvitationDeliveryState {
    /// The creator chose a copy link without mail.
    NotRequested,
    /// Requested, but no SMTP attempt was claimed before interruption.
    NotAttempted,
    /// SMTP transport accepted the message; recipient delivery is unverified.
    Submitted,
    /// No mailer was configured when the request was created.
    Unavailable,
    /// Claimed attempt may have sent; no automatic replay is safe.
    Unknown,
}
impl InvitationDeliveryState {
    fn parse(value: &str) -> Result<Self, WalletError> {
        match value {
            "not_requested" => Ok(Self::NotRequested),
            "not_attempted" => Ok(Self::NotAttempted),
            "submitted" => Ok(Self::Submitted),
            "unavailable" => Ok(Self::Unavailable),
            "unknown" => Ok(Self::Unknown),
            _ => Err(WalletError::Misconfigured(
                "invalid invitation delivery state".into(),
            )),
        }
    }
}

/// Safe invitation record. The token and its hash never appear in history.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerInvitationSummary {
    /// Invitation identity.
    pub id: Uuid,
    /// Reserved customer identity, also retained after acceptance.
    pub organization_id: Uuid,
    /// Bound, normalized recipient address.
    pub email: String,
    /// Reserved customer display name.
    pub name: String,
    /// Stable external customer identifier.
    pub external_id: String,
    /// Pending, accepted, revoked or expired, derived at the database clock.
    pub state: String,
    /// Seven-day acceptance deadline.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// When the invitation was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Recipient consent time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub accepted_at: Option<OffsetDateTime>,
    /// Revocation time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    /// SMTP submission result, independent of invitation acceptance.
    pub mail_state: InvitationDeliveryState,
    /// When the single attempt was claimed, before transmission.
    #[serde(with = "time::serde::rfc3339::option")]
    pub mail_attempted_at: Option<OffsetDateTime>,
    /// When the SMTP submission returned success.
    #[serde(with = "time::serde::rfc3339::option")]
    pub mail_submitted_at: Option<OffsetDateTime>,
}

/// Cursor page and the current saved registration policy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerInvitationPage {
    /// Customer invitation history in newest-first order.
    pub invitations: Vec<CustomerInvitationSummary>,
    /// Scoped cursor for the next page.
    pub next_cursor: Option<Uuid>,
    /// Business policy, read under the same authority as the history.
    pub registration_mode: String,
}

/// One claimed SMTP attempt. Its identity is created only by the authorization
/// method; callers cannot manufacture a completion token for another invite.
#[derive(Debug)]
pub struct InvitationMailTarget {
    id: Uuid,
    /// Recipient read from the immutable invitation, never caller input.
    pub email: String,
    /// Reserved customer name for the invitation body.
    pub name: String,
}

impl Db {
    /// List safe customer invitations under the current staff read capability.
    pub async fn customer_invitations(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<CustomerInvitationPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput("limit must be 1..100".into()));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::CustomersRead)
            .await?;
        if let Some(after) = after {
            let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.invitations WHERE invitation_id=$1 AND business_id=$2 AND kind='customer')")
                .bind(after).bind(business).fetch_one(&mut *tx).await?;
            if !valid {
                return Err(WalletError::NotFound(
                    "unknown customer invitation cursor".into(),
                ));
            }
        }
        let rows = sqlx::query("SELECT invitation_id,pending_customer_id,intended_email,customer_name,external_id,expires_at,created_at,accepted_at,revoked_at,mail_state,mail_attempted_at,mail_submitted_at,CASE WHEN accepted_at IS NOT NULL THEN 'accepted' WHEN revoked_at IS NOT NULL THEN 'revoked' WHEN expires_at<=clock_timestamp() THEN 'expired' ELSE 'pending' END AS state FROM oxsum.invitations WHERE business_id=$1 AND kind='customer' AND ($2::uuid IS NULL OR (created_at,invitation_id)<(SELECT created_at,invitation_id FROM oxsum.invitations WHERE invitation_id=$2 AND business_id=$1 AND kind='customer')) ORDER BY created_at DESC,invitation_id DESC LIMIT $3")
            .bind(business).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut invitations = rows
            .iter()
            .map(|row| {
                Ok(CustomerInvitationSummary {
                    id: row.try_get("invitation_id")?,
                    organization_id: row.try_get("pending_customer_id")?,
                    email: row.try_get("intended_email")?,
                    name: row.try_get("customer_name")?,
                    external_id: row.try_get("external_id")?,
                    state: row.try_get("state")?,
                    expires_at: row.try_get("expires_at")?,
                    created_at: row.try_get("created_at")?,
                    accepted_at: row.try_get("accepted_at")?,
                    revoked_at: row.try_get("revoked_at")?,
                    mail_state: InvitationDeliveryState::parse(
                        &row.try_get::<String, _>("mail_state")?,
                    )?,
                    mail_attempted_at: row.try_get("mail_attempted_at")?,
                    mail_submitted_at: row.try_get("mail_submitted_at")?,
                })
            })
            .collect::<Result<Vec<_>, WalletError>>()?;
        let more = invitations.len() > limit as usize;
        invitations.truncate(limit as usize);
        let next_cursor = if more {
            invitations.last().map(|item| item.id)
        } else {
            None
        };
        let registration_mode = sqlx::query_scalar(
            "SELECT registration_mode FROM oxsum.businesses WHERE business_id=$1",
        )
        .bind(business)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(CustomerInvitationPage {
            invitations,
            next_cursor,
            registration_mode,
        })
    }

    /// Claim one delivery attempt before any SMTP write. Only a live request
    /// with its original token and current staff authority can claim it.
    pub async fn begin_customer_invitation_mail(
        &self,
        token: &str,
        invitation: Uuid,
        secret: &str,
        configured: bool,
    ) -> Result<Option<InvitationMailTarget>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::CustomersWrite)
            .await?;
        let row=sqlx::query("UPDATE oxsum.invitations SET mail_state=$4,mail_attempted_at=CASE WHEN $5 THEN clock_timestamp() ELSE NULL END WHERE invitation_id=$1 AND business_id=$2 AND kind='customer' AND token_hash=$3 AND mail_state='not_attempted' AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>clock_timestamp() RETURNING intended_email,customer_name")
            .bind(invitation).bind(business).bind(Sha256::digest(secret.as_bytes()).to_vec()).bind(if configured {"unknown"} else {"unavailable"}).bind(configured).fetch_optional(&mut *tx).await?;
        let target = if configured {
            row.as_ref()
                .map(|row| {
                    Ok::<_, WalletError>(InvitationMailTarget {
                        id: invitation,
                        email: row.try_get("intended_email")?,
                        name: row.try_get("customer_name")?,
                    })
                })
                .transpose()?
        } else {
            None
        };
        tx.commit().await?;
        Ok(target)
    }

    /// Record transport acceptance for the one previously claimed attempt.
    /// Unknown remains the safe result if this update or the transport fails.
    pub async fn complete_customer_invitation_mail(
        &self,
        target: &InvitationMailTarget,
    ) -> Result<(), WalletError> {
        sqlx::query("UPDATE oxsum.invitations SET mail_state='submitted',mail_submitted_at=clock_timestamp() WHERE invitation_id=$1 AND mail_state='unknown'")
            .bind(target.id).execute(self.pool()).await?;
        Ok(())
    }

    /// Read a single invitation's submission fact under staff authority.
    pub async fn customer_invitation_mail_state(
        &self,
        token: &str,
        invitation: Uuid,
    ) -> Result<InvitationDeliveryState, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::CustomersWrite)
            .await?;
        let state:String=sqlx::query_scalar("SELECT mail_state FROM oxsum.invitations WHERE invitation_id=$1 AND business_id=$2 AND kind='customer'")
            .bind(invitation).bind(business).fetch_optional(&mut *tx).await?.ok_or_else(||WalletError::NotFound("unknown customer invitation".into()))?;
        tx.commit().await?;
        InvitationDeliveryState::parse(&state)
    }
}
