//! The dashboard pages: `/dashboard`, `/dashboard/chat`, `/dashboard/bills`,
//! `/dashboard/keys`, `/dashboard/members`, `/dashboard/log`,
//! `/dashboard/requests`, `/dashboard/usage`.
//!
//! Every page renders server-side with the session's data, calling `oxsum-core`
//! directly the way the Leptos server functions did. Mutations are plain HTML
//! forms posting back to `#[page(POST ...)]` handlers on the same URLs.
//!
//! Topcoat's view model shapes this module: a page's rendered view must own
//! its data (the `view!` body is `async move`), so every value a template
//! touches is an owned local — cloned out of the session before the `view!`.

use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::{Form, Json};
use topcoat::router::error::{SeeOther, bad_request, see_other};
use topcoat::router::{RouterBuilder, page, parse_query_params, route};
use topcoat::view::{View, view};

use crate::bills::{BILLS_LIMIT, BillView};
use crate::heads::{ConsistencyView, SignedHeadView};
use crate::requests::{REQUESTS_PATH, RequestFilters, RequestView};
use crate::session::{SessionCtx, page_ctx};
use crate::usage::{USAGE_DAYS, UsageView};
use crate::views::{credits, dashboard_frame, signed_credits};
use crate::web_config;

/// Registers the dashboard pages on the router builder.
pub(crate) fn register(builder: RouterBuilder) -> RouterBuilder {
    builder
        .page(overview_page)
        .page(chat_page)
        .route(chat_key)
        .page(bills_page)
        .page(bills_consistency)
        .page(keys_page)
        .page(keys_create)
        .route(keys_revoke)
        .page(members_page)
        .route(members_add)
        .route(members_remove)
        .route(members_role)
        .route(members_transfer)
        .page(members_invite)
        .page(log_page)
        .page(requests_page)
        .page(usage_page)
}

fn internal(message: &str) -> topcoat::Error {
    topcoat::router::error::internal_server_error(topcoat::Error::msg(message.to_owned())).into()
}

/// What the session may do with the organization's keys: owners and admins see
/// all, members only the keys they created. The core's rule, as the REST
/// endpoints apply it.
fn key_scope(principal: &oxsum_core::SessionPrincipal) -> oxsum_core::KeyScope {
    match principal.role {
        oxsum_core::Role::Owner | oxsum_core::Role::Admin => oxsum_core::KeyScope::All,
        oxsum_core::Role::Member => oxsum_core::KeyScope::Own(principal.user.id),
    }
}

fn role_name(role: oxsum_core::Role) -> &'static str {
    match role {
        oxsum_core::Role::Owner => "owner",
        oxsum_core::Role::Admin => "admin",
        oxsum_core::Role::Member => "member",
    }
}

/// The dashboard overview: organization, balance, frozen total, this month's
/// spend, in-flight holds and newest entries.
#[page("/dashboard")]
async fn overview_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let tenant_id = sctx.principal.organization.tenant_id.clone();
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let email_verified = sctx.principal.user.email_verified;
    let role = role_name(sctx.principal.role);
    let wallet = sctx
        .tenants
        .get(&tenant_id)
        .await
        .map_err(|_| internal("the wallet could not be opened"))?;
    let available = credits(
        wallet
            .available()
            .await
            .map_err(|_| internal("the balance could not be read"))?,
    );
    let frozen = credits(
        wallet
            .reserved()
            .await
            .map_err(|_| internal("the frozen total could not be read"))?,
    );
    let month_spend = credits(
        wallet
            .month_spend()
            .await
            .map_err(|_| internal("this month's spend could not be read"))?,
    );
    let holds = sctx
        .db
        .open_holds_for_tenant(&tenant_id)
        .await
        .map_err(|_| internal("the in-flight holds could not be read"))?;
    let entries = wallet
        .recent_entries(25)
        .await
        .map_err(|_| internal("the transaction log could not be read"))?;
    let mail_configured = web_config(cx)?.mail_configured;

    Ok(view! {
        dashboard_frame(
            title: "Overview",
            org_name: org_name,
            user_email: user_email,
            <h1>"Overview"</h1>
            if !email_verified && mail_configured {
                <p class="banner">
                    "Your email is not verified. Check your inbox for the verification mail."
                </p>
            }
            <dl>
                <dt>"Available"</dt><dd>(available)</dd>
                <dt>"Frozen"</dt><dd>(frozen)</dd>
                <dt>"Spent this month"</dt><dd>(month_spend)</dd>
                <dt>"Role"</dt><dd>(role)</dd>
            </dl>
            <h2>"In-flight holds"</h2>
            <table>
                <thead><tr><th>"Request"</th><th>"Model"</th><th>"Frozen"</th></tr></thead>
                <tbody>
                    for hold in holds {
                        <tr>
                            <td>(hold.request_id.clone())</td>
                            <td>(hold.model.clone())</td>
                            <td>(credits(hold.freeze_minor))</td>
                        </tr>
                    }
                </tbody>
            </table>
            <h2>"Recent entries"</h2>
            <table>
                <thead><tr><th>"#"</th><th>"Description"</th></tr></thead>
                <tbody>
                    for entry in entries {
                        <tr>
                            <td>(entry.index)</td>
                            <td>(entry.description.clone())</td>
                        </tr>
                    }
                </tbody>
            </table>
        )
    })
}

