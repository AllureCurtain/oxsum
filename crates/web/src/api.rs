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

use crate::bills::BillView;
use crate::requests::RequestView;

#[cfg(feature = "ssr")]
use crate::requests::RequestFilters;

#[cfg(feature = "ssr")]
use axum::http::header::COOKIE;
#[cfg(feature = "ssr")]
use axum::http::request::Parts;
#[cfg(feature = "ssr")]
use oxsum_core::{
    ApiKey, Db, KeyScope, Kind, LogEntry, Member, MembershipActor, OpenHold, Role,
    SessionPrincipal, Tenants, WalletError,
};

/// What the dashboard overview shows: the organization and who acts for it, the balance,
/// the frozen total, this month's spend, the in-flight holds, and the newest log entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardData {
    pub org_name: String,
    pub org_kind: String,
    pub user_email: String,
    pub role: String,
    pub available_minor: i64,
    /// The organization's frozen total: the sum of its outstanding holds, in minor units.
    /// The wallet's reserved (pending-layer) balance, so an organization holding nothing
    /// reads 0.
    pub frozen_minor: i64,
    /// What settled against the wallet since the first instant of the current UTC month,
    /// in minor units. The settled-layer debits, so top-ups are not spend; a month with
    /// nothing charged reads 0.
    pub month_spend_minor: i64,
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

/// One membership, as the members page shows it and addresses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberView {
    /// The user's id: what a management action names, as the REST endpoints do.
    pub user_id: String,
    pub email: String,
    pub role: String,
    pub joined_at: String,
}

/// The members page's data: the acting role, which decides the controls the page renders,
/// and the organization's members.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MembersView {
    pub role: String,
    pub members: Vec<MemberView>,
}

/// The ownership after a transfer, as the members page reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    pub owner: MemberView,
    pub previous_owner: MemberView,
}

/// One organization the user belongs to, as the organization switcher shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgView {
    /// The organization's id: what a switch names, as the REST endpoint does.
    pub id: String,
    pub name: String,
    pub kind: String,
    /// The role the user holds in this organization.
    pub role: String,
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
            user_id: member.user_id.to_string(),
            email: member.email.clone(),
            role: role_name(member.role).to_owned(),
            joined_at: member.joined_at.to_string(),
        }
    }
}

/// A role as the page shows and compares it: the same lowercase words the contract uses.
#[cfg(feature = "ssr")]
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Admin => "admin",
        Role::Member => "member",
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

/// The dashboard overview: organization, session, balance, frozen total, this month's
/// spend, in-flight holds and newest entries.
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
    // The frozen total is the ledger's own answer (the pending layer, which is what holds
    // reserve), so it cannot drift from what the holds actually reserved.
    let frozen_minor = wallet
        .reserved()
        .await
        .map_err(|_| ServerFnError::new("the frozen total could not be read"))?;
    let month_spend_minor = wallet
        .month_spend()
        .await
        .map_err(|_| ServerFnError::new("this month's spend could not be read"))?;
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
        frozen_minor,
        month_spend_minor,
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

/// The bills page: the organization's settled entries, newest first, each with the
/// content hash its proof verifies against.
///
/// The same read, the same limit and so the same rows as the two exports
/// (`crates/server/src/bills.rs`): the page and the files it offers cannot disagree
/// about what was billed.
#[server(prefix = "/_pages")]
pub async fn get_bills() -> Result<Vec<BillView>, ServerFnError> {
    let (_db, tenants, principal) = session_ctx().await?;
    let wallet = tenants
        .get(&principal.organization.tenant_id)
        .await
        .map_err(|_| ServerFnError::new("the wallet could not be opened"))?;
    let bills = wallet
        .settled_entries(crate::bills::BILLS_LIMIT)
        .await
        .map_err(|_| ServerFnError::new("the bills could not be read"))?;
    Ok(bills.iter().map(BillView::from).collect())
}

