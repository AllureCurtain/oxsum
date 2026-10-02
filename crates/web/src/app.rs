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
use leptos_router::hooks::use_navigate;
use leptos_router::path;

use crate::api::{
    CreatedKeyView, DashboardData, EntryView, HoldView, KeyView, MemberView, create_key,
    get_dashboard, get_keys, get_log, get_members, revoke_key,
};

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
                <ParentRoute path=path!("/dashboard") view=DashboardLayout>
                    <Route path=path!("/") view=OverviewPage/>
                    <Route path=path!("/keys") view=KeysPage/>
                    <Route path=path!("/members") view=MembersPage/>
                    <Route path=path!("/log") view=LogPage/>
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
        // SSR only emits the inert form; the submit handler runs in the browser.
        #[cfg(not(feature = "hydrate"))]
        let _ = (&navigate, &set_busy, &set_error);
    };

    view! {
        <main class="center">
            <form class="card" on:submit=submit aria-label="Log in">
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
                <A href="/dashboard/keys">"API keys"</A>
                <A href="/dashboard/members">"Members"</A>
                <A href="/dashboard/log">"Transaction log"</A>
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
fn credits(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

/// The overview: who is logged in, the balance, the in-flight holds (live), and the
/// newest log entries.
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
    billing_socket::watch(set_holds);
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

/// The billing WebSocket: snapshot first, then live turn events. Browser-only.
#[cfg(feature = "hydrate")]
mod billing_socket {
    use leptos::prelude::*;
    use serde::Deserialize;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{MessageEvent, WebSocket};

    use crate::api::HoldView;

    /// One message from `/ws/billing`.
    #[derive(Debug, Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    enum BillingMessage {
        #[serde(rename_all = "camelCase")]
        Snapshot { holds: Vec<SnapshotHold> },
        #[serde(rename_all = "camelCase")]
        TurnStarted {
            request_id: String,
            model: String,
            channel: String,
            freeze_minor: i64,
        },
        #[serde(rename_all = "camelCase")]
        TurnProgress {
            request_id: String,
            output_chars: usize,
        },
        #[serde(rename_all = "camelCase")]
        TurnSettled { request_id: String },
    }

    /// A hold as the snapshot carries it: the server's `OpenHold`, camelCased.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SnapshotHold {
        request_id: String,
        model: String,
        channel: String,
        price_version: i64,
        freeze_minor: i64,
    }

    /// Opens the socket and applies every message to the holds signal. The socket lives
    /// as long as the section; closing it on cleanup.
    pub fn watch(set_holds: WriteSignal<Vec<HoldView>>) {
        let socket = StoredValue::new(None::<WebSocket>);
        Effect::new(move |_| {
            let Ok(ws) = WebSocket::new("/ws/billing") else {
                return;
            };
            let onmessage = Closure::wrap(Box::new(move |event: MessageEvent| {
                if let Some(text) = event.data().as_string() {
                    apply(&text, set_holds);
                }
            }) as Box<dyn FnMut(MessageEvent)>);
            ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
            // The closure outlives this effect: it is only dropped with the page.
            onmessage.forget();
            socket.set_value(Some(ws));
        });
        on_cleanup(move || {
            if let Some(ws) = socket.get_value() {
                let _ = ws.close();
            }
        });
    }

    /// Folds one socket message into the holds list.
    fn apply(text: &str, set_holds: WriteSignal<Vec<HoldView>>) {
        let Ok(message) = serde_json::from_str::<BillingMessage>(text) else {
            return;
        };
        match message {
            BillingMessage::Snapshot { holds } => set_holds.set(
                holds
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
            BillingMessage::TurnStarted {
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
            BillingMessage::TurnProgress {
                request_id,
                output_chars,
            } => set_holds.update(|holds| {
                if let Some(hold) = holds.iter_mut().find(|hold| hold.request_id == request_id) {
                    hold.output_chars = output_chars;
                }
            }),
            // A settled turn leaves the in-flight list: its charge is in the log.
            BillingMessage::TurnSettled { request_id } => {
                set_holds.update(|holds| holds.retain(|hold| hold.request_id != request_id));
            }
        }
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

/// The members page: everyone in the organization, with their roles.
#[component]
fn MembersPage() -> impl IntoView {
    let members = Resource::new(|| (), |_| async { get_members().await });
    view! {
        <section class="card" aria-label="Members">
            <h1>"Members"</h1>
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || members.get().map(|result| match result {
                    Ok(members) => view! { <MemberTable members=members/> }.into_any(),
                    Err(error) => view! { <p class="error" role="alert">{error.to_string()}</p> }.into_any(),
                })}
            </Suspense>
        </section>
    }
}

#[component]
fn MemberTable(#[prop(into)] members: Vec<MemberView>) -> impl IntoView {
    view! {
        {if members.is_empty() {
            view! { <p class="muted">"No members."</p> }.into_any()
        } else {
            view! {
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Email"</th>
                            <th scope="col">"Role"</th>
                            <th scope="col">"Joined"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each=move || members.clone() key=|member| member.email.clone() let(member)>
                            <tr>
                                <td>{member.email.clone()}</td>
                                <td>{member.role.clone()}</td>
                                <td class="mono">{member.joined_at.clone()}</td>
                            </tr>
                        </For>
                    </tbody>
                </table>
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
#[component]
fn VerifyPage() -> impl IntoView {
    let (bundle, set_bundle) = signal(String::new());
    let (content_hash, set_content_hash) = signal(String::new());
    let (outcome, set_outcome) = signal(Option::<VerifyOutcome>::None);

    let ready = move || !(bundle.get().trim().is_empty() || content_hash.get().trim().is_empty());

    let verify = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
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