/// The chat page: top up, pick a model and chat, with each turn's billing.
///
/// The page shell renders server-side; the conversation itself runs in the
/// browser against the existing API (`/api/v1/topups`, `/v1/models`,
/// streaming `/v1/chat/completions`, `/ws/billing`), exactly like the Leptos
/// page. The API key the chat needs is minted by `POST /dashboard/chat/key`
/// when the browser has none stored.
#[page("/dashboard/chat")]
async fn chat_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    Ok(view! {
        dashboard_frame(
            title: "Chat",
            org_name: org_name,
            user_email: user_email,
            <h1>"Chat"</h1>
            <div id="chat">
                <div id="turns"></div>
                <form id="chat-form">
                    <select id="model" aria-label="Model"><option>"loading…"</option></select>
                    <input id="input" type="text" placeholder="Ask something…" autocomplete="off" />
                    <button type="submit">"Send"</button>
                </form>
                <form id="topup-form">
                    <input id="topup-amount" type="text" placeholder="Top up credits" />
                    <button type="submit">"Top up"</button>
                </form>
                <p id="chat-error" class="error" hidden="hidden"></p>
            </div>
            <script src="/chat.js"></script>
        )
    })
}

/// Mints an API key for the chat page, when the browser has none stored.
/// Answers the key's secret once, as JSON.
#[route(POST "/dashboard/chat/key")]
async fn chat_key(cx: &Cx) -> Result<Json<serde_json::Value>> {
    let sctx = page_ctx(cx).await?;
    let created = sctx
        .db
        .create_key(
            sctx.principal.organization.id,
            Some("chat".to_owned()),
            None,
            Some(sctx.principal.user.id),
            oxsum_core::KeyConstraints::default(),
        )
        .await
        .map_err(|_| internal("the key could not be created"))?;
    Ok(Json(serde_json::json!({ "secret": created.secret })))
}