/// The requests page: the organization's newest gateway requests with the usage and the
/// cost the ledger's settlement records carry, filtered by key and by model (issue #55).
///
/// The rows are read out of the organization's own ledger ([`Wallet::recent_requests`]),
/// so one organization can never list another's turn, and every number in a row is what
/// the settlement entry's content hash covers: the model, the token counts and the charge
/// all come from the record the gateway wrote when the turn settled.
///
/// The key filter names an API key by its display prefix, and a row's key is resolved
/// through the keys this session may see (`key_scope`): a member reads their own
/// requests' keys and nobody else's, the rule the keys page already applies. A request the
/// ledger attributes to no key is organization history, which any credential of the
/// organization may read.
#[server(prefix = "/_pages")]
pub async fn get_requests(
    key: Option<String>,
    model: Option<String>,
) -> Result<Vec<RequestView>, ServerFnError> {
    let (db, tenants, principal) = session_ctx().await?;
    let organization = &principal.organization;
    let wallet = tenants
        .get(&organization.tenant_id)
        .await
        .map_err(|_| ServerFnError::new("the wallet could not be opened"))?;
    let requests = wallet
        .recent_requests(crate::requests::REQUESTS_LIMIT)
        .await
        .map_err(|_| ServerFnError::new("the requests could not be read"))?;
    let keys = db
        .list_keys(organization.id, key_scope(&principal))
        .await
        .map_err(|_| ServerFnError::new("the keys could not be read"))?;
    let filters = RequestFilters::new(key.as_deref(), model.as_deref());
    let rows = requests
        .iter()
        .filter_map(|request| {
            let key = request
                .key_id
                .as_deref()
                .and_then(|id| keys.iter().find(|key| key.id.as_simple().to_string() == id));
            // Attributed to a key outside this session's scope — another member's key —
            // is not this session's request to read.
            if request.key_id.is_some() && key.is_none() {
                return None;
            }
            let row = RequestView::new(request, key);
            filters.matches(&row).then_some(row)
        })
        .collect();
    Ok(rows)
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
            None,
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
///
/// Readable by any member of the organization: reading who is in it is not a management
/// action (docs/decisions.md, "any member may read the member list"). The session resolves
/// against `memberships`, so this only ever reads the list of an organization the caller is
/// in — there is no organization for a caller to name. The acting role comes back with the
/// list, because it decides which controls the page renders.
#[server(prefix = "/_pages")]
pub async fn get_members() -> Result<MembersView, ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let members = db
        .members(principal.organization.id)
        .await
        .map_err(|_| ServerFnError::new("the members could not be read"))?;
    Ok(MembersView {
        role: role_name(principal.role).to_owned(),
        members: members.iter().map(MemberView::from).collect(),
    })
}

/// Who the session is for the membership rules: the person, and their role.
///
/// The same thing `Principal::membership_actor` builds on the REST surface, and the same
/// rules apply (crates/core/src/orgs.rs): an owner or an admin manages, a member is refused
/// there. A session is always a person, so no API-key case exists here.
#[cfg(feature = "ssr")]
fn membership_actor(principal: &SessionPrincipal) -> MembershipActor {
    MembershipActor {
        user_id: principal.user.id,
        role: principal.role,
    }
}

/// A refused organization or membership action, in the words the rule used.
///
/// Every failure the rules can produce is the caller's to read — who may not do what, which
/// member does not exist, which is the last owner — so the page can state it instead of
/// showing a generic failure (DESIGN.md: a failure says what went wrong). Storage failures
/// stay out of the response.
#[cfg(feature = "ssr")]
fn rule_error(error: WalletError, fallback: &'static str) -> ServerFnError {
    match error {
        WalletError::InvalidInput(message)
        | WalletError::Forbidden(message)
        | WalletError::Conflict(message)
        | WalletError::NotFound(message) => ServerFnError::new(message),
        _ => ServerFnError::new(fallback),
    }
}

/// Adds an existing account to the organization as a member, by email.
///
/// "Invite" means what it can mean today: the account must already exist. Pending
/// invitations for people who have no account yet are issue #59, so an unknown email is
/// refused in words and nothing is stored.
#[server(prefix = "/_pages")]
pub async fn add_member(email: String) -> Result<MemberView, ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let member = db
        .add_member(
            principal.organization.id,
            membership_actor(&principal),
            &email,
        )
        .await
        .map_err(|error| rule_error(error, "the member could not be added"))?;
    Ok(MemberView::from(&member))
}

/// Removes a member from the organization. The person and their own organization stay.
#[server(prefix = "/_pages")]
pub async fn remove_member(user_id: String) -> Result<MemberView, ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let id = member_id(&user_id)?;
    let member = db
        .remove_member(principal.organization.id, membership_actor(&principal), id)
        .await
        .map_err(|error| rule_error(error, "the member could not be removed"))?;
    Ok(MemberView::from(&member))
}

/// Changes a member's role. `owner` is refused by the core: ownership is transferred with
/// [`transfer_ownership`], never assigned, so this cannot promote anyone.
#[server(prefix = "/_pages")]
pub async fn change_member_role(
    user_id: String,
    role: String,
) -> Result<MemberView, ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let id = member_id(&user_id)?;
    // The page sends the words the table shows; anything else is not a role the page offers.
    let role = match role.as_str() {
        "admin" => Role::Admin,
        "member" => Role::Member,
        _ => return Err(ServerFnError::new("role must be admin or member")),
    };
    let member = db
        .change_member_role(
            principal.organization.id,
            membership_actor(&principal),
            id,
            role,
        )
        .await
        .map_err(|error| rule_error(error, "the role could not be changed"))?;
    Ok(MemberView::from(&member))
}

/// Transfers ownership to a member: they become the owner and the acting owner an admin, so
/// the organization keeps exactly one owner. Only an owner may call this.
#[server(prefix = "/_pages")]
pub async fn transfer_ownership(user_id: String) -> Result<TransferView, ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let id = member_id(&user_id)?;
    let ownership = db
        .transfer_ownership(principal.organization.id, membership_actor(&principal), id)
        .await
        .map_err(|error| rule_error(error, "ownership could not be transferred"))?;
    Ok(TransferView {
        owner: MemberView::from(&ownership.owner),
        previous_owner: MemberView::from(&ownership.previous_owner),
    })
}

