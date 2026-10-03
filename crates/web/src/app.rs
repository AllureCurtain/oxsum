//! The dashboard pages.
//!
//! `/login` and `/logout` call the session endpoints (`POST /api/v1/auth/login`,
//! `POST /api/v1/auth/logout`, `GET /api/v1/session`) from the browser with `fetch`,
//! because the session cookie is `HttpOnly` and only a real call to those endpoints
//! sets or clears it. Everything else on the pages goes through the server functions
//! in [`crate::api`]: SSR calls `oxsum-core` directly, and the browser calls the same
//! functions after hydration.

use leptos::hydration::HydrationScripts;
use leptos::prelude::*;
use leptos_meta::{Stylesheet, Title, provide_meta_context};
use leptos_router::components::{A, Outlet, ParentRoute, Redirect, Route, Router, Routes};
use leptos_router::hooks::{use_navigate, use_query_map};
use leptos_router::path;

use crate::admin::{
    AdminAnomaliesPage, AdminChannelsPage, AdminInFlightPage, AdminLayout, AdminOrganizationsPage,
};
use crate::api::{
    CreatedKeyView, DashboardData, EntryView, HoldView, KeyView, MemberView, MembersView,
    TransferView, add_member, change_member_role, create_key, get_bills, get_dashboard, get_keys,
    get_log, get_members, get_requests, remove_member, revoke_key, transfer_ownership,
};
use crate::bills::BillView;
use crate::chat::ChatPage;
use crate::requests::{REQUESTS_PATH, RequestFilters, RequestView};

/// The HTML shell: the document around the app. `leptos_axum` renders this as the
/// whole response, so the `<head>` leptos_meta needs lives here, together with the
/// hydration scripts that make the server-rendered page interactive.
#[component]
pub fn Shell(options: leptos::config::LeptosOptions) -> impl IntoView {
    provide_meta_context();
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <Stylesheet id="leptos" href="/pkg/oxsum.css"/>
                <Title text="oxsum dashboard"/>
                <HydrationScripts options/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

/// The route tree: the login/logout pages and the dashboard.
#[component]
pub fn App() -> impl IntoView {
    view! {
        <Router>
            <Routes fallback=|| view! { <NotFound/> }>
                <Route path=path!("/") view=|| view! { <Redirect path="/dashboard"/> }/>
                <Route path=path!("/login") view=LoginPage/>
                <Route path=path!("/logout") view=LogoutPage/>
                // Public: anyone holding a bill and its content hash can verify it, no
                // session needed. Verification runs in the browser, not on the server.
                <Route path=path!("/verify") view=VerifyPage/>
                // The deployment's surface: the operator token gates it in the browser,
                // and the admin endpoints refuse what the gate missed (issue #57).
                <ParentRoute path=path!("/admin") view=AdminLayout>
                    <Route path=path!("/") view=|| view! { <Redirect path="/admin/channels"/> }/>
                    <Route path=path!("/channels") view=AdminChannelsPage/>
                    <Route path=path!("/organizations") view=AdminOrganizationsPage/>
                    <Route path=path!("/in-flight") view=AdminInFlightPage/>
                    <Route path=path!("/anomalies") view=AdminAnomaliesPage/>
                </ParentRoute>
                <ParentRoute path=path!("/dashboard") view=DashboardLayout>
                    <Route path=path!("/") view=OverviewPage/>
                    <Route path=path!("/chat") view=ChatPage/>
                    <Route path=path!("/bills") view=BillsPage/>
                    <Route path=path!("/keys") view=KeysPage/>
                    <Route path=path!("/members") view=MembersPage/>
                    <Route path=path!("/log") view=LogPage/>
                    <Route path=path!("/requests") view=RequestsPage/>
                </ParentRoute>
            </Routes>
        </Router>
    }
}

/// Unknown path: a way back, not a dead end.
#[component]
fn NotFound() -> impl IntoView {
    view! {
        <main class="center">
            <h1>"Not found"</h1>
            <p>"This page does not exist."</p>
            <p><A href="/dashboard">"Back to the dashboard"</A></p>
        </main>
    }
}

/// Browser-only calls to the session endpoints. The cookie is `HttpOnly`, so these go
/// through `fetch`: a server function could not set or clear it on the browser.
#[cfg(feature = "hydrate")]
mod browser {
    use gloo_net::http::Request;
    use serde::Serialize;

    #[derive(Serialize)]
    struct LoginBody<'a> {
        email: &'a str,
        password: &'a str,
    }

    /// `true` when the browser currently holds a live session.
    pub async fn session_ok() -> bool {
        Request::get("/api/v1/session")
            .send()
            .await
            .map(|response| response.ok())
            .unwrap_or(false)
    }

    /// Logs in through the session endpoint. The `Set-Cookie` on the response is stored
    /// by the browser itself; there is nothing to carry back.
    pub async fn login(email: &str, password: &str) -> Result<(), String> {
        let response = Request::post("/api/v1/auth/login")
            .json(&LoginBody { email, password })
            .map_err(|_| "could not build the request".to_owned())?
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        match response.status() {
            200 => Ok(()),
            401 => Err("Invalid email or password.".to_owned()),
            _ => Err("Login failed; try again.".to_owned()),
        }
    }

    /// Logs out through the session endpoint, which also clears the cookie.
    pub async fn logout() {
        let _ = Request::post("/api/v1/auth/logout").send().await;
    }
}