/// The bills page: the organization's transactions, newest first.
#[page("/dashboard/bills")]
async fn bills_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let tenant_id = sctx.principal.organization.tenant_id.clone();
    let org_id = sctx.principal.organization.id;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let wallet = sctx
        .tenants
        .get(&tenant_id)
        .await
        .map_err(|_| internal("the wallet could not be opened"))?;
    let query = parse_query_params::<CursorQuery>(cx).unwrap_or_default();
    let page = wallet
        .transactions_page(query.before, BILLS_LIMIT)
        .await
        .map_err(|_| internal("the bills could not be read"))?;
    let keys = sctx
        .db
        .list_keys(org_id, key_scope(&sctx.principal))
        .await
        .map_err(|_| internal("the keys could not be read"))?;
    let rows: Vec<BillView> = page
        .rows
        .iter()
        .filter(|tx| match tx.key_id.as_deref() {
            Some(id) => keys.iter().any(|key| key.id.as_simple().to_string() == id),
            None => true,
        })
        .map(BillView::from)
        .collect();
    let next_cursor = page.next_cursor;

    // The tree-head section: the operator-signed head, served for the archive
    // check. The check that ran in the browser in the Leptos dashboard now
    // runs here on the server (see `bills_consistency`).
    let head_info: Option<(u64, String, String)> = match signed_head_view(cx, &sctx).await? {
        Some(head) => Some((head.size, head.root, head.key.key_name)),
        None => None,
    };

    Ok(view! {
        dashboard_frame(
            title: "Bills",
            org_name: org_name,
            user_email: user_email,
            <h1>"Bills"</h1>
            <p>
                <a href="/dashboard/bills/export.csv">"CSV"</a>
                " "
                <a href="/dashboard/bills/export.json">"JSON"</a>
            </p>
            <table>
                <thead><tr><th>"Booked"</th><th>"Kind"</th><th>"Amount"</th><th>"Description"</th><th>"Hash"</th><th></th></tr></thead>
                <tbody>
                    for bill in rows {
                        <tr>
                            <td>(bill.booked_on.clone())</td>
                            <td>(bill.kind.clone())</td>
                            <td>(signed_credits(bill.amount_minor))</td>
                            <td>(bill.description.clone())</td>
                            <td class="mono">(bill.content_hash.clone())</td>
                            <td><a href=(format!("/verify?entry={}", bill.entry_id))>"verify"</a></td>
                        </tr>
                    }
                </tbody>
            </table>
            if let Some(cursor) = next_cursor {
                <p><a href=(format!("/dashboard/bills?before={cursor}"))>"Older"</a></p>
            }
            if let Some(head) = head_info {
                <section>
                    <h2>"Ledger head"</h2>
                    <p>
                        "Size " (head.0) ", root "
                        <code>(head.1.clone())</code>
                    </p>
                    <p class="muted">
                        "Signed by " (head.2.clone())
                        ". Paste a previously archived head's size to check consistency."
                    </p>
                    <form method="post" action="/dashboard/bills/consistency">
                        <label>
                            "Archived size"
                            <input type="number" name="from" min="1" required="required" />
                        </label>
                        <button type="submit">"Check consistency"</button>
                    </form>
                </section>
            } else {
                <section>
                    <h2>"Ledger head"</h2>
                    <p class="muted">"This deployment signs no heads."</p>
                </section>
            }
        )
    })
}

#[derive(Deserialize, Default)]
struct CursorQuery {
    before: Option<u64>,
}

/// The session's organization's signed ledger head, or `None` when this
/// deployment signs no heads.
async fn signed_head_view(cx: &Cx, sctx: &SessionCtx) -> Result<Option<SignedHeadView>> {
    let seed = match web_config(cx)?.head_seed {
        Some(seed) => seed,
        None => return Ok(None),
    };
    let key = oxsum_core::signing_key(seed)
        .map_err(|_| internal("the head signing key is misconfigured"))?;
    let wallet = sctx
        .tenants
        .get(&sctx.principal.organization.tenant_id)
        .await
        .map_err(|_| internal("the wallet could not be opened"))?;
    let signed = wallet
        .signed_head(&key)
        .await
        .map_err(|_| internal("the ledger head could not be read"))?;
    Ok(Some(signed.into()))
}

#[derive(Deserialize)]
struct ConsistencyForm {
    from: u64,
}

/// Checks a previously archived head against the current one, server-side.
#[page(POST "/dashboard/bills/consistency")]
async fn bills_consistency(cx: &Cx, Form(form): Form<ConsistencyForm>) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let seed = web_config(cx)?
        .head_seed
        .ok_or_else(|| internal("this deployment signs no heads"))?;
    let key = oxsum_core::signing_key(seed)
        .map_err(|_| internal("the head signing key is misconfigured"))?;
    let wallet = sctx
        .tenants
        .get(&sctx.principal.organization.tenant_id)
        .await
        .map_err(|_| internal("the wallet could not be opened"))?;
    let from = form.from;
    let consistency: ConsistencyView = wallet
        .consistency(from, &key)
        .await
        .map_err(|error| match error {
            oxsum_core::WalletError::InvalidInput(message) => bad_request(message).into(),
            _ => internal("the consistency proof could not be built"),
        })?
        .into();
    let size = consistency.signed.size;
    Ok(view! {
        dashboard_frame(
            title: "Consistency",
            org_name: org_name,
            user_email: user_email,
            <h1>"Consistency proof"</h1>
            <p>
                "From size " (from) " to size " (size) ": "
                <strong>"the log still contains the archived head."</strong>
            </p>
            <p><a href="/dashboard/bills">"Back to bills"</a></p>
        )
    })
}

