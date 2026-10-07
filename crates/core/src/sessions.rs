//! Web sessions: the second credential next to the API key.
//!
//! A request may carry `Authorization: Bearer oxs-…` (an organization's key) or the session
//! cookie. Both resolve to a [`Principal`]: a key acts *as* the organization, a session as the
//! user, the organization they act as, and their role in it.
//!
//! The cookie value is `oxsess-` plus 32 random bytes; the database keeps only its SHA-256
//! hash, so a dump of `oxsum.sessions` authenticates nothing — the same reason `api_keys`
//! keeps only key hashes. Resolution joins `memberships` on `(user_id, organization_id)`, so
//! a session stops authenticating the moment its membership is gone: the row cannot outlive
//! the authorization it names. See docs/product.md, "Roles".

use std::fmt::Write as _;
use std::sync::LazyLock;
use std::time::Duration;

use password_auth::verify_password;
use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::keys::ActingKey;
use crate::orgs::{self, MembershipActor, Organization, Role};
use crate::users::User;

/// The session cookie's name. Carried in `openapi.yaml` as the `sessionCookie` scheme.
pub const SESSION_COOKIE: &str = "oxsum_session";
/// Every session token starts with this, so the value is recognizable in logs and dumps.
const TOKEN_MARK: &str = "oxsess-";
/// 32 random bytes, the same strength as an API key secret.
const TOKEN_BYTES: usize = 32;
/// How long a login stays valid: absolute expiry, no sliding renewal in v1.
pub const SESSION_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);
/// `last_used_at` is touched at most this often: the column stays meaningful without a write
/// on every authenticated request.
const LAST_USED_TOUCH: Duration = Duration::from_secs(5 * 60);

/// Who acts: an organization's API key, or a logged-in user acting as their organization.
#[derive(Debug, Clone)]
pub enum Principal {
    /// A machine credential: acts as the organization, with the organization's full authority.
    /// Pre-session behaviour, deliberately unchanged — a key is not a person. Carries which
    /// key acted, so the hold path can attribute spend to it and enforce its spend limit.
    Key(KeyPrincipal),
    /// A person: the user, the organization they act as, and their role in it.
    Session(SessionPrincipal),
}

/// The organization a key acts as, plus the key that acted.
#[derive(Debug, Clone)]
pub struct KeyPrincipal {
    pub organization: Organization,
    pub key: ActingKey,
}

impl Principal {
    /// The organization the request acts for, whichever credential named it.
    #[must_use]
    pub fn organization(&self) -> &Organization {
        match self {
            Self::Key(key) => &key.organization,
            Self::Session(session) => &session.organization,
        }
    }

    /// The API key that acted, if the credential was a key rather than a session.
    #[must_use]
    pub fn acting_key(&self) -> Option<&ActingKey> {
        match self {
            Self::Key(key) => Some(&key.key),
            Self::Session(_) => None,
        }
    }

    /// The acting user, if a person is acting rather than a key.
    #[must_use]
    pub fn user_id(&self) -> Option<Uuid> {
        match self {
            Self::Key(_) => None,
            Self::Session(session) => Some(session.user.id),
        }
    }

    /// The acting user itself, if a person is acting rather than a key — the
    /// email flows need the address and its verified state, not just the id.
    #[must_use]
    pub fn user(&self) -> Option<&User> {
        match self {
            Self::Key(_) => None,
            Self::Session(session) => Some(&session.user),
        }
    }

    /// What the principal may do with the organization's keys (docs/product.md, "Roles").
    ///
    /// Roles constrain sessions, not keys: a key keeps acting as the whole organization,
    /// while a user acts with the authority their role grants them.
    #[must_use]
    pub fn key_scope(&self) -> KeyScope {
        match self {
            Self::Key(_) => KeyScope::Organization,
            Self::Session(session) => match session.role {
                Role::Owner | Role::Admin => KeyScope::All,
                Role::Member => KeyScope::Own(session.user.id),
            },
        }
    }

    /// Who manages the organization's memberships, if this credential may at all.
    ///
    /// The one place a key does *not* act with the organization's full authority: managing
    /// members is a person's action, and a key is not a person and names no role. A session
    /// carries the person and their role, and the rules in `crates/core/src/orgs.rs` refuse a
    /// member; a key is refused here, before any rule is reached (docs/decisions.md,
    /// "membership management is a person's action").
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for an API key.
    pub fn membership_actor(&self) -> Result<MembershipActor, WalletError> {
        match self {
            Self::Key(_) => Err(WalletError::Forbidden(
                "managing members needs a logged-in owner or admin: an API key is not a person \
                 and names no role"
                    .into(),
            )),
            Self::Session(session) => Ok(MembershipActor {
                user_id: session.user.id,
                role: session.role,
            }),
        }
    }
}