/// Logs in through `POST /api/v1/auth/login`, then enters the dashboard.
#[component]
fn LoginPage() -> impl IntoView {
    let (email, set_email) = signal(String::new());
    let (password, set_password) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);
    let navigate = use_navigate();

    // Already logged in: the session endpoint says so, and there is nothing to do here.
    Effect::new({
        let navigate = navigate.clone();
        move |_| {
            // Cloned per run: the async block below takes its clone, so the closure
            // itself stays callable more than once.
            let navigate = navigate.clone();
            #[cfg(feature = "hydrate")]
            leptos::task::spawn_local(async move {
                if browser::session_ok().await {
                    navigate("/dashboard", Default::default());
                }
            });
            // SSR only emits the page; the redirect runs in the browser.
            #[cfg(not(feature = "hydrate"))]
            let _ = &navigate;
        }
    });

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        {
            let navigate = navigate.clone();
            leptos::task::spawn_local(async move {
                set_busy.set(true);
                set_error.set(None);
                match browser::login(&email.get_untracked(), &password.get_untracked()).await {
                    Ok(()) => navigate("/dashboard", Default::default()),
                    Err(message) => {
                        set_error.set(Some(message));
                        set_busy.set(false);
                    }
                }
            });
        }
        // SSR only emits the inert form; the submit handler runs in the browser. The
        // method is for the case where the handler never arrives — a browser without the
        // wasm, or a failed hydration: a native submit must not put the password in the
        // URL, and `GET /login` is the only thing a form without a method would do
        // (issue #46).
        #[cfg(not(feature = "hydrate"))]
        let _ = (&navigate, &set_busy, &set_error);
    };

    view! {
        <main class="center">
            <form class="card" method="post" on:submit=submit aria-label="Log in">
                <h1>"oxsum"</h1>
                <p class="muted">"Log in to the dashboard."</p>
                {move || error.get().map(|message| view! { <p class="error" role="alert">{message}</p> })}
                <label>
                    "Email"
                    <input
                        type="email"
                        name="email"
                        autocomplete="username"
                        required
                        prop:value=move || email.get()
                        on:input=move |ev| set_email.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Password"
                    <input
                        type="password"
                        name="password"
                        autocomplete="current-password"
                        required
                        prop:value=move || password.get()
                        on:input=move |ev| set_password.set(event_target_value(&ev))
                    />
                </label>
                <button type="submit" prop:disabled=move || busy.get()>
                    {move || if busy.get() { "Logging in…" } else { "Log in" }}
                </button>
            </form>
        </main>
    }
}

/// Logs out through `POST /api/v1/auth/logout`, then returns to the login page.
#[component]
fn LogoutPage() -> impl IntoView {
    let navigate = use_navigate();
    Effect::new(move |_| {
        #[cfg(feature = "hydrate")]
        {
            let navigate = navigate.clone();
            leptos::task::spawn_local(async move {
                browser::logout().await;
                navigate("/login", Default::default());
            });
        }
        // SSR only emits the page; the logout call runs in the browser.
        #[cfg(not(feature = "hydrate"))]
        let _ = &navigate;
    });
    view! {
        <main class="center">
            <p class="muted">"Logging out…"</p>
        </main>
    }
}

/// The dashboard frame: navigation plus the session guard.
///
/// A browser without a live session is sent back to `/login`: the session endpoint is
/// the check, and an unauthenticated render below never sees real data because the
/// server functions refuse it too.
#[component]
fn DashboardLayout() -> impl IntoView {
    let navigate = use_navigate();
    Effect::new(move |_| {
        #[cfg(feature = "hydrate")]
        {
            let navigate = navigate.clone();
            leptos::task::spawn_local(async move {
                if !browser::session_ok().await {
                    navigate("/login", Default::default());
                }
            });
        }
        // SSR only emits the frame; the session check runs in the browser.
        #[cfg(not(feature = "hydrate"))]
        let _ = &navigate;
    });
    view! {
        <div class="layout">
            <nav class="sidenav" aria-label="Dashboard">
                <div class="brand">"oxsum"</div>
                <A href="/dashboard">"Overview"</A>
                <A href="/dashboard/chat">"Chat"</A>
                <A href="/dashboard/bills">"Bills"</A>
                <A href="/dashboard/keys">"API keys"</A>
                <A href="/dashboard/members">"Members"</A>
                <A href="/dashboard/log">"Transaction log"</A>
                <A href="/dashboard/requests">"Requests"</A>
                // Top-level and public, like /logout: verification needs no session.
                <A href="/verify">"Verify a bill"</A>
                <A href="/logout">"Log out"</A>
            </nav>
            <main class="content">
                <Outlet/>
            </main>
        </div>
    }
}