/// The keys page: the keys the session's role may see.
#[page("/dashboard/keys")]
async fn keys_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let org_id = sctx.principal.organization.id;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let keys = sctx
        .db
        .list_keys(org_id, key_scope(&sctx.principal))
        .await
        .map_err(|_| internal("the keys could not be read"))?;
    // The same projection the `budget.exhaustion_forecast` event fires on, per key (issue #220).
    let forecasts = sctx
        .db
        .key_budget_forecasts(org_id, key_scope(&sctx.principal))
        .await
        .map_err(|_| internal("the budget forecasts could not be read"))?;
    // The committed figure each budget line is measured against — settled spend
    // plus outstanding holds inside the key's window (issue #232).
    let wallet = sctx
        .tenants
        .get(&sctx.principal.organization.tenant_id)
        .await
        .map_err(|_| internal("the wallet could not be opened"))?;
    let today = time::OffsetDateTime::now_utc().date();
    let mut committed = std::collections::BTreeMap::new();
    for key in &keys {
        if key.spend_limit_minor.is_some() && key.revoked_at.is_none() {
            let window = key.budget_duration.map(|d| d.period_start(today));
            let figure = wallet
                .key_committed_window(&key.id, window)
                .await
                .unwrap_or(0);
            committed.insert(key.id, figure);
        }
    }
    Ok(view! {
        dashboard_frame(
            title: "Keys",
            org_name: org_name,
            user_email: user_email,
            <h1>"API keys"</h1>
            <form method="post" action="/dashboard/keys/create">
                <label>
                    "Name"
                    <input type="text" name="name" />
                </label>
                <button type="submit">"Create key"</button>
            </form>
            <table>
                <thead><tr><th>"Name"</th><th>"Prefix"</th><th>"Budget"</th><th></th></tr></thead>
                <tbody>
                    for key in keys {
                        <tr>
                            <td>(key.name.clone().unwrap_or_default())</td>
                            <td><code>(key.prefix.clone())</code></td>
                            <td>(budget_line(&key, &forecasts, committed.get(&key.id).copied()))</td>
                            <td>
                                <form method="post" action="/dashboard/keys/revoke" style="display:inline">
                                    <input type="hidden" name="key_id" value=(key.id.to_string()) />
                                    <button type="submit">"Revoke"</button>
                                </form>
                            </td>
                        </tr>
                    }
                </tbody>
            </table>
        )
    })
}

/// The budget cell one key row renders: the percentage consumed of the limit
/// and its window (issue #232), and the projected exhaustion date when the
/// current burn rate crosses inside the period — the same date the
/// `budget.exhaustion_forecast` webhook announces (issue #220).
fn budget_line(
    key: &oxsum_core::ApiKey,
    forecasts: &[oxsum_core::BudgetForecast],
    committed: Option<i64>,
) -> String {
    use std::fmt::Write as _;
    let Some(limit) = key.spend_limit_minor else {
        return String::new();
    };
    let mut line = String::new();
    if let Some(committed) = committed {
        let pct = if limit > 0 {
            (i128::from(committed.max(0)) * 100 / i128::from(limit)) as i64
        } else {
            0
        };
        let _ = write!(line, "{pct}% of ");
    }
    let _ = write!(line, "{}", credits(limit));
    if let Some(duration) = key.budget_duration {
        let _ = write!(line, " / {}", duration.as_str());
    }
    if let Some(forecast) = forecasts.iter().find(|f| f.key_id == key.id) {
        let _ = write!(
            line,
            " — exhausts {} ({}d)",
            forecast.projected_exhaustion_date, forecast.days_remaining
        );
    }
    line
}

#[derive(Deserialize)]
struct KeyNameForm {
    name: Option<String>,
}

/// Mints a key through the session, showing the secret once.
#[page(POST "/dashboard/keys/create")]
async fn keys_create(cx: &Cx, Form(form): Form<KeyNameForm>) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let name = form.name.filter(|name| !name.trim().is_empty());
    let created = sctx
        .db
        .create_key(
            sctx.principal.organization.id,
            name,
            None,
            Some(sctx.principal.user.id),
            oxsum_core::KeyConstraints::default(),
        )
        .await
        .map_err(|error| match error {
            oxsum_core::WalletError::InvalidInput(message) => bad_request(message).into(),
            _ => internal("the key could not be created"),
        })?;
    let secret = created.secret;
    Ok(view! {
        dashboard_frame(
            title: "Key created",
            org_name: org_name,
            user_email: user_email,
            <h1>"Key created"</h1>
            <p class="muted">"The secret is shown here and nowhere else."</p>
            <p><code>(secret)</code></p>
            <p><a href="/dashboard/keys">"Back to keys"</a></p>
        )
    })
}

