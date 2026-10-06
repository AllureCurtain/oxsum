//! Invitation links: how an organization brings in someone who has no account yet.
//!
//! An owner or admin mints a link ([`Db::create_invitation`]); the person holding it
//! registers into the organization ([`Db::redeem_invitation`]), which is the only
//! registration an `invite`-mode deployment allows. A link is valid seven days and
//! usable once — `accepted_at` set means spent — and the database keeps only the
//! token's SHA-256 hash, for the same reason `api_keys` keeps only key hashes: a dump
//! of `oxsum.invitations` redeems nothing.

use std::fmt::Write;

use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::keys;
use crate::orgs::{self, MembershipActor, Role};
use crate::users::{self, NewUser, Registration, User};

/// Every invitation token starts with this, so the value is recognizable in logs.
const TOKEN_MARK: &str = "oxi-";
const TOKEN_BYTES: usize = 32;
/// A link's life: valid seven days from minting, usable once.
const LIFETIME: Duration = Duration::days(7);
/// The role a redeemed invitation grants. Links only ever make members: granting a
/// role is membership management's business, done after the person is in.
const REDEEMED_ROLE: Role = Role::Member;

/// A freshly minted invitation: the only time the token is visible.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedInvitation {
    pub id: Uuid,
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

impl Db {
    /// Mints an invitation link for `organization_id`: `token` is the whole secret —
    /// whoever holds it can register — and it is answered once, here.
    ///
    /// Minting is membership management: the same owner-or-admin rule as
    /// [`Db::add_member`] applies, and the same person-only rule does at the door
    /// (there is no key variant of [`MembershipActor`]).
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for a member; storage failures surface as
    /// [`WalletError`].
    pub async fn create_invitation(
        &self,
        actor: MembershipActor,
        organization_id: Uuid,
    ) -> Result<CreatedInvitation, WalletError> {
        actor.authorized()?;
        let id = Uuid::new_v4();
        let token = generate_token();
        let expires_at: OffsetDateTime = sqlx::query_scalar(
            "INSERT INTO oxsum.invitations \
                 (invitation_id, organization_id, invited_by, token_hash, expires_at) \
             VALUES ($1, $2, $3, $4, now() + $5) \
             RETURNING expires_at",
        )
        .bind(id)
        .bind(organization_id)
        .bind(actor.user_id)
        .bind(hash_token(&token).as_slice())
        .bind(LIFETIME)
        .fetch_one(self.pool())
        .await?;
        Ok(CreatedInvitation {
            id,
            token,
            expires_at,
        })
    }

    /// Registers a user through an invitation: one transaction creates the account,
    /// makes it a `member` of the invitation's organization, spends the link and mints
    /// the first API key for that organization.
    ///
    /// No personal organization is created — the invitation's organization is the
    /// user's first and only membership, so login acts as it. A token that is unknown,
    /// spent or expired is [`WalletError::NotFound`] whichever it is: which of the three
    /// is wrong is the holder's business, and the row is taken `FOR UPDATE` so two
    /// redemptions of one link cannot both win.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unusable token; [`WalletError::InvalidInput`]
    /// for a bad email or password; [`WalletError::Conflict`] when the email is taken
    /// (the invitation stays live — it was never spent); storage failures surface as
    /// [`WalletError`].
    pub async fn redeem_invitation(
        &self,
        token: &str,
        new: NewUser,
    ) -> Result<Registration, WalletError> {
        let email = users::validate_email(&new.email)?;
        let email_normalized = email.to_lowercase();
        users::validate_password(&new.password)?;

        // Argon2 runs before the transaction opens, so the slow hash never holds a
        // connection (the same choice signup makes in `Db::register`).
        let password_hash = password_auth::generate_hash(&new.password);

        let user_id = Uuid::new_v4();
        let mut tx = self.pool().begin().await?;

        // Lock the invitation before spending it: the check and the spend are one
        // statement's row, so a concurrent redemption waits for this transaction and
        // then finds the link spent.
        let invitation = sqlx::query(
            "SELECT invitation_id, organization_id \
             FROM oxsum.invitations \
             WHERE token_hash = $1 AND accepted_at IS NULL AND expires_at > now() \
             FOR UPDATE",
        )
        .bind(hash_token(token).as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(invitation) = invitation else {
            return Err(WalletError::NotFound("the invitation is not valid".into()));
        };
        let invitation_id: Uuid = invitation.try_get("invitation_id")?;
        let organization_id: Uuid = invitation.try_get("organization_id")?;

        sqlx::query(
            "INSERT INTO oxsum.users (user_id, email, email_normalized, password_hash) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(user_id)
        .bind(&email)
        .bind(&email_normalized)
        .bind(&password_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            keys::conflict_or_storage(e, "users_email_normalized_key", "email already registered")
        })?;
        orgs::add_member(&mut tx, organization_id, user_id, REDEEMED_ROLE).await?;
        sqlx::query(
            "UPDATE oxsum.invitations SET accepted_at = now(), accepted_by = $1 \
             WHERE invitation_id = $2",
        )
        .bind(user_id)
        .bind(invitation_id)
        .execute(&mut *tx)
        .await?;
        let api_key = keys::insert(
            &mut tx,
            organization_id,
            Some("default".to_owned()),
            None,
            Some(user_id),
            keys::KeyConstraints::default(),
        )
        .await?;
        tx.commit().await?;

        // The organization row the response carries: it exists — the invitation's
        // foreign key insists — so a miss here is storage, not a state.
        let row = sqlx::query(
            "SELECT organization_id, name, tenant_id, kind, created_at \
             FROM oxsum.organizations WHERE organization_id = $1",
        )
        .bind(organization_id)
        .fetch_one(self.pool())
        .await?;
        let organization = orgs::organization_from_row(&row)?;

        Ok(Registration {
            user: User { id: user_id, email },
            organization,
            api_key,
        })
    }
}

/// A new invitation token: the mark plus 32 random bytes from the operating system's
/// CSPRNG, hex-encoded like an API key secret or a session token.
fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(TOKEN_MARK.len() + TOKEN_BYTES * 2);
    token.push_str(TOKEN_MARK);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(token, "{byte:02x}");
    }
    token
}

/// SHA-256 of the token, the only form the database ever holds.
fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