/// Formats minor units as credits with six decimals, without floating point: money is
/// integers all the way down, including on the way to the screen.
pub(crate) fn credits(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

/// The overview: who is logged in, the balance with the frozen total and this month's
/// spend, the in-flight holds (live), and the newest log entries.
#[component]
fn OverviewPage() -> impl IntoView {
    let dashboard = Resource::new(|| (), |_| async { get_dashboard().await });
    view! {
        <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
            {move || dashboard.get().map(|result| match result {
                Ok(data) => view! { <Overview data=data/> }.into_any(),
                Err(_) => view! {
                    <section class="card">
                        <h1>"Dashboard"</h1>
                        <p class="error" role="alert">
                            "Could not load the dashboard. "
                            <A href="/login">"Log in again"</A>
                            " and retry."
                        </p>
                    </section>
                }.into_any(),
            })}
        </Suspense>
    }
}

/// The overview card: who is logged in, the available balance, the frozen total, this
/// month's spend. The in-flight holds and the newest entries follow it.
#[component]
fn Overview(data: DashboardData) -> impl IntoView {
    view! {
        <section class="card">
            <h1>{data.org_name.clone()}</h1>
            <p class="muted">
                {format!("{} · {} · {}", data.user_email, data.role, data.org_kind)}
            </p>
            <p class="balance">
                <span class="muted">"Available balance"</span>
                <strong class="mono">{credits(data.available_minor)}</strong>
            </p>
            <p class="balance">
                <span class="muted">"Frozen by outstanding holds"</span>
                <strong class="mono pending">{credits(data.frozen_minor)}</strong>
            </p>
            <p class="balance">
                <span class="muted">"Spent this month"</span>
                <strong class="mono">{credits(data.month_spend_minor)}</strong>
            </p>
        </section>
        <HoldsSection initial=data.holds.clone()/>
        <section class="card" aria-label="Recent entries">
            <h2>"Recent entries"</h2>
            <EntryTable entries=data.entries.clone()/>
            <p><A href="/dashboard/log">"Full transaction log"</A></p>
        </section>
    }
}

/// The in-flight holds, kept live over the billing WebSocket: the snapshot on connect,
/// then started/progress/settled events as turns happen.
#[component]
fn HoldsSection(#[prop(into)] initial: Vec<HoldView>) -> impl IntoView {
    let (holds, set_holds) = signal(initial);
    #[cfg(feature = "hydrate")]
    crate::billing_socket::watch(move |event| {
        use crate::billing_socket::BillingEvent;
        match event {
            BillingEvent::Snapshot { holds: snapshot } => set_holds.set(
                snapshot
                    .into_iter()
                    .map(|hold| HoldView {
                        request_id: hold.request_id,
                        model: hold.model,
                        channel: hold.channel,
                        price_version: hold.price_version,
                        freeze_minor: hold.freeze_minor,
                        output_chars: 0,
                    })
                    .collect(),
            ),
            BillingEvent::TurnStarted {
                request_id,
                model,
                channel,
                freeze_minor,
            } => set_holds.update(|holds| {
                if !holds.iter().any(|hold| hold.request_id == request_id) {
                    holds.insert(
                        0,
                        HoldView {
                            request_id,
                            model,
                            channel,
                            price_version: 0,
                            freeze_minor,
                            output_chars: 0,
                        },
                    );
                }
            }),
            BillingEvent::TurnProgress {
                request_id,
                output_chars,
            } => set_holds.update(|holds| {
                if let Some(hold) = holds.iter_mut().find(|hold| hold.request_id == request_id) {
                    hold.output_chars = output_chars;
                }
            }),
            // A settled turn leaves the in-flight list: its charge is in the log.
            BillingEvent::TurnSettled { request_id } => {
                set_holds.update(|holds| holds.retain(|hold| hold.request_id != request_id));
            }
        }
    });
    // The signal is only written from the browser; on the server it is read-only.
    #[cfg(not(feature = "hydrate"))]
    let _ = set_holds;
    view! {
        <section class="card" aria-label="In-flight holds">
            <h2>"In-flight holds"</h2>
            <p class="muted live">"Live"</p>
            {move || {
                let current = holds.get();
                if current.is_empty() {
                    view! { <p class="muted">"No holds in flight."</p> }.into_any()
                } else {
                    view! {
                        <table>
                            <thead>
                                <tr>
                                    <th scope="col">"Request"</th>
                                    <th scope="col">"Model"</th>
                                    <th scope="col" class="num">"Freeze"</th>
                                    <th scope="col" class="num">"Streamed"</th>
                                </tr>
                            </thead>
                            <tbody>
                                <For
                                    each=move || holds.get()
                                    key=|hold| hold.request_id.clone()
                                    let(hold)
                                >
                                    <tr>
                                        <td class="mono">{hold.request_id.clone()}</td>
                                        <td>{hold.model.clone()}</td>
                                        <td class="mono num">{credits(hold.freeze_minor)}</td>
                                        <td class="mono num">
                                            {format!("{} chars", hold.output_chars)}
                                        </td>
                                    </tr>
                                </For>
                            </tbody>
                        </table>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

/// The newest ledger entries, newest first.
#[component]
fn EntryTable(#[prop(into)] entries: Vec<EntryView>) -> impl IntoView {
    view! {
        {if entries.is_empty() {
            view! { <p class="muted">"No entries yet."</p> }.into_any()
        } else {
            view! {
                <table>
                    <thead>
                        <tr>
                            <th scope="col" class="num">"#"</th>
                            <th scope="col">"Entry"</th>
                            <th scope="col">"Description"</th>
                            <th scope="col">"Content hash"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each=move || entries.clone() key=|entry| entry.id.clone() let(entry)>
                            <tr>
                                <td class="mono num">{entry.index}</td>
                                <td class="mono">{entry.id.clone()}</td>
                                <td>{entry.description.clone()}</td>
                                <td class="mono">{entry.content_hash.clone()}</td>
                            </tr>
                        </For>
                    </tbody>
                </table>
            }
                .into_any()
        }}
    }
}

/// The bills page: the organization's settled entries, newest first, each with the
/// content hash its proof verifies against — and the two exports, which carry the same
/// rows.
///
/// The exports are links rather than buttons: `GET /dashboard/bills/export.csv` answers
/// with `Content-Disposition: attachment`, so the browser saves the file without any
/// script of ours, and a command-line client can fetch it the same way
/// (docs/decisions.md). The charge is the ledger's integer in both files and travels
/// through the row type unformatted; the table renders that same integer with the
/// dashboard's [`credits()`], so the page and the files cannot disagree about an amount.
#[component]
fn BillsPage() -> impl IntoView {
    let bills = Resource::new(|| (), |_| async { get_bills().await });
    view! {
        <section class="card" aria-label="Bills">
            <h1>"Bills"</h1>
            <p class="muted">
                "This organization's settled entries, newest first: when each was booked, what it charged in credits, and the content hash its proof verifies against. The two downloads carry the same rows, with the charge as the ledger's integer in minor units."
            </p>
            <p>
                <a href="/dashboard/bills/export.csv">"Download CSV"</a>
                " · "
                <a href="/dashboard/bills/export.json">"Download JSON"</a>
            </p>
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || bills.get().map(|result| match result {
                    Ok(bills) => view! { <BillTable bills=bills/> }.into_any(),
                    Err(error) => view! { <p class="error" role="alert">{error.to_string()}</p> }.into_any(),
                })}
            </Suspense>
        </section>
    }
}

/// The settled entries: booked on, entry id, cost in credits, content hash. The columns
/// are the exports' fields, in the same order; the charge is the one column rendered for
/// a reader rather than for a machine, so it goes through [`credits()`] like every other
/// amount in the dashboard.
#[component]
fn BillTable(#[prop(into)] bills: Vec<BillView>) -> impl IntoView {
    view! {
        {if bills.is_empty() {
            view! { <p class="muted">"No settled entries yet."</p> }.into_any()
        } else {
            view! {
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Booked on"</th>
                            <th scope="col">"Entry"</th>
                            <th scope="col" class="num">"Cost (credits)"</th>
                            <th scope="col">"Content hash"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each=move || bills.clone() key=|bill| bill.entry_id.clone() let(bill)>
                            <tr>
                                <td class="mono">{bill.booked_on.clone()}</td>
                                <td class="mono">{bill.entry_id.clone()}</td>
                                <td class="mono num">{credits(bill.charged_minor)}</td>
                                <td class="mono">{bill.content_hash.clone()}</td>
                            </tr>
                        </For>
                    </tbody>
                </table>
            }
                .into_any()
        }}
    }
}

/// The keys page: list, mint, revoke. Members only ever see the keys they created.
#[component]
fn KeysPage() -> impl IntoView {
    let keys = Resource::new(|| (), |_| async { get_keys().await });
    let (name, set_name) = signal(String::new());
    let (created, set_created) = signal(Option::<CreatedKeyView>::None);
    let (mint_error, set_mint_error) = signal(Option::<String>::None);

    let mint = Action::new(|name: &Option<String>| {
        let name = name.clone();
        async move { create_key(name.filter(|text| !text.trim().is_empty())).await }
    });
    Effect::new(move |_| match mint.value().get() {
        Some(Ok(key)) => {
            set_created.set(Some(key));
            set_mint_error.set(None);
            set_name.set(String::new());
            keys.refetch();
        }
        Some(Err(error)) => set_mint_error.set(Some(error.to_string())),
        None => {}
    });

    let revoke = Action::new(|id: &String| {
        let id = id.clone();
        async move { revoke_key(id).await }
    });
    Effect::new(move |_| {
        if revoke.value().get().is_some() {
            keys.refetch();
        }
    });

    view! {
        <section class="card" aria-label="API keys">
            <h1>"API keys"</h1>
            {move || created.get().map(|key| view! {
                <div class="secret" role="alert">
                    <p><strong>"Copy this secret now — it is never shown again."</strong></p>
                    <code class="mono">{key.secret.clone()}</code>
                    <button on:click=move |_| set_created.set(None)>"Dismiss"</button>
                </div>
            })}
            <form
                class="row"
                on:submit=move |ev: web_sys::SubmitEvent| {
                    ev.prevent_default();
                    mint.dispatch(Some(name.get()));
                }
                aria-label="Mint a key"
            >
                <label>
                    "Name"
                    <input
                        type="text"
                        name="key-name"
                        prop:value=move || name.get()
                        on:input=move |ev| set_name.set(event_target_value(&ev))
                    />
                </label>
                <button type="submit" prop:disabled=move || mint.pending().get()>
                    {move || if mint.pending().get() { "Minting…" } else { "Mint key" }}
                </button>
            </form>
            {move || mint_error.get().map(|message| view! { <p class="error" role="alert">{message}</p> })}
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || keys.get().map(|result| match result {
                    Ok(keys) => view! { <KeyTable keys=keys revoke=revoke/> }.into_any(),
                    Err(error) => view! { <p class="error" role="alert">{error.to_string()}</p> }.into_any(),
                })}
            </Suspense>
        </section>
    }
}

#[component]
fn KeyTable(
    #[prop(into)] keys: Vec<KeyView>,
    revoke: Action<String, Result<(), ServerFnError>>,
) -> impl IntoView {
    view! {
        {if keys.is_empty() {
            view! { <p class="muted">"No keys yet."</p> }.into_any()
        } else {
            view! {
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Name"</th>
                            <th scope="col">"Prefix"</th>
                            <th scope="col">"Created"</th>
                            <th scope="col">"Status"</th>
                            <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each=move || keys.clone() key=|key| key.id.clone() let(key)>
                            <tr>
                                <td>{key.name.clone().unwrap_or_else(|| "—".to_owned())}</td>
                                <td class="mono">{key.prefix.clone()}</td>
                                <td class="mono">{key.created_at.clone()}</td>
                                <td>
                                    {if key.revoked_at.is_some() {
                                        "revoked".to_owned()
                                    } else {
                                        "active".to_owned()
                                    }}
                                </td>
                                <td>
                                    {if key.revoked_at.is_none() {
                                        let id = key.id.clone();
                                        view! {
                                            <button
                                                class="danger"
                                                aria-label=format!("Revoke key {}", key.prefix)
                                                on:click=move |_| {
                            revoke.dispatch(id.clone());
                        }
                                            >
                                                "Revoke"
                                            </button>
                                        }
                                            .into_any()
                                    } else {
                                        ().into_any()
                                    }}
                                </td>
                            </tr>
                        </For>
                    </tbody>
                </table>
            }
                .into_any()
        }}
    }
}

/// The members page: everyone in the organization, with their roles, and — for the roles
/// allowed to use them — the four management actions.
///
/// Any member may read the list (docs/decisions.md); only an owner or an admin manages
/// memberships, and the controls are rendered only where the server would accept the call:
/// the core refuses an admin on an owner's row and refuses to remove or demote the
/// organization's last owner (crates/core/src/orgs.rs), and the table shows no control for
/// either. A hidden button is a call the server would have refused anyway — the page never
/// offers what would fail, and never pretends an action worked: every attempt answers in
/// words, the server's own when it failed.
#[component]
fn MembersPage() -> impl IntoView {
    let members = Resource::new(|| (), |_| async { get_members().await });
    let (notice, set_notice) = signal(Option::<Notice>::None);
    // The add form's field: passed down as a signal, so a successful add clears it without
    // the form being rebuilt.
    let email = RwSignal::new(String::new());

    // The four actions, each one call to a page server function. The rules are the core's,
    // the same ones the REST endpoints apply.
    let add = Action::new(|email: &String| {
        let email = email.clone();
        async move { add_member(email).await }
    });
    let remove = Action::new(|user_id: &String| {
        let user_id = user_id.clone();
        async move { remove_member(user_id).await }
    });
    let set_role = Action::new(|(user_id, role): &(String, String)| {
        let (user_id, role) = (user_id.clone(), role.clone());
        async move { change_member_role(user_id, role).await }
    });
    let transfer = Action::new(|user_id: &String| {
        let user_id = user_id.clone();
        async move { transfer_ownership(user_id).await }
    });

    // What each action did: the state it produced in words, or the server's refusal in its
    // own words. The table is re-read either way, so the page shows what is true now.
    Effect::new(move |_| match add.value().get() {
        Some(Ok(member)) => {
            set_notice.set(Some(Notice::done(format!(
                "{} is now a member.",
                member.email
            ))));
            email.set(String::new());
            members.refetch();
        }
        Some(Err(error)) => set_notice.set(Some(Notice::refused(error.to_string()))),
        None => {}
    });
    Effect::new(move |_| match remove.value().get() {
        Some(Ok(member)) => {
            set_notice.set(Some(Notice::done(format!(
                "{} is no longer a member.",
                member.email
            ))));
            members.refetch();
        }
        Some(Err(error)) => set_notice.set(Some(Notice::refused(error.to_string()))),
        None => {}
    });
    Effect::new(move |_| match set_role.value().get() {
        Some(Ok(member)) => {
            set_notice.set(Some(Notice::done(format!(
                "{} is now {}.",
                member.email,
                role_phrase(&member.role)
            ))));
            members.refetch();
        }
        Some(Err(error)) => set_notice.set(Some(Notice::refused(error.to_string()))),
        None => {}
    });
    Effect::new(move |_| match transfer.value().get() {
        Some(Ok(ownership)) => {
            set_notice.set(Some(Notice::done(format!(
                "{} now owns the organization; {} is {}.",
                ownership.owner.email,
                ownership.previous_owner.email,
                role_phrase(&ownership.previous_owner.role)
            ))));
            members.refetch();
        }
        Some(Err(error)) => set_notice.set(Some(Notice::refused(error.to_string()))),
        None => {}
    });

    view! {
        <section class="card" aria-label="Members">
            <h1>"Members"</h1>
            {move || notice.get().map(|notice| {
                let class = if notice.refused { "error" } else { "success" };
                let role = if notice.refused { "alert" } else { "status" };
                view! { <p class=class role=role>{notice.message}</p> }
            })}
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || members.get().map(|result| match result {
                    Ok(data) => view! {
                        <Members data=data add=add remove=remove set_role=set_role transfer=transfer email=email/>
                    }.into_any(),
                    Err(error) => view! { <p class="error" role="alert">{error.to_string()}</p> }.into_any(),
                })}
            </Suspense>
        </section>
    }
}

/// The outcome of the last membership action, in words.
///
/// The sentence carries the meaning; the class only emphasises it, so the page never relies
/// on colour alone (DESIGN.md).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Notice {
    refused: bool,
    message: String,
}

impl Notice {
    fn done(message: String) -> Self {
        Self {
            refused: false,
            message,
        }
    }

    fn refused(message: String) -> Self {
        Self {
            refused: true,
            message,
        }
    }
}

/// The roles, as the table shows them and as the controls compare them.
const ROLE_OWNER: &str = "owner";
const ROLE_ADMIN: &str = "admin";
const ROLE_MEMBER: &str = "member";

/// A role as a sentence says it: "a member", "an admin", "the owner".
fn role_phrase(role: &str) -> &'static str {
    match role {
        ROLE_OWNER => "the owner",
        ROLE_ADMIN => "an admin",
        _ => "a member",
    }
}

/// One row of the members table: the membership, and what the acting role may do with it.
#[derive(Debug, Clone, PartialEq)]
struct MemberRow {
    member: MemberView,
    /// The role may be changed and the member removed: an owner or an admin acting on a row
    /// that is neither an owner's (for an admin) nor the organization's last owner.
    manageable: bool,
    /// Ownership may be transferred to this member: only an owner may, and only to someone
    /// who is not already the owner.
    transferable: bool,
}

/// The members card's body: the add form, the table, and the per-row controls.
#[component]
fn Members(
    data: MembersView,
    add: Action<String, Result<MemberView, ServerFnError>>,
    remove: Action<String, Result<MemberView, ServerFnError>>,
    set_role: Action<(String, String), Result<MemberView, ServerFnError>>,
    transfer: Action<String, Result<TransferView, ServerFnError>>,
    email: RwSignal<String>,
) -> impl IntoView {
    let role = data.role.clone();
    let manages = role == ROLE_OWNER || role == ROLE_ADMIN;
    let owner_count = data
        .members
        .iter()
        .filter(|member| member.role == ROLE_OWNER)
        .count();
    // The conditions are the core's own: an admin may not touch an owner, and the last owner
    // is neither removed nor demoted. Computed once per render, so the row says what the
    // server would say.
    let rows: Vec<MemberRow> = data
        .members
        .iter()
        .map(|member| {
            let last_owner = member.role == ROLE_OWNER && owner_count <= 1;
            let admin_on_owner = role == ROLE_ADMIN && member.role == ROLE_OWNER;
            MemberRow {
                member: member.clone(),
                manageable: manages && !last_owner && !admin_on_owner,
                transferable: role == ROLE_OWNER && member.role != ROLE_OWNER,
            }
        })
        .collect();
    let owner_rows = role == ROLE_OWNER;
    view! {
        {manages.then(|| view! {
            <form
                class="row"
                aria-label="Add a member"
                on:submit=move |ev: web_sys::SubmitEvent| {
                    ev.prevent_default();
                    // The handle is dropped: the Action itself reports the outcome.
                    drop(add.dispatch(email.get_untracked()));
                }
            >
                <label>
                    "Email of an existing account"
                    <input
                        type="email"
                        name="member-email"
                        required=true
                        prop:value=move || email.get()
                        on:input=move |ev| email.set(event_target_value(&ev))
                    />
                </label>
                <button type="submit" prop:disabled=move || add.pending().get()>
                    {move || if add.pending().get() { "Adding…" } else { "Add member" }}
                </button>
            </form>
            <p class="muted">
                "The account must already exist: inviting people who have no account yet is not built (issue #59)."
            </p>
        })}
        {if rows.is_empty() {
            view! { <p class="muted">"No members."</p> }.into_any()
        } else {
            view! {
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Email"</th>
                            <th scope="col">"Role"</th>
                            <th scope="col">"Joined"</th>
                            {manages.then(|| view! {
                                <th scope="col"><span class="visually-hidden">"Actions"</span></th>
                            })}
                        </tr>
                    </thead>
                    <tbody>
                        <For each=move || rows.clone() key=|row| row.member.user_id.clone() let(row)>
                            {
                                let member = row.member.clone();
                                let manage_id = member.user_id.clone();
                                let remove_id = member.user_id.clone();
                                let transfer_id = member.user_id.clone();
                                let remove_label = format!("Remove {}", member.email);
                                let transfer_label = format!("Transfer ownership to {}", member.email);
                                let demote = member.role == ROLE_ADMIN;
                                view! {
                                    <tr>
                                        <td>{member.email.clone()}</td>
                                        <td>{member.role.clone()}</td>
                                        <td class="mono">{member.joined_at.clone()}</td>
                                        {manages.then(|| view! {
                                            <td>
                                                <div class="actions">
                                                    {row.manageable.then(|| view! {
                                                        {if demote {
                                                            view! {
                                                                <button
                                                                    on:click=move |_| drop(set_role.dispatch((manage_id.clone(), ROLE_MEMBER.to_owned())))
                                                                    prop:disabled=move || set_role.pending().get()
                                                                >"Make member"</button>
                                                            }.into_any()
                                                        } else {
                                                            view! {
                                                                <button
                                                                    on:click=move |_| drop(set_role.dispatch((manage_id.clone(), ROLE_ADMIN.to_owned())))
                                                                    prop:disabled=move || set_role.pending().get()
                                                                >"Make admin"</button>
                                                            }.into_any()
                                                        }}
                                                        <button
                                                            class="danger"
                                                            aria-label=remove_label
                                                            on:click=move |_| drop(remove.dispatch(remove_id.clone()))
                                                            prop:disabled=move || remove.pending().get()
                                                        >"Remove"</button>
                                                    })}
                                                    {row.transferable.then(|| view! {
                                                        <button
                                                            aria-label=transfer_label
                                                            on:click=move |_| drop(transfer.dispatch(transfer_id.clone()))
                                                            prop:disabled=move || transfer.pending().get()
                                                        >"Make owner"</button>
                                                    })}
                                                    {(!row.manageable && !row.transferable).then(|| view! {
                                                        <span class="muted">"—"</span>
                                                    })}
                                                </div>
                                            </td>
                                        })}
                                    </tr>
                                }
                            }
                        </For>
                    </tbody>
                </table>
                {owner_rows.then(|| view! {
                    <p class="muted">
                        "An organization has exactly one owner: transferring ownership makes the previous owner an admin."
                    </p>
                })}
            }
                .into_any()
        }}
    }
}

/// The transaction log page: the newest entries, newest first.
#[component]
fn LogPage() -> impl IntoView {
    let log = Resource::new(|| (), |_| async { get_log().await });
    view! {
        <section class="card" aria-label="Transaction log">
            <h1>"Transaction log"</h1>
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || log.get().map(|result| match result {
                    Ok(entries) => view! { <EntryTable entries=entries/> }.into_any(),
                    Err(error) => view! { <p class="error" role="alert">{error.to_string()}</p> }.into_any(),
                })}
            </Suspense>
        </section>
    }
}

/// The requests page: the organization's gateway requests, newest first, filterable by key
/// and by model.
///
/// The filters are the URL's own query string (`?key=<prefix>&model=<name>`): the form is a
/// plain `GET` to this page, and each row's key and model are links that set that one
/// filter — so a filtered view can be linked and survives a reload, in a browser that
/// hydrates and in one that does not.
///
/// A request is listed once it has settled, and its status is the settlement kind the bill
/// records (`usage`, `estimated`, `capped`, …), beside the tokens it used and what it
/// charged, rendered with the same credits formatting as every other amount. A turn still in
/// flight has no settlement yet: the overview shows those live, with their frozen upper
/// bound.
#[component]
fn RequestsPage() -> impl IntoView {
    let query = use_query_map();
    // Read once, from the URL: SSR and the browser agree on the filters, and a native `GET`
    // re-renders this component with the new query string.
    let filters = StoredValue::new(RequestFilters::new(
        query.get().get("key").as_deref(),
        query.get().get("model").as_deref(),
    ));
    let requests = Resource::new(
        || (),
        move |_| async move {
            get_requests(
                filters.get_value().key.clone(),
                filters.get_value().model.clone(),
            )
            .await
        },
    );
    view! {
        <section class="card" aria-label="Requests">
            <h1>"Requests"</h1>
            <p class="muted">
                "This organization's gateway requests, newest first: each settled turn's status, the tokens it used and what it charged in credits (1 credit = 1,000,000). Filter by key or by model — the filters are in the URL, so a filtered view can be linked and survives a reload."
            </p>
            <RequestFilterForm filters=filters.get_value()/>
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || requests.get().map(|result| match result {
                    Ok(rows) => view! {
                        <RequestTable
                            requests=rows
                            filters=filters.get_value()
                            filtered=!filters.get_value().is_unfiltered()
                        />
                    }
                        .into_any(),
                    Err(error) => view! { <p class="error" role="alert">{error.to_string()}</p> }
                        .into_any(),
                })}
            </Suspense>
        </section>
    }
}