#[derive(Deserialize)]
struct KeyIdForm {
    key_id: String,
}

/// Revokes a key.
#[route(POST "/dashboard/keys/revoke")]
async fn keys_revoke(cx: &Cx, Form(form): Form<KeyIdForm>) -> Result<SeeOther> {
    let sctx = page_ctx(cx).await?;
    let id = form
        .key_id
        .parse::<uuid::Uuid>()
        .map_err(|_| bad_request("not a key id"))?;
    sctx.db
        .revoke_key(
            sctx.principal.organization.id,
            id,
            key_scope(&sctx.principal),
        )
        .await
        .map_err(|_| internal("the key could not be revoked"))?;
    Ok(see_other("/dashboard/keys"))
}

/// The members page: everyone in the organization, with their roles.
#[page("/dashboard/members")]
async fn members_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let org_id = sctx.principal.organization.id;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let role = sctx.principal.role;
    let own_id = sctx.principal.user.id;
    let can_manage = matches!(role, oxsum_core::Role::Owner | oxsum_core::Role::Admin);
    let is_owner = matches!(role, oxsum_core::Role::Owner);
    let members = sctx
        .db
        .members(org_id)
        .await
        .map_err(|_| internal("the members could not be read"))?;
    Ok(view! {
        dashboard_frame(
            title: "Members",
            org_name: org_name,
            user_email: user_email,
            <h1>"Members"</h1>
            <p class="muted">"Your role: " (role_name(role))</p>
            <table>
                <thead><tr><th>"Email"</th><th>"Role"</th><th></th></tr></thead>
                <tbody>
                    for member in members {
                        <tr>
                            <td>(member.email.clone())</td>
                            <td>(role_name(member.role))</td>
                            <td>
                                if can_manage {
                                    <form method="post" action="/dashboard/members/role" style="display:inline">
                                        <input type="hidden" name="user_id" value=(member.user_id.to_string()) />
                                        <select name="role">
                                            <option value="admin">"admin"</option>
                                            <option value="member">"member"</option>
                                        </select>
                                        <button type="submit">"Set"</button>
                                    </form>
                                    <form method="post" action="/dashboard/members/remove" style="display:inline">
                                        <input type="hidden" name="user_id" value=(member.user_id.to_string()) />
                                        <button type="submit">"Remove"</button>
                                    </form>
                                }
                                if is_owner && member.user_id != own_id {
                                    <form method="post" action="/dashboard/members/transfer" style="display:inline">
                                        <input type="hidden" name="user_id" value=(member.user_id.to_string()) />
                                        <button type="submit">"Make owner"</button>
                                    </form>
                                }
                            </td>
                        </tr>
                    }
                </tbody>
            </table>
            if can_manage {
                <section>
                    <h2>"Add member"</h2>
                    <form method="post" action="/dashboard/members/add">
                        <label>
                            "Email"
                            <input type="email" name="email" required="required" />
                        </label>
                        <button type="submit">"Add"</button>
                    </form>
                </section>
                <section>
                    <h2>"Invite"</h2>
                    <form method="post" action="/dashboard/members/invite">
                        <button type="submit">"Create invitation link"</button>
                    </form>
                </section>
            }
        )
    })
}

#[derive(Deserialize)]
struct MemberEmailForm {
    email: String,
}

#[derive(Deserialize)]
struct MemberIdForm {
    user_id: String,
}

#[derive(Deserialize)]
struct MemberRoleForm {
    user_id: String,
    role: String,
}

fn membership_actor(principal: &oxsum_core::SessionPrincipal) -> oxsum_core::MembershipActor {
    oxsum_core::MembershipActor {
        user_id: principal.user.id,
        role: principal.role,
    }
}

fn rule_error(error: oxsum_core::WalletError, fallback: &str) -> topcoat::Error {
    match error {
        oxsum_core::WalletError::InvalidInput(message)
        | oxsum_core::WalletError::Forbidden(message)
        | oxsum_core::WalletError::Conflict(message)
        | oxsum_core::WalletError::NotFound(message) => bad_request(message).into(),
        _ => internal(fallback),
    }
}