/// A member id as the page sends it: the `userId` of a row.
#[cfg(feature = "ssr")]
fn member_id(user_id: &str) -> Result<uuid::Uuid, ServerFnError> {
    user_id
        .parse::<uuid::Uuid>()
        .map_err(|_| ServerFnError::new("not a member id"))
}

/// One settled chat turn's bill, as the chat page shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnBill {
    pub request_id: String,
    pub model: String,
    pub charged_minor: i64,
    /// The settlement kind, in the record's own words (`usage`, `estimated`, …).
    pub kind: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub price_version: i64,
    pub freeze_minor: i64,
    /// The content hash the bill verifies against, recomputed from the entry — the
    /// same value `verify_bundle` checks.
    pub content_hash: String,
    /// The proof bundle JSON, as `/verify` pastes it.
    pub bundle_json: String,
}

/// The bill for one settled chat turn: the settlement entry's proof bundle plus the
/// charge read out of it. Answers `None` while the turn has not settled.
///
/// The request id is the gateway's (`x-oxsum-request-id`): it is kept to the id's
/// own alphabet, so it can only name a gateway hold key — and the wallet is the
/// session's organization's, so one organization can never read another's bills.
#[server(prefix = "/_pages")]
pub async fn get_turn_bill(request_id: String) -> Result<Option<TurnBill>, ServerFnError> {
    /// What the settlement entry records, as the gateway wrote it (`billing.rs`).
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SettlementRecord {
        model: String,
        kind: String,
        input_tokens: i64,
        output_tokens: i64,
        price_version: i64,
        charged: i64,
        freeze: i64,
    }

    let (_db, tenants, principal) = session_ctx().await?;
    if request_id.is_empty()
        || request_id.len() > 64
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(ServerFnError::new("not a request id"));
    }
    let wallet = tenants
        .get(&principal.organization.tenant_id)
        .await
        .map_err(|_| ServerFnError::new("the wallet could not be opened"))?;
    // The entry id is caller-computable (docs/api.md): the hold the gateway took is
    // `req-<id>:hold`, and the settlement's key — and so its entry id — derives from it.
    let hold_key = format!("req-{request_id}:hold");
    let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
    let bundle = wallet
        .receipt_proof(entry_id)
        .await
        .map_err(|_| ServerFnError::new("the bill could not be read"))?;
    let Some(bundle) = bundle else {
        return Ok(None);
    };
    let record: SettlementRecord = serde_json::from_str(bundle.entry.description().as_str())
        .map_err(|_| ServerFnError::new("the bill did not parse"))?;
    let bundle_json =
        serde_json::to_string(&bundle).map_err(|_| ServerFnError::new("the bill did not parse"))?;
    Ok(Some(TurnBill {
        request_id,
        model: record.model,
        charged_minor: record.charged,
        kind: record.kind,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        price_version: record.price_version,
        freeze_minor: record.freeze,
        content_hash: bundle.entry.content_hash().to_string(),
        bundle_json,
    }))
}

/// The organization switcher's list: every organization the session's user belongs
/// to, and which one the session acts as now.
#[server(prefix = "/_pages")]
pub async fn list_organizations() -> Result<(Vec<OrgView>, String), ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let organizations = db
        .organizations_of(principal.user.id)
        .await
        .map_err(|_| ServerFnError::new("the organizations could not be read"))?;
    let views = organizations
        .iter()
        .map(|entry| OrgView {
            id: entry.organization.id.to_string(),
            name: entry.organization.name.clone(),
            kind: match entry.organization.kind {
                Kind::Personal => "personal",
                Kind::Team => "team",
            }
            .to_owned(),
            role: role_name(entry.role).to_owned(),
        })
        .collect();
    Ok((views, principal.organization.id.to_string()))
}

/// Create a team organization: the session's user becomes its owner. The session
/// keeps acting as the organization it had — switching is a separate action.
#[server(prefix = "/_pages")]
pub async fn create_team_org(name: String) -> Result<(), ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    db.create_team_organization(principal.user.id, &name)
        .await
        .map_err(|error| rule_error(error, "the organization could not be created"))?;
    Ok(())
}

/// Switch the organization the session acts as. The session row is updated, so
/// the dashboard reload that follows reads the new organization everywhere.
#[server(prefix = "/_pages")]
pub async fn switch_organization(organization_id: String) -> Result<(), ServerFnError> {
    let (db, _tenants, principal) = session_ctx().await?;
    let organization_id = organization_id
        .parse::<uuid::Uuid>()
        .map_err(|_| ServerFnError::new("not an organization id"))?;
    db.switch_organization(principal.session.id, principal.user.id, organization_id)
        .await
        .map_err(|error| rule_error(error, "the organization could not be switched"))?;
    Ok(())
}