/// The filter form: a plain `GET` to this page, so the submitted values land in the query
/// string with no script of ours — a browser that never hydrates filters just as well —
/// and the fields come back filled from the URL. "Clear" is a link to the unfiltered page.
#[component]
fn RequestFilterForm(filters: RequestFilters) -> impl IntoView {
    let key = filters.key.clone().unwrap_or_default();
    let model = filters.model.clone().unwrap_or_default();
    let clear = (!filters.is_unfiltered()).then(|| {
        view! {
            <a href=REQUESTS_PATH>"Clear filters"</a>
        }
    });
    view! {
        <form class="row" method="get" action=REQUESTS_PATH aria-label="Filter requests">
            <label>
                "API key"
                <input
                    type="text"
                    name="key"
                    autocomplete="off"
                    spellcheck="false"
                    placeholder="oxs-…"
                    value=key
                />
            </label>
            <label>
                "Model"
                <input
                    type="text"
                    name="model"
                    autocomplete="off"
                    spellcheck="false"
                    placeholder="deepseek-chat"
                    value=model
                />
            </label>
            <button type="submit">"Filter"</button>
            {clear}
        </form>
    }
}

/// The requests, one row each: when the settlement was booked, the request id, the model
/// and the key that name it, its status, the tokens it used and what it cost, rendered as
/// credits like every other amount in the dashboard. The key and the model are links that
/// add that filter to the URL, keeping whatever filter is already set. With nothing to show
/// the table keeps its header and says why it is empty, so a filtered page is never mistaken
/// for a broken one.
#[component]
fn RequestTable(
    requests: Vec<RequestView>,
    filters: RequestFilters,
    filtered: bool,
) -> impl IntoView {
    view! {
        <table>
            <thead>
                <tr>
                    <th scope="col">"Booked on"</th>
                    <th scope="col">"Request"</th>
                    <th scope="col">"Model"</th>
                    <th scope="col">"Key"</th>
                    <th scope="col">"Status"</th>
                    <th scope="col" class="num">"Input tokens"</th>
                    <th scope="col" class="num">"Output tokens"</th>
                    <th scope="col" class="num">"Cost (credits)"</th>
                </tr>
            </thead>
            <tbody>
                {if requests.is_empty() {
                    view! {
                        <tr>
                            <td colspan="8" class="muted">
                                {if filtered {
                                    "No requests match these filters."
                                } else {
                                    "No requests yet."
                                }}
                            </td>
                        </tr>
                    }
                        .into_any()
                } else {
                    view! {
                        <For
                            each=move || requests.clone()
                            key=|request| request.request_id.clone()
                            let(request)
                        >
                            <tr>
                                <td class="mono">{request.booked_on.clone()}</td>
                                <td class="mono">{request.request_id.clone()}</td>
                                <td>
                                    <a
                                        href=filters.with_model(&request.model).href()
                                        title="Filter by this model"
                                    >
                                        {request.model.clone()}
                                    </a>
                                </td>
                                <td class="mono">
                                    {match request.key_prefix.clone() {
                                        Some(prefix) => {
                                            let href = filters.with_key(&prefix).href();
                                            view! {
                                                <a href=href title="Filter by this key">{prefix}</a>
                                            }
                                                .into_any()
                                        }
                                        None => view! { "—" }.into_any(),
                                    }}
                                </td>
                                <td>{request.status.clone()}</td>
                                <td class="mono num">{request.input_tokens}</td>
                                <td class="mono num">{request.output_tokens}</td>
                                <td class="mono num">{credits(request.cost_minor)}</td>
                            </tr>
                        </For>
                    }
                        .into_any()
                }}
            </tbody>
        </table>
    }
}