/// Adds an existing account to the organization as a member, by email.
#[route(POST "/dashboard/members/add")]
async fn members_add(cx: &Cx, Form(form): Form<MemberEmailForm>) -> Result<SeeOther> {
    let sctx = page_ctx(cx).await?;
    sctx.db
        .add_member(
            sctx.principal.organization.id,
            membership_actor(&sctx.principal),
            &form.email,
        )
        .await
        .map_err(|error| rule_error(error, "the member could not be added"))?;
    Ok(see_other("/dashboard/members"))
}

/// Removes a member from the organization.
#[route(POST "/dashboard/members/remove")]
async fn members_remove(cx: &Cx, Form(form): Form<MemberIdForm>) -> Result<SeeOther> {
    let sctx = page_ctx(cx).await?;
    let id = form
        .user_id
        .parse::<uuid::Uuid>()
        .map_err(|_| bad_request("not a member id"))?;
    sctx.db
        .remove_member(
            sctx.principal.organization.id,
            membership_actor(&sctx.principal),
            id,
        )
        .await
        .map_err(|error| rule_error(error, "the member could not be removed"))?;
    Ok(see_other("/dashboard/members"))
}

/// Changes a member's role.
#[route(POST "/dashboard/members/role")]
async fn members_role(cx: &Cx, Form(form): Form<MemberRoleForm>) -> Result<SeeOther> {
    let sctx = page_ctx(cx).await?;
    let id = form
        .user_id
        .parse::<uuid::Uuid>()
        .map_err(|_| bad_request("not a member id"))?;
    let role = match form.role.as_str() {
        "admin" => oxsum_core::Role::Admin,
        "member" => oxsum_core::Role::Member,
        _ => return Err(bad_request("role must be admin or member").into()),
    };
    sctx.db
        .change_member_role(
            sctx.principal.organization.id,
            membership_actor(&sctx.principal),
            id,
            role,
        )
        .await
        .map_err(|error| rule_error(error, "the role could not be changed"))?;
    Ok(see_other("/dashboard/members"))
}

/// Transfers ownership to a member.
#[route(POST "/dashboard/members/transfer")]
async fn members_transfer(cx: &Cx, Form(form): Form<MemberIdForm>) -> Result<SeeOther> {
    let sctx = page_ctx(cx).await?;
    let id = form
        .user_id
        .parse::<uuid::Uuid>()
        .map_err(|_| bad_request("not a member id"))?;
    sctx.db
        .transfer_ownership(
            sctx.principal.organization.id,
            membership_actor(&sctx.principal),
            id,
        )
        .await
        .map_err(|error| rule_error(error, "ownership could not be transferred"))?;
    Ok(see_other("/dashboard/members"))
}

/// Mints an invitation link into the session's organization.
#[page(POST "/dashboard/members/invite")]
async fn members_invite(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let created = sctx
        .db
        .create_invitation(
            membership_actor(&sctx.principal),
            sctx.principal.organization.id,
        )
        .await
        .map_err(|error| rule_error(error, "the invitation could not be created"))?;
    let invite_link = format!("/register?invite={}", created.token);
    let expires_at = created.expires_at.to_string();
    Ok(view! {
        dashboard_frame(
            title: "Invitation",
            org_name: org_name,
            user_email: user_email,
            <h1>"Invitation link"</h1>
            <p><code>(invite_link)</code></p>
            <p class="muted">"Expires " (expires_at)</p>
            <p><a href="/dashboard/members">"Back to members"</a></p>
        )
    })
}