/// How much of an organization's key inventory a principal may see and touch.
#[derive(Debug, Clone, Copy)]
pub enum KeyScope {
    /// The organization itself (an API key acting as it): every key, as before sessions.
    Organization,
    /// An owner or admin: every key of the organization.
    All,
    /// A member: only the keys they created.
    Own(Uuid),
}

/// The user behind a session cookie, resolved at request time.
#[derive(Debug, Clone)]
pub struct SessionPrincipal {
    pub session: Session,
    pub user: User,
    pub organization: Organization,
    pub role: Role,
}

/// A session as the API presents it. Never the token, never the hash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_used_at: OffsetDateTime,
}

/// A session plus its token, returned exactly once: at login.
#[derive(Debug, Clone)]
pub struct CreatedSession {
    pub principal: SessionPrincipal,
    pub token: String,
}

/// An argon2 hash of a password nobody knows, for the unknown-email login path: verifying
/// against it costs the same as verifying a real password, so the two failures take the
/// same time. The value is public; only its cost matters.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| password_auth::generate_hash("oxsum has no such user"));

impl Db {
    /// Logs a user in: verifies the password, then mints a session for the organization the
    /// user acts as.
    ///
    /// An unknown email and a wrong password fail identically — same error, same message —
    /// and the unknown-email path still pays for one argon2 verification, so neither timing
    /// nor wording reveals whether an account exists. The hash runs before the session row
    /// is written, and outside any transaction, so a slow hash never holds a connection.
    pub async fn login(&self, email: &str, password: &str) -> Result<CreatedSession, WalletError> {
        let normalized = email.trim().to_lowercase();
        let row = sqlx::query(
            "SELECT user_id, email, password_hash, \
             (email_verified_at IS NOT NULL) AS email_verified \
             FROM oxsum.users WHERE email_normalized = $1",
        )
        .bind(&normalized)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            // No such account: burn one hash verification anyway, then fail like a wrong
            // password. The result is discarded; the cost is the point.
            let _: Result<(), _> = verify_password(password, DUMMY_HASH.as_str());
            return Err(WalletError::InvalidCredentials);
        };
        let user = User {
            id: row.try_get("user_id")?,
            email: row.try_get("email")?,
            email_verified: row.try_get("email_verified")?,
        };
        let password_hash: String = row.try_get("password_hash")?;
        if verify_password(password, &password_hash).is_err() {
            return Err(WalletError::InvalidCredentials);
        }
        self.session_for(user.id).await
    }

    /// Mints a session for a user whose credential was already checked — the
    /// password path's tail, and the whole of an OAuth login's second half
    /// (issue #152).
    ///
    /// The organization the session acts as is the user's oldest membership.
    /// A user with no membership cannot produce this state through any flow,
    /// so `InvalidCredentials` stands in — no better answer exists for it.
    ///
    /// # Errors
    ///
    /// `WalletError::InvalidCredentials` for a memberless user; storage failures
    /// surface as `WalletError`.
    pub(crate) async fn session_for(&self, user_id: Uuid) -> Result<CreatedSession, WalletError> {
        let row = sqlx::query(
            "SELECT email, (email_verified_at IS NOT NULL) AS email_verified \
             FROM oxsum.users WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        let user = User {
            id: user_id,
            email: row.try_get("email")?,
            email_verified: row.try_get("email_verified")?,
        };
        let row = sqlx::query(
            "SELECT m.role, o.organization_id, o.name, o.tenant_id, o.kind, o.created_at \
             FROM oxsum.memberships m JOIN oxsum.organizations o USING (organization_id) \
             WHERE m.user_id = $1 ORDER BY m.created_at, m.organization_id LIMIT 1",
        )
        .bind(user.id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(WalletError::InvalidCredentials)?;
        let role = Role::parse(&row.try_get::<String, _>("role")?)?;
        let organization = orgs::organization_from_row(&row)?;

        let token = generate_token();
        let session_id = Uuid::new_v4();
        let expires_at = OffsetDateTime::now_utc() + SESSION_LIFETIME;
        let row = sqlx::query(
            "INSERT INTO oxsum.sessions \
                 (session_id, user_id, organization_id, token_hash, expires_at) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING created_at, last_used_at",
        )
        .bind(session_id)
        .bind(user.id)
        .bind(organization.id)
        .bind(hash_token(&token).as_slice())
        .bind(expires_at)
        .fetch_one(self.pool())
        .await?;
        let session = Session {
            id: session_id,
            created_at: row.try_get("created_at")?,
            expires_at,
            last_used_at: row.try_get("last_used_at")?,
        };
        Ok(CreatedSession {
            principal: SessionPrincipal {
                session,
                user,
                organization,
                role,
            },
            token,
        })
    }

    /// Resolves a session cookie to the user acting through it.
    ///
    /// `None` covers every way a session can fail — unknown token, revoked, expired, or a
    /// membership that no longer exists — so a probe cannot tell them apart. The membership
    /// join is the point: a session stops authenticating the moment its membership is gone.
    pub async fn authenticate_session(
        &self,
        token: &str,
    ) -> Result<Option<SessionPrincipal>, WalletError> {
        let hash = hash_token(token);
        let row = sqlx::query(
            "SELECT s.session_id, s.created_at, s.expires_at, s.last_used_at, \
                    u.user_id, u.email, (u.email_verified_at IS NOT NULL) AS email_verified, \
                    o.organization_id, o.name, o.tenant_id, o.kind, o.created_at AS org_created_at, \
                    m.role \
             FROM oxsum.sessions s \
             JOIN oxsum.memberships m \
               ON m.user_id = s.user_id AND m.organization_id = s.organization_id \
             JOIN oxsum.users u ON u.user_id = s.user_id \
             JOIN oxsum.organizations o ON o.organization_id = s.organization_id \
             WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()",
        )
        .bind(hash.as_slice())
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        let session_id: Uuid = row.try_get("session_id")?;
        touch_last_used(self.pool(), session_id).await?;
        // `organization_from_row` reads `created_at`; the join renames the organization's
        // column to `org_created_at`, so it is read back under its own name here.
        let organization = Organization {
            id: row.try_get("organization_id")?,
            name: row.try_get("name")?,
            tenant_id: row.try_get("tenant_id")?,
            kind: orgs::Kind::parse(&row.try_get::<String, _>("kind")?)?,
            created_at: row.try_get("org_created_at")?,
        };
        Ok(Some(SessionPrincipal {
            session: Session {
                id: session_id,
                created_at: row.try_get("created_at")?,
                expires_at: row.try_get("expires_at")?,
                last_used_at: row.try_get("last_used_at")?,
            },
            user: User {
                id: row.try_get("user_id")?,
                email: row.try_get("email")?,
                email_verified: row.try_get("email_verified")?,
            },
            organization,
            role: Role::parse(&row.try_get::<String, _>("role")?)?,
        }))
    }

    /// Logs out: revokes the session the cookie names.
    ///
    /// Idempotent: revoking twice is not an error, and neither is naming a session that is
    /// already gone — the first revocation's timestamp stands.
    pub async fn logout(&self, token: &str) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.sessions SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE token_hash = $1",
        )
        .bind(hash_token(token).as_slice())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Switches the organization `session_id` acts as: the session row is updated, so
    /// the choice survives reloads, new tabs and every request after this one.
    ///
    /// The target must be an organization `user_id` is a member of — the membership
    /// supplies both the proof and the role the session now holds. A missing
    /// membership is [`WalletError::NotFound`] whatever the cause, so the answer does
    /// not say whether the organization exists at all.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] when the user is not a member of the named
    /// organization; storage failures surface as [`WalletError`].
    pub async fn switch_organization(
        &self,
        session_id: Uuid,
        user_id: Uuid,
        organization_id: Uuid,
    ) -> Result<(Organization, Role), WalletError> {
        let row = sqlx::query(
            "SELECT m.role, o.organization_id, o.name, o.tenant_id, o.kind, o.created_at              FROM oxsum.memberships m JOIN oxsum.organizations o USING (organization_id)              WHERE m.user_id = $1 AND m.organization_id = $2",
        )
        .bind(user_id)
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Err(WalletError::NotFound("not found".into()));
        };
        let organization = orgs::organization_from_row(&row)?;
        let role = Role::parse(&row.try_get::<String, _>("role")?)?;
        sqlx::query(
            "UPDATE oxsum.sessions SET organization_id = $1              WHERE session_id = $2 AND user_id = $3 AND revoked_at IS NULL",
        )
        .bind(organization_id)
        .bind(session_id)
        .bind(user_id)
        .execute(self.pool())
        .await?;
        Ok((organization, role))
    }
}

/// Refreshes `last_used_at`, at most once per [`LAST_USED_TOUCH`]: the column records when
/// the session was last seen without costing a write on every authenticated request.
async fn touch_last_used(pool: &PgPool, session_id: Uuid) -> Result<(), WalletError> {
    let stale_before = OffsetDateTime::now_utc() - LAST_USED_TOUCH;
    sqlx::query(
        "UPDATE oxsum.sessions SET last_used_at = now() \
         WHERE session_id = $1 AND last_used_at < $2",
    )
    .bind(session_id)
    .bind(stale_before)
    .execute(pool)
    .await?;
    Ok(())
}

/// A new session token: the mark plus 32 random bytes from the operating system's CSPRNG,
/// hex-encoded like an API key secret.
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
///
/// A plain hash rather than a password hash: the token is 256 bits of machine randomness,
/// so there is nothing to brute-force, while every authenticated request pays for it.
fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
