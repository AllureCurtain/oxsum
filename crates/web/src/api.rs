//! The dashboard's page API.
//!
//! Server-side rendering calls `oxsum-core` directly; browser interactions call these
//! server functions, which run on the server. There is no hand-written page API
//! (docs/architecture.md). The login/logout pages are the exception: they call the
//! session endpoints (`POST /api/v1/auth/login`, `POST /api/v1/auth/logout`,
//! `GET /api/v1/session`) straight from the browser, because the session cookie is
//! `HttpOnly` and only a real call to those endpoints sets or clears it.
//!
//! Every function here requires a session: the dashboard is the session's surface, and
//! an API key names no session. Role rules are the core's (`key_scope`), the same ones
//! the REST endpoints enforce.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(feature = "ssr")]
use axum::http::header::COOKIE;
#[cfg(feature = "ssr")]
use axum::http::request::Parts;
#[cfg(feature = "ssr")]
use oxsum_core::{
    ApiKey, Db, KeyScope, Kind, LogEntry, Member, OpenHold, Role, SessionPrincipal, Tenants,
    WalletError,
};

/// What the dashboard overview shows: the organization and who acts for it, the balance,
/// the in-flight holds, and the newest log entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardData {
    pub org_name: String,
    pub org_kind: String,
    pub user_email: String,
    pub role: String,
    pub available_minor: i64,
    pub holds: Vec<HoldView>,
    pub entries: Vec<EntryView>,
}

/// One in-flight hold, as the holds section shows it — and as the billing WebSocket
/// keeps it live.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldView {
    pub request_id: String,
    pub model: String,
    pub channel: String,
    pub price_version: i64,
    pub freeze_minor: i64,
    /// Forwarded answer characters, from the live progress events; 0 until the first.
    pub output_chars: usize,
}

/// One ledger entry, as the transaction log shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    pub index: u64,
    pub id: String,
    pub description: String,
    pub content_hash: String,
}

/// One API key, as the keys page shows it. Never the secret: it is returned once, at
/// creation, and the database never holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyView {
    pub id: String,
    pub name: Option<String>,
    pub prefix: String,
    pub created_by: Option<String>,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

/// A freshly minted key: the only time the plaintext secret is visible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedKeyView {
    pub key: KeyView,
    pub secret: String,
}

/// One membership, as the members page shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberView {
    pub email: String,
    pub role: String,
    pub joined_at: String,
}

#[cfg(feature = "ssr")]
impl From<&OpenHold> for HoldView {
    fn from(hold: &OpenHold) -> Self {
        Self {
            request_id: hold.request_id.clone(),
            model: hold.model.clone(),
            channel: hold.channel.clone(),
            price_version: hold.price_version,
            freeze_minor: hold.freeze_minor,
            output_chars: 0,
        }
    }
}

#[cfg(feature = "ssr")]
impl From<&LogEntry> for EntryView {
    fn from(entry: &LogEntry) -> Self {
        Self {
            index: entry.index,
            id: entry.id.clone(),
            description: entry.description.clone(),
            content_hash: entry.content_hash.clone(),
        }
    }
}

#[cfg(feature = "ssr")]
impl From<&ApiKey> for KeyView {
    fn from(key: &ApiKey) -> Self {
        Self {
            id: key.id.to_string(),
            name: key.name.clone(),
            prefix: key.prefix.clone(),
            created_by: key.created_by.map(|id| id.to_string()),
            created_at: key.created_at.to_string(),
            revoked_at: key.revoked_at.map(|at| at.to_string()),
        }
    }
}

#[cfg(feature = "ssr")]
impl From<&Member> for MemberView {
    fn from(member: &Member) -> Self {
        Self {
            email: member.email.clone(),
            role: match member.role {
                Role::Owner => "owner",
                Role::Admin => "admin",
                Role::Member => "member",
            }
            .to_owned(),
            joined_at: member.joined_at.to_string(),
        }
    }
}

/// The session behind the request: the database, the tenants, and who is logged in.
///
/// A missing or dead session is "not logged in", whatever the cause — the same
/// indistinguishability the auth middleware keeps. A storage failure is an error, not
/// a login state: the page should say something broke, not that nobody is logged in.
#[cfg(feature = "ssr")]
async fn session_ctx() -> Result<(Db, Tenants, SessionPrincipal), ServerFnError> {
    let db = use_context::<Db>().ok_or_else(|| ServerFnError::new("the server is not ready"))?;
    let tenants =
        use_context::<Tenants>().ok_or_else(|| ServerFnError::new("the server is not ready"))?;
    let parts = use_context::<Parts>().ok_or_else(|| ServerFnError::new("no request"))?;
    let token = session_cookie(&parts).ok_or_else(|| ServerFnError::new("not logged in"))?;
    let principal = db
        .authenticate_session(&token)
        .await
        .map_err(|_| ServerFnError::new("the session could not be checked"))?
        .ok_or_else(|| ServerFnError::new("not logged in"))?;
    Ok((db, tenants, principal))
}