/// The transaction log page: the newest entries, newest first.
#[page("/dashboard/log")]
async fn log_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let tenant_id = sctx.principal.organization.tenant_id.clone();
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let wallet = sctx
        .tenants
        .get(&tenant_id)
        .await
        .map_err(|_| internal("the wallet could not be opened"))?;
    let query = parse_query_params::<CursorQuery>(cx).unwrap_or_default();
    let page = wallet
        .entries_page(query.before, 100)
        .await
        .map_err(|_| internal("the transaction log could not be read"))?;
    // The checkpoint the witness-checkpoints pass last anchored externally —
    // the "we cannot rewrite this log" evidence (issue #231).
    let checkpoint = sctx
        .db
        .witness_checkpoint(sctx.principal.organization.id)
        .await
        .map_err(|_| internal("the anchored checkpoint could not be read"))?;
    // Check each cosignature against the deployment's pinned keyset (issue
    // #259): the verdict names the witnesses that failed rather than only
    // counting the ones that passed. A parse failure on the stored note is a
    // server-side integrity bug — the checkpoint is rendered but flagged.
    let witness_keys = web_config(cx)?.witness_keys;
    let witness_verdicts = checkpoint.as_ref().and_then(|checkpoint| {
        use oxsum_verify::{WitnessNote, WitnessPin};
        let pins: Vec<WitnessPin<'_>> = witness_keys
            .iter()
            .map(|k| WitnessPin {
                witness_url: &k.witness_url,
                key_name: &k.key_name,
                public_key: &k.public_key,
                key_hash: &k.key_hash,
            })
            .collect();
        let notes: Vec<WitnessNote<'_>> = checkpoint
            .cosignatures
            .iter()
            .map(|co| WitnessNote {
                witness_url: &co.witness_url,
                note: &co.note,
            })
            .collect();
        oxsum_core::SignedTreeHead::parse(&checkpoint.note)
            .ok()
            .map(move |parsed| {
                oxsum_verify::verify_witnesses(&notes, &pins, parsed.origin.as_str(), &parsed.head)
                    .into_iter()
                    .map(|(url, verdict)| (url.to_owned(), verdict))
                    .collect::<Vec<_>>()
            })
    });
    // One line under the checkpoint: how many cosignatures verify, and the
    // witnesses that failed named rather than counted (issue #259).
    let witness_line = witness_verdicts.as_ref().map(|verdicts| {
        let verified = verdicts
            .iter()
            .filter(|(_, v)| matches!(v, oxsum_verify::WitnessVerdict::Verified { .. }))
            .count();
        let failed: Vec<_> = verdicts
            .iter()
            .filter(|(_, v)| !matches!(v, oxsum_verify::WitnessVerdict::Verified { .. }))
            .collect();
        if failed.is_empty() && verified > 0 {
            format!(
                "Anchored by {verified} witness{} — every cosignature verifies under the deployment's pinned keys.",
                if verified == 1 { "" } else { "es" }
            )
        } else {
            let mut line = format!("{verified} of {} cosignatures verify.", verdicts.len());
            for (url, verdict) in failed {
                line.push_str(&format!(" {url}: {verdict}."));
            }
            line
        }
    });
    let rows = page.rows;
    let next_cursor = page.next_cursor;
    Ok(view! {
        dashboard_frame(
            title: "Log",
            org_name: org_name,
            user_email: user_email,
            <h1>"Transaction log"</h1>
            if let Some(checkpoint) = checkpoint {
                <p class="muted">
                    (format!(
                        "Anchored checkpoint: head #{}, published {}, cosigned by {} witness{}.",
                        checkpoint.head_size,
                        checkpoint.updated_at,
                        checkpoint.cosignatures.len(),
                        if checkpoint.cosignatures.len() == 1 { "" } else { "es" },
                    ))
                    for cosig in &checkpoint.cosignatures {
                        <br />
                        <code>(cosig.witness_url.clone())</code>
                    }
                </p>
                if let Some(line) = &witness_line {
                    <p class="muted">(line.clone())</p>
                }
            }
            <table>
                <thead><tr><th>"#"</th><th>"Description"</th><th>"Content hash"</th></tr></thead>
                <tbody>
                    for entry in rows {
                        <tr>
                            <td>(entry.index)</td>
                            <td>(entry.description.clone())</td>
                            <td><code>(entry.content_hash.clone())</code></td>
                        </tr>
                    }
                </tbody>
            </table>
            if let Some(cursor) = next_cursor {
                <p><a href=(format!("/dashboard/log?before={cursor}"))>"Older"</a></p>
            }
        )
    })
}