/// What one verification attempt concluded.
#[derive(Debug, Clone)]
enum VerifyOutcome {
    /// The bundle matches the content hash and the proof links it to the tree head.
    Passed,
    /// The bundle does not match the hash, or the proof does not link to the head.
    Failed,
    /// The content hash field is not 64 hexadecimal characters.
    BadHash,
    /// The bundle field does not parse as a proof bundle; carries the parse error.
    BadBundle(String),
}

/// The public verification page: paste a proof bundle and the content hash recorded
/// with the bill, and see the verdict.
///
/// Verification runs entirely in the browser — [`oxsum_verify::verify_bundle`] is
/// the same code the server runs, compiled to WASM — so a passed check does not
/// depend on trusting this server, and the bundle never leaves the browser.
///
/// The chat page links here with both halves prefilled (`?bundle=…&contentHash=…`);
/// the check then runs on load.
#[component]
fn VerifyPage() -> impl IntoView {
    let query = use_query_map();
    // Prefilled from the query string by the chat page's "verify this bill" links.
    // Read once, from the URL: SSR and hydration agree on the values.
    let (bundle, set_bundle) = signal(query.get().get("bundle").unwrap_or_default());
    let (content_hash, set_content_hash) =
        signal(query.get().get("contentHash").unwrap_or_default());
    let (outcome, set_outcome) = signal(Option::<VerifyOutcome>::None);

    let ready = move || !(bundle.get().trim().is_empty() || content_hash.get().trim().is_empty());

    let run = move || {
        let outcome = match oxsum_verify::Hash::parse_hex(content_hash.get_untracked().trim()) {
            Err(_) => VerifyOutcome::BadHash,
            Ok(expected) => match oxsum_verify::verify_bundle(&bundle.get_untracked(), &expected) {
                Err(error) => VerifyOutcome::BadBundle(error.to_string()),
                Ok(true) => VerifyOutcome::Passed,
                Ok(false) => VerifyOutcome::Failed,
            },
        };
        set_outcome.set(Some(outcome));
    };

    // A prefilled link runs the check on load instead of waiting for a click.
    let auto_ran = StoredValue::new(false);
    Effect::new(move |_| {
        if !auto_ran.get_value() && ready() {
            auto_ran.set_value(true);
            run();
        }
    });

    let verify = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        run();
    };

    view! {
        <main class="center">
            <section class="card verify" aria-label="Verify a bill">
                <h1>"Verify a bill"</h1>
                <p class="muted">
                    "Paste the proof bundle and the content hash you recorded with the bill. "
                    "Verification runs in this browser — the bundle is never sent to the server."
                </p>
                <form on:submit=verify aria-label="Verify a proof bundle">
                    <label>
                        "Proof bundle (JSON)"
                        <textarea
                            name="bundle"
                            rows="10"
                            spellcheck="false"
                            autocomplete="off"
                            placeholder="{\"entry\": …}"
                            prop:value=move || bundle.get()
                            on:input=move |ev| set_bundle.set(event_target_value(&ev))
                        />
                    </label>
                    <label>
                        "Content hash"
                        <input
                            type="text"
                            name="content-hash"
                            autocomplete="off"
                            spellcheck="false"
                            placeholder="64 hexadecimal characters"
                            class="mono"
                            prop:value=move || content_hash.get()
                            on:input=move |ev| set_content_hash.set(event_target_value(&ev))
                        />
                    </label>
                    <button type="submit" prop:disabled=move || !ready()>
                        "Verify"
                    </button>
                </form>
                {move || outcome.get().map(|outcome| view! { <Verdict outcome=outcome/> })}
                <p class="muted fine-print">
                    "Verification proves: this record was not altered after being written, and history was not rewritten. "
                    "It does not prove: upstream really returned that many tokens."
                </p>
            </section>
        </main>
    }
}

/// The verdict of one verification attempt: an icon plus words, never color alone.
#[component]
fn Verdict(outcome: VerifyOutcome) -> impl IntoView {
    let (class, icon, title, detail) = match &outcome {
        VerifyOutcome::Passed => (
            "success",
            "✓",
            "Verification passed.",
            "The bundle matches the content hash, and the inclusion proof links it to the tree head."
                .to_owned(),
        ),
        VerifyOutcome::Failed => (
            "error",
            "✗",
            "Verification failed.",
            "The bundle does not match this content hash, or the inclusion proof does not link to the tree head. Changing any single number in the bundle fails the check."
                .to_owned(),
        ),
        VerifyOutcome::BadHash => (
            "error",
            "✗",
            "That is not a content hash.",
            "The content hash is 64 hexadecimal characters — the value recorded with the bill."
                .to_owned(),
        ),
        VerifyOutcome::BadBundle(error) => (
            "error",
            "✗",
            "That is not a proof bundle.",
            format!("The bundle does not parse as JSON: {error}"),
        ),
    };
    view! {
        <div class="verdict" role="status">
            <p class=class>
                <span aria-hidden="true">{icon}</span> <strong>{title}</strong>
            </p>
            <p class="muted">{detail}</p>
        </div>
    }
}