/// The session cookie's value, from the request the server function runs on.
#[cfg(feature = "ssr")]
fn session_cookie(parts: &Parts) -> Option<String> {
    let cookies = parts.headers.get(COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == oxsum_core::SESSION_COOKIE)
            .then(|| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

/// The dashboard overview: organization, session, balance, in-flight holds, newest entries.
#[server(prefix = "/_pages")]
pub async fn get_dashboard() -> Result<DashboardData, ServerFnError> {
    let (db, tenants, principal) = session_ctx().await?;
    let org = &principal.organization;
    let wallet = tenants
        .get(&org.tenant_id)
        .await
        .map_err(|_| ServerFnError::new("the wallet could not be opened"))?;
    let available_minor = wallet
        .available()
        .await
        .map_err(|_| ServerFnError::new("the balance could not be read"))?;
    let holds = db
        .open_holds_for_tenant(&org.tenant_id)
        .await
        .map_err(|_| ServerFnError::new("the in-flight holds could not be read"))?;
    let entries = wallet
        .recent_entries(25)
        .await
        .map_err(|_| ServerFnError::new("the transaction log could not be read"))?;
    Ok(DashboardData {
        org_name: org.name.clone(),
        org_kind: match org.kind {
            Kind::Personal => "personal",
            Kind::Team => "team",
        }
        .to_owned(),
        user_email: principal.user.email.clone(),
        role: match principal.role {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Member => "member",
        }
        .to_owned(),
        available_minor,
        holds: holds.iter().map(HoldView::from).collect(),
        entries: entries.iter().map(EntryView::from).collect(),
    })
}

/// The transaction log page: the newest entries, newest first.
#[server(prefix = "/_pages")]
pub async fn get_log() -> Result<Vec<EntryView>, ServerFnError> {
    let (_db, tenants, principal) = session_ctx().await?;
    let wallet = tenants
        .get(&principal.organization.tenant_id)
        .await
        .map_err(|_| ServerFnError::new("the wallet could not be opened"))?;
    let entries = wallet
        .recent_entries(100)
        .await
        .map_err(|_| ServerFnError::new("the transaction log could not be read"))?;
    Ok(entries.iter().map(EntryView::from).collect())
}

/// What the session may do with the organization's keys: owners and admins see all,
/// members only the keys they created. The core's rule, as the REST endpoints apply it
/// — roles constrain sessions, not keys (docs/decisions.md).
#[cfg(feature = "ssr")]
fn key_scope(principal: &SessionPrincipal) -> KeyScope {
    match principal.role {
        Role::Owner | Role::Admin => KeyScope::All,
        Role::Member => KeyScope::Own(principal.user.id),
    }
}

/// The keys page: the keys the session's role may see (members only the ones they
/// created, owners and admins all of them — the core's `key_scope`, as on the REST
/// endpoints).
#[server(prefix = "/_pages")]
pub async fn get_keys() -> Result<Vec<KeyView>, ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let keys = db
        .list_keys(principal.organization.id, key_scope(&principal))
        .await
        .map_err(|_| ServerFnError::new("the keys could not be read"))?;
    Ok(keys.iter().map(KeyView::from).collect())
}

/// Mints a key through the session: it records the acting user as its creator, the way
/// `POST /api/v1/org/keys` does.
#[server(prefix = "/_pages")]
pub async fn create_key(name: Option<String>) -> Result<CreatedKeyView, ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let created = db
        .create_key(
            principal.organization.id,
            name,
            None,
            Some(principal.user.id),
        )
        .await
        .map_err(|error| match error {
            // A bad name is the caller's mistake, and the message says what is wrong
            // with it; anything else stays out of the response.
            WalletError::InvalidInput(message) => ServerFnError::new(message),
            _ => ServerFnError::new("the key could not be created"),
        })?;
    Ok(CreatedKeyView {
        key: KeyView::from(&created.key),
        secret: created.secret,
    })
}

/// Revokes a key. A key the session may not touch — another member's, for a member — is
/// not found, not forbidden, the way `DELETE /api/v1/org/keys/{id}` treats it.
#[server(prefix = "/_pages")]
pub async fn revoke_key(key_id: String) -> Result<(), ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let id = key_id
        .parse::<uuid::Uuid>()
        .map_err(|_| ServerFnError::new("not a key id"))?;
    let revoked = db
        .revoke_key(principal.organization.id, id, key_scope(&principal))
        .await
        .map_err(|_| ServerFnError::new("the key could not be revoked"))?;
    revoked
        .map(|_| ())
        .ok_or_else(|| ServerFnError::new("key not found"))
}

/// The members page: everyone in the organization, with their roles.
#[server(prefix = "/_pages")]
pub async fn get_members() -> Result<Vec<MemberView>, ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let members = db
        .members(principal.organization.id)
        .await
        .map_err(|_| ServerFnError::new("the members could not be read"))?;
    Ok(members.iter().map(MemberView::from).collect())
}