/// The requests page: the organization's newest gateway requests, filtered by
/// key and by model through the query string.
#[page("/dashboard/requests")]
async fn requests_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let tenant_id = sctx.principal.organization.tenant_id.clone();
    let org_id = sctx.principal.organization.id;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let wallet = sctx
        .tenants
        .get(&tenant_id)
        .await
        .map_err(|_| internal("the wallet could not be opened"))?;
    let query = parse_query_params::<RequestsQuery>(cx).unwrap_or_default();
    let page = wallet
        .requests_page(query.before, crate::requests::REQUESTS_LIMIT)
        .await
        .map_err(|_| internal("the requests could not be read"))?;
    let keys = sctx
        .db
        .list_keys(org_id, key_scope(&sctx.principal))
        .await
        .map_err(|_| internal("the keys could not be read"))?;
    let filters = RequestFilters::new(query.key.as_deref(), query.model.as_deref());
    let rows: Vec<RequestView> = page
        .rows
        .iter()
        .filter_map(|request| {
            let key = request
                .key_id
                .as_deref()
                .and_then(|id| keys.iter().find(|key| key.id.as_simple().to_string() == id));
            if request.key_id.is_some() && key.is_none() {
                return None;
            }
            let row = RequestView::new(request, key);
            filters.matches(&row).then_some(row)
        })
        .collect();
    let key_value = query.key.clone().unwrap_or_default();
    let model_value = query.model.clone().unwrap_or_default();
    let no_rows = rows.is_empty();
    let next_cursor = page.next_cursor;
    let older_href: Option<String> = next_cursor.map(|cursor| {
        format!(
            "{REQUESTS_PATH}?before={cursor}&key={}&model={}",
            url_encode(&key_value),
            url_encode(&model_value)
        )
    });
    Ok(view! {
        dashboard_frame(
            title: "Requests",
            org_name: org_name,
            user_email: user_email,
            <h1>"Requests"</h1>
            <form method="get" action=(REQUESTS_PATH)>
                <label>
                    "Key prefix"
                    <input type="text" name="key" value=(key_value) />
                </label>
                <label>
                    "Model"
                    <input type="text" name="model" value=(model_value) />
                </label>
                <button type="submit">"Filter"</button>
            </form>
            <table>
                <thead><tr><th>"Request"</th><th>"Booked"</th><th>"Model"</th><th>"Key"</th><th>"Status"</th><th>"Cost"</th></tr></thead>
                <tbody>
                    for row in rows {
                        <tr>
                            <td>(row.request_id.clone())</td>
                            <td>(row.booked_on.clone())</td>
                            <td>(row.model.clone())</td>
                            <td>(row.key_prefix.clone().unwrap_or_default())</td>
                            <td>
                                (row.status.clone())
                                if let Some(reason) = &row.failure_reason {
                                    <br /><small>(reason.clone())</small>
                                }
                            </td>
                            <td>(credits(row.cost_minor))</td>
                        </tr>
                    }
                </tbody>
            </table>
            if no_rows {
                <p>"No requests match these filters."</p>
            }
            if let Some(href) = older_href {
                <p><a href=(href)>"Older"</a></p>
            }
        )
    })
}

#[derive(Deserialize, Default)]
struct RequestsQuery {
    key: Option<String>,
    model: Option<String>,
    before: Option<u64>,
}

/// The usage page: the organization's daily usage rollup over the trailing
/// [`USAGE_DAYS`] UTC days.
#[page("/dashboard/usage")]
async fn usage_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let tenant_id = sctx.principal.organization.tenant_id.clone();
    let org_id = sctx.principal.organization.id;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let to = time::OffsetDateTime::now_utc().date();
    let from = to - time::Duration::days(USAGE_DAYS - 1);
    let days: Vec<String> = (0..USAGE_DAYS)
        .map(|offset| (from + time::Duration::days(offset)).to_string())
        .collect();
    let rows = sctx
        .db
        .usage_daily(&tenant_id, from, to)
        .await
        .map_err(|_| internal("the usage could not be read"))?;
    let keys = sctx
        .db
        .list_keys(org_id, key_scope(&sctx.principal))
        .await
        .map_err(|_| internal("the keys could not be read"))?;
    let usage = UsageView {
        days,
        rows: rows
            .iter()
            .filter(|row| match row.key_id {
                Some(id) => keys.iter().any(|key| key.id.as_simple() == id.as_simple()),
                None => true,
            })
            .map(|row| crate::usage::UsageDayView::new(row, &keys))
            .collect(),
    };
    let day_rows = usage.rows;
    Ok(view! {
        dashboard_frame(
            title: "Usage",
            org_name: org_name,
            user_email: user_email,
            <h1>"Usage"</h1>
            <p class="muted">"Trailing " (USAGE_DAYS) " days."</p>
            <table>
                <thead><tr><th>"Day"</th><th>"Input tokens"</th><th>"Output tokens"</th><th>"Cost"</th></tr></thead>
                <tbody>
                    for row in day_rows {
                        <tr>
                            <td>(row.day.clone())</td>
                            <td>(row.input_tokens)</td>
                            <td>(row.output_tokens)</td>
                            <td>(credits(row.charged_minor))</td>
                        </tr>
                    }
                </tbody>
            </table>
        )
    })
}

/// Minimal percent-encoding for query values we build.
fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
