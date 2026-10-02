//! Users and signup.
//!
//! Passwords are argon2 hashes from `password-auth`, one function each way and no knobs to
//! get wrong (docs/decisions.md, "users and login"). Signup is one transaction: the user,
//! the personal organization, the owner membership and the first API key. A user without an
//! organization, or an organization without its owner, cannot exist.

use password_auth::generate_hash;
use serde::Serialize;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::keys::{self, CreatedApiKey};
use crate::orgs::{self, Kind, Organization, Role};

/// The shortest password signup accepts. Length is the only rule that survives contact with
/// real users: no composition classes, no expiry.
const MIN_PASSWORD: usize = 12;
const MAX_PASSWORD: usize = 256;
/// RFC 5321's maximum path length.
const MAX_EMAIL: usize = 320;
const MAX_ORG_NAME: usize = 80;
/// The name of the key signup mints, so the list has something readable in it.
const FIRST_KEY_NAME: &str = "default";

/// A user as the API presents it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: Uuid,
    pub email: String,
}

/// What a caller must supply to register.
#[derive(Debug, Clone)]
pub struct NewUser {
    pub email: String,
    pub password: String,
    /// Name of the personal organization; defaults to the local part of the email.
    pub organization_name: Option<String>,
}

/// What registration produced. The key's secret is never seen again after this response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub user: User,
    pub organization: Organization,
    pub api_key: CreatedApiKey,
}

impl Db {
    /// Registers a user with a personal organization and the first API key.
    ///
    /// The ledger is deliberately not part of this transaction: it is created on first use
    /// by `Tenants::get`, idempotently, and a ledger whose schema already exists is reused.
    /// Registering therefore cannot fail half way through a ledger migration, and a retry
    /// after a failed registration is safe.
    pub async fn register(&self, new: NewUser) -> Result<Registration, WalletError> {
        let email = validate_email(&new.email)?;
        let email_normalized = email.to_lowercase();
        validate_password(&new.password)?;
        let organization_name = organization_name(new.organization_name.as_deref(), &email)?;

        // Argon2 is deliberately slow (~100ms), and it runs before the transaction opens,
        // so hashing never holds a database connection. `generate_hash` is infallible.
        let password_hash = generate_hash(&new.password);

        let user_id = Uuid::new_v4();
        let organization_id = Uuid::new_v4();
        let mut tx = self.pool().begin().await?;
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
        let organization =
            orgs::insert(&mut tx, organization_id, &organization_name, Kind::Personal).await?;
        orgs::add_member(&mut tx, organization_id, user_id, Role::Owner).await?;
        let api_key = keys::insert(
            &mut tx,
            organization_id,
            Some(FIRST_KEY_NAME.to_owned()),
            None,
            Some(user_id),
            None,
        )
        .await?;
        tx.commit().await?;

        Ok(Registration {
            user: User { id: user_id, email },
            organization,
            api_key,
        })
    }
}

/// Trims and sanity-checks an address: one `@`, something on each side, no whitespace.
///
/// v1 sends no email, so this only has to stop obvious mistakes and keep the unique index
/// meaningful; it is not an address validator.
fn validate_email(raw: &str) -> Result<String, WalletError> {
    let email = raw.trim();
    if email.is_empty() || email.len() > MAX_EMAIL || email.chars().any(char::is_whitespace) {
        return Err(WalletError::InvalidInput(
            "email must be one address of at most 320 bytes".into(),
        ));
    }
    let (local, domain) = email
        .split_once('@')
        .ok_or_else(|| WalletError::InvalidInput("email must look like name@example.com".into()))?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return Err(WalletError::InvalidInput(
            "email must look like name@example.com".into(),
        ));
    }
    Ok(email.to_owned())
}

fn validate_password(password: &str) -> Result<(), WalletError> {
    // Characters on both ends, matching the limits openapi.yaml states: a user counting
    // characters should not be told they pass and then be refused.
    let length = password.chars().count();
    if length < MIN_PASSWORD {
        return Err(WalletError::InvalidInput(format!(
            "password must be at least {MIN_PASSWORD} characters"
        )));
    }
    if length > MAX_PASSWORD {
        return Err(WalletError::InvalidInput(format!(
            "password must be at most {MAX_PASSWORD} characters"
        )));
    }
    Ok(())
}

/// The personal organization's name: the caller's choice, or the email's local part.
fn organization_name(given: Option<&str>, email: &str) -> Result<String, WalletError> {
    if let Some(given) = given.map(str::trim).filter(|name| !name.is_empty()) {
        if given.chars().count() > MAX_ORG_NAME {
            return Err(WalletError::InvalidInput(format!(
                "organizationName must be at most {MAX_ORG_NAME} characters"
            )));
        }
        return Ok(given.to_owned());
    }
    let local = email.split('@').next().unwrap_or("personal");
    Ok(local.chars().take(MAX_ORG_NAME).collect())
}
