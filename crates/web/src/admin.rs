//! The platform-admin pages: the deployment's surface.
//!
//! The dashboard's pages are a session's surface; these are the operator token's
//! (`OXSUM_ADMIN_TOKEN`). The token is not a cookie and no server function could
//! authorize with it, so the pages call `/api/v1/admin/*` directly from the browser —
//! the same endpoints a `curl` drives (docs/decisions.md, "an operator token rather
//! than a session"). The token itself is kept in `localStorage`, like the chat page's
//! key: it never enters a cookie and the server never holds it. A page that finds no
//! token asks for one; a `401` forgets it and asks again, which also covers a
//! deployment that never configured a token — its admin surface is closed and every
//! call is refused.
//!
//! SSR renders the gate and the page skeletons only: the data arrives with the browser.

use leptos::prelude::*;
use leptos_router::components::{A, Outlet};
use serde::Deserialize;

/// The operator token the layout holds, shared with the pages it renders.
///
/// `None` shows the token form instead of the page. Writing `None` forgets the token
/// everywhere — signal, storage and every open page flip back to the form — which is
/// what a `401` asks for.
#[derive(Clone, Copy)]
struct AdminToken(RwSignal<Option<String>>);

/// The token the pages attach, taken from context once.
fn admin_token() -> AdminToken {
    expect_context::<AdminToken>()
}

/// The `/admin` frame: navigation plus the token gate.
#[component]
pub fn AdminLayout() -> impl IntoView {
    let token = RwSignal::new(Option::<String>::None);
    provide_context(AdminToken(token));

    // The token is read on the client: SSR has no localStorage and always renders the
    // gate. A first paint behind a token that may not exist would flash the wrong thing.
    Effect::new(move |_| {
        #[cfg(feature = "hydrate")]
        token.set(browser::load_token());
        #[cfg(not(feature = "hydrate"))]
        let _ = token;
    });

    let (entered, set_entered) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_busy.set(true);
            set_error.set(None);
            let candidate = entered.get_untracked();
            // The token is proven, not stored: one real call decides whether the gate
            // opens, so a typo is "rejected" rather than a saved secret that never works.
            match browser::get::<Vec<ChannelView>>(&candidate, "/api/v1/admin/channels").await {
                Ok(_) => {
                    browser::save_token(&candidate);
                    token.set(Some(candidate));
                }
                Err(browser::FetchError::Unauthorized) => {
                    set_error.set(Some(
                        "The token was refused. Check it, or set OXSUM_ADMIN_TOKEN if the \
                         deployment has none."
                            .to_owned(),
                    ));
                }
                Err(browser::FetchError::Failed(message)) => {
                    set_error.set(Some(message));
                }
            }
            set_busy.set(false);
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = (entered, set_entered, set_error, set_busy);
    };

    view! {
        <div class="layout">
            <nav class="sidenav" aria-label="Platform admin">
                <div class="brand">"oxsum admin"</div>
                <A href="/admin/channels">"Channels & prices"</A>
                <A href="/admin/organizations">"Organizations"</A>
                <A href="/admin/in-flight">"In-flight"</A>
                <A href="/admin/anomalies">"Anomalies"</A>
                <A href="/admin/reconciliation">"Reconciliation"</A>
                <A href="/admin/closing">"Closing"</A>
                <A href="/dashboard">"Dashboard"</A>
            </nav>
            <main class="content">
                {move || {
                    if token.get().is_some() {
                        view! { <Outlet/> }.into_any()
                    } else {
                        view! {
                            <form class="card" method="post" on:submit=submit aria-label="Platform admin token">
                                <h1>"Platform admin"</h1>
                                <p class="muted">
                                    "The deployment's operator token opens this surface — the \
                                     same credential `/api/v1/admin` takes."
                                </p>
                                {move || {
                                    error.get().map(|message| {
                                        view! { <p class="error" role="alert">{message}</p> }
                                    })
                                }}
                                <label>
                                    "Operator token"
                                    <input
                                        type="password"
                                        name="token"
                                        autocomplete="off"
                                        required
                                        on:input=move |ev| set_entered.set(event_target_value(&ev))
                                    />
                                </label>
                                <button type="submit" prop:disabled=move || busy.get()>
                                    {move || if busy.get() { "Checking…" } else { "Enter" }}
                                </button>
                            </form>
                        }
                        .into_any()
                    }
                }}
            </main>
        </div>
    }
}

/// A channel as `GET /api/v1/admin/channels` returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelView {
    name: String,
    base_url: String,
    api_key_last4: String,
    created_at: String,
    models: Vec<ModelPriceView>,
}

/// An organization as `GET /api/v1/admin/organizations` returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationView {
    id: String,
    name: String,
    kind: String,
    members: i64,
    created_at: String,
    available_minor: i64,
    reserved_minor: i64,
    credit_limit_minor: i64,
    credit_used_minor: i64,
}

/// One page of organizations, as the endpoint answers it (issue #93): the rows and
/// where the walk resumes — `None` at the list's end. The cursor is opaque: echoed
/// back verbatim, never constructed.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationsPageView {
    organizations: Vec<OrganizationView>,
    next_cursor: Option<String>,
}

/// A closing record as `GET /api/v1/admin/closings` returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClosingView {
    organization: String,
    period: String,
    entry_count: i64,
    tree_size: i64,
    tree_root: String,
    trial_balance_root: String,
    seal_hash: String,
}

/// An anomalous turn as `GET /api/v1/admin/anomalies` returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnomalyView {
    organization: String,
    request_id: String,
    model: String,
    channel: String,
    price_version: i64,
    kind: String,
    charged_minor: i64,
    freeze_minor: i64,
    booked_on: String,
}

/// An unsettled hold as `GET /api/v1/admin/holds` returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InFlightHoldView {
    organization: String,
    request_id: String,
    model: String,
    channel: String,
    price_version: i64,
    freeze_minor: i64,
    opened_at: String,
    sweep_attempts: i64,
    last_error: Option<String>,
    dead_at: Option<String>,
}

/// One model's price at a version, as the admin API returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPriceView {
    model: String,
    version: i64,
    input_price_per_million: i64,
    output_price_per_million: i64,
    max_output_tokens: i64,
    created_at: String,
}

/// Runs an admin call and forgets the token when the surface refuses it: an expired or
/// wrong credential must not keep the gate open.
#[cfg(feature = "hydrate")]
async fn admin_call<T>(
    token: AdminToken,
    call: impl std::future::Future<Output = Result<T, browser::FetchError>>,
) -> Result<T, String> {
    match call.await {
        Ok(value) => Ok(value),
        Err(browser::FetchError::Unauthorized) => {
            browser::forget_token();
            token.0.set(None);
            Err("The token was refused.".to_owned())
        }
        Err(browser::FetchError::Failed(message)) => Err(message),
    }
}

/// The channels and prices page: what the gateway relays to, and what each model costs.
///
/// Lists every channel with its connection and the current version of each of its
/// models; creates or repoints a channel; appends a price version to a model; and
/// unfolds a channel's whole price history — the record a bill names when it says a
/// version. Nothing is ever rewritten or deleted: a connection moves forward and a
/// price list only grows, because an old bill has to stay checkable.
#[component]
pub fn AdminChannelsPage() -> impl IntoView {
    let token = admin_token();
    let channels = LocalResource::new(move || async move { load_channels(token).await });
    let notice = RwSignal::new(Option::<(String, &'static str)>::None);

    view! {
        <h1>"Channels & prices"</h1>
        <p class="muted">
            "The upstreams the gateway relays to and the price each model charges. A \
             repoint moves a channel's connection and leaves its prices; a price change \
             appends a version and never rewrites one, so a bill stays checkable."
        </p>
        {move || {
            notice
                .get()
                .map(|(message, class)| view! { <p class=class role="status">{message}</p> })
        }}
        <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
            {move || {
                channels.get().map(|result| match result {
                    Err(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
                    Ok(channels) if channels.is_empty() => view! {
                        <p class="muted">"No channels yet — add the first one below."</p>
                    }
                    .into_any(),
                    Ok(channels) => view! {
                        <div>
                            {channels
                                .into_iter()
                                .map(|channel| {
                                    view! { <ChannelCard channel=channel notice=notice/> }
                                })
                                .collect_view()}
                        </div>
                    }
                    .into_any(),
                })
            }}
        </Suspense>
        <ChannelForm channels=channels notice=notice/>
    }
}

/// The resource's read. `LocalResource` only ever runs in the browser; the SSR body is
/// a placeholder so the page compiles for the server too.
#[cfg(feature = "hydrate")]
async fn load_channels(token: AdminToken) -> Result<Vec<ChannelView>, String> {
    admin_call(
        token,
        browser::get(
            &token.0.get_untracked().unwrap_or_default(),
            "/api/v1/admin/channels",
        ),
    )
    .await
}

#[cfg(not(feature = "hydrate"))]
async fn load_channels(_token: AdminToken) -> Result<Vec<ChannelView>, String> {
    Err(String::new())
}

/// One channel: its connection, the current price of every model it serves, a form that
/// appends a version, and its history on request.
#[component]
fn ChannelCard(
    channel: ChannelView,
    notice: RwSignal<Option<(String, &'static str)>>,
) -> impl IntoView {
    let token = admin_token();
    let name = StoredValue::new(channel.name.clone());
    let (history, set_history) = signal(Option::<Vec<ModelPriceView>>::None);
    let (history_error, set_history_error) = signal(Option::<String>::None);

    let show_history = move |_| {
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_history_error.set(None);
            let path = format!("/api/v1/admin/channels/{}/prices", name.get_value());
            match admin_call(
                token,
                browser::get::<Vec<ModelPriceView>>(
                    &token.0.get_untracked().unwrap_or_default(),
                    &path,
                ),
            )
            .await
            {
                Ok(prices) => set_history.set(Some(prices)),
                Err(message) => set_history_error.set(Some(message)),
            }
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = (token.0, name, set_history, set_history_error);
    };

    view! {
        <section class="card">
            <h2>{channel.name.clone()}</h2>
            <p class="muted mono">
                {channel.base_url.clone()}
                " · key …"
                {channel.api_key_last4.clone()}
                " · since "
                {channel.created_at.clone()}
            </p>
            <table>
                <thead>
                    <tr>
                        <th>"Model"</th>
                        <th>"Version"</th>
                        <th class="num">"Input / M"</th>
                        <th class="num">"Output / M"</th>
                        <th class="num">"Max output"</th>
                    </tr>
                </thead>
                <tbody>
                    {channel
                        .models
                        .iter()
                        .map(|model| {
                            view! {
                                <tr>
                                    <td class="mono">{model.model.clone()}</td>
                                    <td class="mono">{"v"}{model.version}</td>
                                    <td class="mono num">{model.input_price_per_million}</td>
                                    <td class="mono num">{model.output_price_per_million}</td>
                                    <td class="mono num">{model.max_output_tokens}</td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
            <PriceForm channel=name notice=notice/>
            <button type="button" on:click=show_history>
                {move || {
                    if history.get().is_some() { "Refresh history" } else { "Show history" }
                }}
            </button>
            {move || {
                history_error
                    .get()
                    .map(|message| view! { <p class="error" role="alert">{message}</p> })
            }}
            {move || {
                history.get().map(|prices| {
                    view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>"Model"</th>
                                    <th>"Version"</th>
                                    <th class="num">"Input / M"</th>
                                    <th class="num">"Output / M"</th>
                                    <th class="num">"Max output"</th>
                                    <th>"Written"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {prices
                                    .into_iter()
                                    .map(|price| {
                                        view! {
                                            <tr>
                                                <td class="mono">{price.model}</td>
                                                <td class="mono">{"v"}{price.version}</td>
                                                <td class="mono num">{price.input_price_per_million}</td>
                                                <td class="mono num">{price.output_price_per_million}</td>
                                                <td class="mono num">{price.max_output_tokens}</td>
                                                <td class="mono">{price.created_at}</td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                    }
                })
            }}
        </section>
    }
}

/// Appends a price version for one of a channel's models.
#[component]
fn PriceForm(
    channel: StoredValue<String>,
    notice: RwSignal<Option<(String, &'static str)>>,
) -> impl IntoView {
    let token = admin_token();
    let (model, set_model) = signal(String::new());
    let (input, set_input) = signal(String::new());
    let (output, set_output) = signal(String::new());
    let (max, set_max) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_busy.set(true);
            set_error.set(None);
            notice.set(None);
            let (Ok(input_price), Ok(output_price), Ok(max_output)) = (
                input.get_untracked().parse::<i64>(),
                output.get_untracked().parse::<i64>(),
                max.get_untracked().parse::<i64>(),
            ) else {
                set_error.set(Some(
                    "The prices and the maximum are whole numbers.".to_owned(),
                ));
                set_busy.set(false);
                return;
            };
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Body {
                model: String,
                input_price_per_million: i64,
                output_price_per_million: i64,
                max_output_tokens: i64,
            }
            let body = Body {
                model: model.get_untracked(),
                input_price_per_million: input_price,
                output_price_per_million: output_price,
                max_output_tokens: max_output,
            };
            let label = body.model.clone();
            let path = format!("/api/v1/admin/channels/{}/prices", channel.get_value());
            match admin_call(
                token,
                browser::post(&token.0.get_untracked().unwrap_or_default(), &path, &body),
            )
            .await
            {
                Ok(answer) => {
                    let version = answer.get("version").and_then(|v| v.as_i64());
                    notice.set(Some((
                        match version {
                            Some(version) => format!("{label} is now at v{version}."),
                            None => format!("{label} saved."),
                        },
                        "success",
                    )));
                }
                Err(message) => set_error.set(Some(message)),
            }
            set_busy.set(false);
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = (
            channel, notice, token.0, model, input, output, max, set_model, set_input, set_output,
            set_max, set_error, set_busy,
        );
    };

    view! {
        <form class="row" method="post" on:submit=submit aria-label="Append a price">
            <label>
                "Model"
                <input type="text" required on:input=move |ev| set_model.set(event_target_value(&ev))/>
            </label>
            <label>
                "Input / M"
                <input
                    type="text"
                    inputmode="numeric"
                    required
                    on:input=move |ev| set_input.set(event_target_value(&ev))
                />
            </label>
            <label>
                "Output / M"
                <input
                    type="text"
                    inputmode="numeric"
                    required
                    on:input=move |ev| set_output.set(event_target_value(&ev))
                />
            </label>
            <label>
                "Max output"
                <input
                    type="text"
                    inputmode="numeric"
                    required
                    on:input=move |ev| set_max.set(event_target_value(&ev))
                />
            </label>
            <button type="submit" prop:disabled=move || busy.get()>"Append price"</button>
        </form>
        {move || {
            error
                .get()
                .map(|message| view! { <p class="error" role="alert">{message}</p> })
        }}
    }
}

/// Creates a channel, or repoints the connection of one that exists.
#[component]
fn ChannelForm(
    channels: LocalResource<Result<Vec<ChannelView>, String>>,
    notice: RwSignal<Option<(String, &'static str)>>,
) -> impl IntoView {
    let token = admin_token();
    let (name, set_name) = signal(String::new());
    let (base_url, set_base_url) = signal(String::new());
    let (api_key, set_api_key) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_busy.set(true);
            set_error.set(None);
            notice.set(None);
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Body {
                name: String,
                base_url: String,
                api_key: String,
            }
            let body = Body {
                name: name.get_untracked(),
                base_url: base_url.get_untracked(),
                api_key: api_key.get_untracked(),
            };
            let label = body.name.clone();
            match admin_call(
                token,
                browser::post(
                    &token.0.get_untracked().unwrap_or_default(),
                    "/api/v1/admin/channels",
                    &body,
                ),
            )
            .await
            {
                Ok(_) => {
                    notice.set(Some((format!("{label} saved."), "success")));
                    channels.refetch();
                }
                Err(message) => set_error.set(Some(message)),
            }
            set_busy.set(false);
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = (
            channels,
            notice,
            token.0,
            name,
            base_url,
            api_key,
            set_name,
            set_base_url,
            set_api_key,
            set_error,
            set_busy,
        );
    };

    view! {
        <section class="card">
            <h2>"Add or repoint a channel"</h2>
            <p class="muted">
                "A name that already exists is repointed to the new address and key; its \
                 price history stays. The upstream credential is stored sealed and only \
                 its last four characters are ever read back."
            </p>
            <form class="row" method="post" on:submit=submit aria-label="Add or repoint a channel">
                <label>
                    "Name"
                    <input type="text" required on:input=move |ev| set_name.set(event_target_value(&ev))/>
                </label>
                <label>
                    "Base URL"
                    <input
                        type="text"
                        required
                        placeholder="https://…"
                        on:input=move |ev| set_base_url.set(event_target_value(&ev))
                    />
                </label>
                <label>
                    "Upstream API key"
                    <input
                        type="password"
                        autocomplete="off"
                        required
                        on:input=move |ev| set_api_key.set(event_target_value(&ev))
                    />
                </label>
                <button type="submit" prop:disabled=move || busy.get()>"Save channel"</button>
            </form>
            {move || {
                error
                    .get()
                    .map(|message| view! { <p class="error" role="alert">{message}</p> })
            }}
        </section>
    }
}

/// The organizations page: every organization, its headcount, and what its wallet
/// shows. Money moves on a different surface — top-ups and adjustments are #60 — so
/// this page reads and never writes.
#[component]
pub fn AdminOrganizationsPage() -> impl IntoView {
    let token = admin_token();
    let organizations =
        LocalResource::new(move || async move { load_organizations(token, None).await });
    let notice = RwSignal::new(Option::<(String, &'static str)>::None);
    // The pages the walk already loaded past the first, and where it resumes:
    // "Load more" appends, an adjustment's refetch starts the walk over (issue #93).
    let extra = RwSignal::new(Vec::<OrganizationView>::new());
    let resumed = RwSignal::new(Option::<String>::None);
    let more_busy = RwSignal::new(false);
    let more_error = RwSignal::new(Option::<String>::None);
    let on_adjusted = Callback::new(move |_: ()| {
        extra.set(Vec::new());
        resumed.set(None);
        organizations.refetch();
    });
    let load_more = move |cursor: String| {
        let _ = cursor;
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            more_busy.set(true);
            more_error.set(None);
            match load_organizations(token, Some(cursor)).await {
                Ok(page) => {
                    resumed.set(page.next_cursor.clone());
                    extra.update(|rows| rows.extend(page.organizations));
                }
                Err(message) => more_error.set(Some(message)),
            }
            more_busy.set(false);
        });
    };

    view! {
        <h1>"Organizations"</h1>
        <p class="muted">
            "Every organization the ledger holds money for, oldest first. Available is              settled minus unsettled holds; frozen is the sum of the organization's              outstanding holds. An adjustment is a signed amount in credits — a minus              sign deducts — and it needs a reason, which the entry carries."
        </p>
        {move || {
            notice
                .get()
                .map(|(message, class)| view! { <p class=class role="status">{message}</p> })
        }}
        <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
            {move || {
                organizations.get().map(|result| match result {
                    Err(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
                    Ok(page) => {
                        let mut rows = page.organizations.clone();
                        rows.extend(extra.get());
                        // The walk's resume point: the first page's own cursor until
                        // a page has been appended, the appended pages' after.
                        let next = if extra.get().is_empty() {
                            page.next_cursor.clone()
                        } else {
                            resumed.get()
                        };
                        view! {
                            {if rows.is_empty() {
                                view! { <p class="muted">"No organizations yet."</p> }.into_any()
                            } else {
                                view! {
                                    <table>
                                        <thead>
                                            <tr>
                                                <th>"Name"</th>
                                                <th>"Kind"</th>
                                                <th class="num">"Members"</th>
                                                <th class="num">"Available"</th>
                                                <th class="num">"Frozen"</th>
                                                <th class="num">"Credit used / limit"</th>
                                                <th>"Created"</th>
                                                <th>"Adjust · Credit"</th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {rows
                                                .into_iter()
                                                .map(|organization| {
                                                    view! {
                                                        <tr>
                                                            <td>{organization.name.clone()}</td>
                                                            <td>{organization.kind.clone()}</td>
                                                            <td class="num">{organization.members}</td>
                                                            <td class="mono num">
                                                                {crate::app::credits(organization.available_minor)}
                                                            </td>
                                                            <td class="mono num pending">
                                                                {crate::app::credits(organization.reserved_minor)}
                                                            </td>
                                                            <td class="mono num">
                                                                {format!(
                                                                    "{} / {}",
                                                                    crate::app::credits(organization.credit_used_minor),
                                                                    crate::app::credits(organization.credit_limit_minor),
                                                                )}
                                                            </td>
                                                            <td class="mono">{organization.created_at.clone()}</td>
                                                            <td>
                                                                <AdjustForm
                                                                    organization=organization.clone()
                                                                    on_adjusted=on_adjusted
                                                                    notice=notice
                                                                />
                                                                <CreditLimitForm
                                                                    organization=organization
                                                                    on_adjusted=on_adjusted
                                                                    notice=notice
                                                                />
                                                            </td>
                                                        </tr>
                                                    }
                                                })
                                                .collect_view()}
                                        </tbody>
                                    </table>
                                }
                                    .into_any()
                            }}
                            {next.map(|cursor| {
                                view! {
                                    <p>
                                        <button
                                            type="button"
                                            prop:disabled=move || more_busy.get()
                                            on:click=move |_| load_more(cursor.clone())
                                        >
                                            {move || {
                                                if more_busy.get() { "Loading…" } else { "Load more" }
                                            }}
                                        </button>
                                    </p>
                                }
                            })}
                            {move || {
                                more_error
                                    .get()
                                    .map(|message| view! { <p class="error" role="alert">{message}</p> })
                            }}
                        }
                            .into_any()
                    }
                })
            }}
        </Suspense>
    }
}

/// A credit amount like `10` or `-2.50`: `parse_credits` handles the magnitude and a
/// leading `-` makes it a deduction — kept out of `parse_credits` itself, because a
/// negative top-up is not a thing the chat page should accept.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
fn parse_signed_credits(text: &str) -> Result<i64, ()> {
    match text.trim().strip_prefix('-') {
        Some(rest) => crate::chat::parse_credits(rest).and_then(|m| m.checked_neg().ok_or(())),
        None => crate::chat::parse_credits(text),
    }
}

/// One row's adjust form: a signed amount in credits and the reason the money moved.
/// The reason is required because it becomes the entry's description — part of what
/// a bill's proof covers.
#[component]
fn AdjustForm(
    organization: OrganizationView,
    on_adjusted: Callback<()>,
    notice: RwSignal<Option<(String, &'static str)>>,
) -> impl IntoView {
    let token = admin_token();
    let (amount, set_amount) = signal(String::new());
    let (reason, set_reason) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        {
            // The handler must stay FnMut: the async block takes clones, not
            // `organization` itself.
            let organization_id = organization.id.clone();
            let organization_name = organization.name.clone();
            leptos::task::spawn_local(async move {
                set_busy.set(true);
                set_error.set(None);
                notice.set(None);
                // A leading minus deducts; parse_credits keeps money in integers.
                let amount_minor = match parse_signed_credits(&amount.get_untracked()) {
                    Ok(minor) if minor != 0 => minor,
                    _ => {
                        set_error.set(Some("Enter a nonzero amount like 10 or -2.50.".to_owned()));
                        set_busy.set(false);
                        return;
                    }
                };
                if reason.get_untracked().trim().is_empty() {
                    set_error.set(Some("An adjustment needs a reason.".to_owned()));
                    set_busy.set(false);
                    return;
                }
                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Body {
                    amount_minor: i64,
                    reason: String,
                    idempotency_key: String,
                }
                let body = Body {
                    amount_minor,
                    reason: reason.get_untracked(),
                    // Fresh per submit: a retried *call* replays, a second click is a second
                    // adjustment.
                    idempotency_key: uuid::Uuid::new_v4().to_string(),
                };
                let path = format!(
                    "/api/v1/admin/organizations/{}/adjustments",
                    organization_id
                );
                match admin_call(
                    token,
                    browser::post(&token.0.get_untracked().unwrap_or_default(), &path, &body),
                )
                .await
                {
                    Ok(answer) => {
                        let name = organization_name;
                        let available = answer
                            .get("availableMinor")
                            .and_then(|v| v.as_i64())
                            .map(crate::app::credits)
                            .unwrap_or_default();
                        notice.set(Some((
                            format!("Adjusted {name}; it now holds {available} available."),
                            "success",
                        )));
                        set_amount.set(String::new());
                        set_reason.set(String::new());
                        on_adjusted.run(());
                    }
                    Err(message) => set_error.set(Some(message)),
                }
                set_busy.set(false);
            });
        }
        #[cfg(not(feature = "hydrate"))]
        let _ = (
            organization.id.as_str(),
            on_adjusted,
            &token,
            notice,
            amount,
            reason,
            set_amount,
            set_reason,
            set_error,
            set_busy,
        );
    };

    view! {
        <form class="row" method="post" on:submit=submit aria-label="Adjust the balance">
            <label>
                "Amount"
                <input
                    type="text"
                    inputmode="decimal"
                    name="amount"
                    placeholder="10 or -2.50"
                    required
                    prop:value=move || amount.get()
                    on:input=move |ev| set_amount.set(event_target_value(&ev))
                />
            </label>
            <label>
                "Reason"
                <input
                    type="text"
                    name="reason"
                    required
                    prop:value=move || reason.get()
                    on:input=move |ev| set_reason.set(event_target_value(&ev))
                />
            </label>
            <button type="submit" prop:disabled=move || busy.get()>
                {move || if busy.get() { "Booking…" } else { "Apply" }}
            </button>
            {move || error.get().map(|message| view! { <p class="error" role="alert">{message}</p> })}
        </form>
    }
}

/// One row's credit-limit form: the whole line the organization may draw, in
/// credits — absolute, not a delta. Shrinking below what is still drawn is a
/// validation error the server answers; the form only reports it.
#[component]
fn CreditLimitForm(
    organization: OrganizationView,
    on_adjusted: Callback<()>,
    notice: RwSignal<Option<(String, &'static str)>>,
) -> impl IntoView {
    let token = admin_token();
    let (limit, set_limit) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        {
            let organization_id = organization.id.clone();
            let organization_name = organization.name.clone();
            leptos::task::spawn_local(async move {
                set_busy.set(true);
                set_error.set(None);
                notice.set(None);
                let credit_limit_minor = match crate::chat::parse_credits(&limit.get_untracked()) {
                    Ok(minor) => minor,
                    Err(_) => {
                        set_error.set(Some("Enter a credit limit like 10 or 0.".to_owned()));
                        set_busy.set(false);
                        return;
                    }
                };
                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Body {
                    credit_limit_minor: i64,
                    idempotency_key: String,
                }
                let body = Body {
                    credit_limit_minor,
                    // Fresh per submit: a retried *call* replays, a second click is a
                    // second change.
                    idempotency_key: uuid::Uuid::new_v4().to_string(),
                };
                let path = format!("/api/v1/admin/organizations/{organization_id}");
                match admin_call(
                    token,
                    browser::patch(&token.0.get_untracked().unwrap_or_default(), &path, &body),
                )
                .await
                {
                    Ok(answer) => {
                        let name = organization_name;
                        let used = answer
                            .get("creditUsedMinor")
                            .and_then(|v| v.as_i64())
                            .map(crate::app::credits)
                            .unwrap_or_default();
                        let limit = answer
                            .get("creditLimitMinor")
                            .and_then(|v| v.as_i64())
                            .map(crate::app::credits)
                            .unwrap_or_default();
                        notice.set(Some((
                            format!("Credit line for {name} is now {used} of {limit} drawn."),
                            "success",
                        )));
                        set_limit.set(String::new());
                        on_adjusted.run(());
                    }
                    Err(message) => set_error.set(Some(message)),
                }
                set_busy.set(false);
            });
        }
        #[cfg(not(feature = "hydrate"))]
        let _ = (
            organization.id.as_str(),
            on_adjusted,
            &token,
            notice,
            limit,
            set_limit,
            set_error,
            set_busy,
        );
    };

    view! {
        <form class="row" method="post" on:submit=submit aria-label="Set the credit limit">
            <label>
                "Credit limit"
                <input
                    type="text"
                    inputmode="decimal"
                    name="creditLimit"
                    placeholder="10"
                    required
                    prop:value=move || limit.get()
                    on:input=move |ev| set_limit.set(event_target_value(&ev))
                />
            </label>
            <button type="submit" prop:disabled=move || busy.get()>
                {move || if busy.get() { "Setting…" } else { "Set" }}
            </button>
            {move || error.get().map(|message| view! { <p class="error" role="alert">{message}</p> })}
        </form>
    }
}

/// The resource's read. `LocalResource` only ever runs in the browser; the SSR body is
/// a placeholder so the page compiles for the server too.
#[cfg(feature = "hydrate")]
async fn load_organizations(
    token: AdminToken,
    cursor: Option<String>,
) -> Result<OrganizationsPageView, String> {
    let path = match cursor {
        Some(cursor) => format!("/api/v1/admin/organizations?cursor={cursor}"),
        None => "/api/v1/admin/organizations".to_owned(),
    };
    admin_call(
        token,
        browser::get(&token.0.get_untracked().unwrap_or_default(), &path),
    )
    .await
}

#[cfg(not(feature = "hydrate"))]
async fn load_organizations(
    _token: AdminToken,
    _cursor: Option<String>,
) -> Result<OrganizationsPageView, String> {
    Err(String::new())
}

/// The in-flight page: every hold the platform is currently reserving, across all
/// organizations — what the sweeper watches and the ledgers still freeze.
#[component]
pub fn AdminInFlightPage() -> impl IntoView {
    let token = admin_token();
    let holds = LocalResource::new(move || async move { load_holds(token).await });

    view! {
        <h1>"In-flight requests"</h1>
        <p class="muted">
            "Every unsettled hold, globally and newest first. A hold that settled is              off the list — the ledger is the source of truth, and the sweeper clears              rows whose holds are gone. A hold older than the timeout is one the              sweeper is about to release as `swept`."
        </p>
        <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
            {move || {
                holds.get().map(|result| match result {
                    Err(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
                    Ok(holds) if holds.is_empty() => view! {
                        <p class="muted">"Nothing in flight."</p>
                    }
                    .into_any(),
                    Ok(holds) => view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>"Organization"</th>
                                    <th>"Request"</th>
                                    <th>"Model"</th>
                                    <th>"Channel"</th>
                                    <th>"Price"</th>
                                    <th class="num">"Frozen"</th>
                                    <th>"Opened"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {holds
                                    .into_iter()
                                    .map(|hold| {
                                        view! {
                                            <tr>
                                                <td>{hold.organization}</td>
                                                <td class="mono">{hold.request_id}</td>
                                                <td class="mono">{hold.model}</td>
                                                <td>{hold.channel}</td>
                                                <td class="mono">{"v"}{hold.price_version}</td>
                                                <td class="mono num pending">
                                                    {crate::app::credits(hold.freeze_minor)}
                                                </td>
                                                <td class="mono">
                                                    {hold.opened_at}
                                                    {hold
                                                        .dead_at
                                                        .map(|_| {
                                                            view! {
                                                                <span class="error" title=hold.last_error.clone()>
                                                                    {format!(" dead ({} failed sweeps)", hold.sweep_attempts)}
                                                                </span>
                                                            }
                                                        })}
                                                </td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                    }
                    .into_any(),
                })
            }}
        </Suspense>
    }
}

/// The resource's read. `LocalResource` only ever runs in the browser; the SSR body is
/// a placeholder so the page compiles for the server too.
#[cfg(feature = "hydrate")]
async fn load_holds(token: AdminToken) -> Result<Vec<InFlightHoldView>, String> {
    admin_call(
        token,
        browser::get(
            &token.0.get_untracked().unwrap_or_default(),
            "/api/v1/admin/holds",
        ),
    )
    .await
}

#[cfg(not(feature = "hydrate"))]
async fn load_holds(_token: AdminToken) -> Result<Vec<InFlightHoldView>, String> {
    Err(String::new())
}

/// The anomalies page: the settled turns that did not price cleanly — `capped`,
/// `estimated`, `client_cancelled`, `swept` — across all organizations, with a
/// per-channel summary so where the platform loses money upstream is the first thing
/// on the page.
#[component]
pub fn AdminAnomaliesPage() -> impl IntoView {
    let token = admin_token();
    let anomalies = LocalResource::new(move || async move { load_anomalies(token).await });

    view! {
        <h1>"Anomalies"</h1>
        <p class="muted">
            "The turns that did not price cleanly: an estimate where upstream reported \
             nothing, a caller who left mid-stream, a charge that hit the freeze's \
             ceiling, a hold the sweeper had to release. Every row is what that turn's \
             bill proves — the records are the ledgers' own settlement descriptions."
        </p>
        <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
            {move || {
                anomalies.get().map(|result| match result {
                    Err(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
                    Ok(anomalies) if anomalies.is_empty() => view! {
                        <p class="muted">"No anomalies — every turn priced cleanly."</p>
                    }
                    .into_any(),
                    Ok(anomalies) => {
                        // Where the platform loses money, per channel: turns and the
                        // charged total each.
                        let mut channels: Vec<(String, u64, i64)> = Vec::new();
                        for anomaly in &anomalies {
                            match channels
                                .iter_mut()
                                .find(|(name, _, _)| name == &anomaly.channel)
                            {
                                Some((_, turns, charged)) => {
                                    *turns += 1;
                                    *charged += anomaly.charged_minor;
                                }
                                None => channels.push((
                                    anomaly.channel.clone(),
                                    1,
                                    anomaly.charged_minor,
                                )),
                            }
                        }
                        view! {
                            <table>
                                <thead>
                                    <tr>
                                        <th>"Channel"</th>
                                        <th class="num">"Anomalous turns"</th>
                                        <th class="num">"Charged"</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {channels
                                        .into_iter()
                                        .map(|(channel, turns, charged)| {
                                            view! {
                                                <tr>
                                                    <td>{channel}</td>
                                                    <td class="num">{turns}</td>
                                                    <td class="mono num">{crate::app::credits(charged)}</td>
                                                </tr>
                                            }
                                        })
                                        .collect_view()}
                                </tbody>
                            </table>
                            <table>
                                <thead>
                                    <tr>
                                        <th>"Organization"</th>
                                        <th>"Request"</th>
                                        <th>"Model"</th>
                                        <th>"Channel"</th>
                                        <th>"Kind"</th>
                                        <th class="num">"Price"</th>
                                        <th class="num">"Charged"</th>
                                        <th class="num">"Frozen"</th>
                                        <th>"Booked"</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {anomalies
                                        .into_iter()
                                        .map(|anomaly| {
                                            view! {
                                                <tr>
                                                    <td>{anomaly.organization}</td>
                                                    <td class="mono">{anomaly.request_id}</td>
                                                    <td class="mono">{anomaly.model}</td>
                                                    <td>{anomaly.channel}</td>
                                                    <td class="mono">{anomaly.kind}</td>
                                                    <td class="mono num">
                                                        {"v"}{anomaly.price_version}
                                                    </td>
                                                    <td class="mono num">
                                                        {crate::app::credits(anomaly.charged_minor)}
                                                    </td>
                                                    <td class="mono num">
                                                        {crate::app::credits(anomaly.freeze_minor)}
                                                    </td>
                                                    <td class="mono">{anomaly.booked_on}</td>
                                                </tr>
                                            }
                                        })
                                        .collect_view()}
                                </tbody>
                            </table>
                        }
                        .into_any()
                    }
                })
            }}
        </Suspense>
    }
}

/// The resource's read. `LocalResource` only ever runs in the browser; the SSR body is
/// a placeholder so the page compiles for the server too.
#[cfg(feature = "hydrate")]
async fn load_anomalies(token: AdminToken) -> Result<Vec<AnomalyView>, String> {
    admin_call(
        token,
        browser::get(
            &token.0.get_untracked().unwrap_or_default(),
            "/api/v1/admin/anomalies",
        ),
    )
    .await
}

#[cfg(not(feature = "hydrate"))]
async fn load_anomalies(_token: AdminToken) -> Result<Vec<AnomalyView>, String> {
    Err(String::new())
}

/// One drift class as `GET /api/v1/admin/reconciliation` returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriftClassView {
    class: String,
    count: i64,
    samples: Vec<DriftSampleView>,
}

/// One offending identifier inside a drift class.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriftSampleView {
    organization: String,
    detail: String,
}

/// The reconciliation report as the endpoint returns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconciliationView {
    checked_at: String,
    organizations: i64,
    clean: bool,
    classes: Vec<DriftClassView>,
}

/// A drift class's label as a sentence.
fn drift_label(class: &str) -> &'static str {
    match class {
        "usage_orphans" => "Usage rows with no settlement entry",
        "settlements_unrecorded" => "Settled turns that wrote no usage row",
        "deposits_unbooked" => "Credited deposits with no ledger entry",
        "deposits_mismatched" => "Deposits the rail paid a different amount on",
        "deposits_stuck" => "Deposits confirmed but never credited or reversed",
        "watches_orphaned" => "Hold watches with no live reservation",
        "holds_unwatched" => "Pending holds the sweeper cannot see",
        "holds_dead_lettered" => "Holds whose sweep failed ten times — an operator's problem",
        "log_gaps" => "Log positions that are not dense",
        _ => "Unclassified drift",
    }
}

/// The reconciliation page: the drift between the ledgers and the tables that
/// project them, one class at a time. The report is read-only — every class
/// is a thing for an operator to look into, not a state the scan repairs.
#[component]
pub fn AdminReconciliationPage() -> impl IntoView {
    let token = admin_token();
    let report = LocalResource::new(move || async move { load_reconciliation(token).await });

    view! {
        <h1>"Reconciliation"</h1>
        <p class="muted">
            "The books against the projections: a usage row with no settlement entry, \
             a settled turn that wrote no record, a deposit that claims a credit the \
             ledger never posted, a hold the sweeper cannot see. The scan reports and \
             never repairs — every row below is for an operator to act on."
        </p>
        <Suspense fallback=move || view! { <p class="muted">"Scanning…"</p> }>
            {move || {
                report.get().map(|result| match result {
                    Err(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
                    Ok(report) => {
                        let drift: Vec<_> =
                            report.classes.iter().filter(|class| class.count > 0).collect();
                        view! {
                            <p class="muted">
                                {format!(
                                    "{} organization{} scanned at {}.",
                                    report.organizations,
                                    if report.organizations == 1 { "" } else { "s" },
                                    report.checked_at
                                )}
                            </p>
                            {if report.clean {
                                view! {
                                    <p class="muted">"Clean — the projections match the books."</p>
                                }
                                .into_any()
                            } else {
                                view! {
                                    <table>
                                        <thead>
                                            <tr>
                                                <th>"Drift"</th>
                                                <th class="num">"Count"</th>
                                                <th>"Samples"</th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {drift
                                                .into_iter()
                                                .map(|class| {
                                                    view! {
                                                        <tr>
                                                            <td>{drift_label(&class.class)}</td>
                                                            <td class="num">{class.count}</td>
                                                            <td>
                                                                {class
                                                                    .samples
                                                                    .iter()
                                                                    .map(|sample| {
                                                                        view! {
                                                                            <div>
                                                                                <span class="mono">{sample.detail.clone()}</span>
                                                                                " "
                                                                                <span class="muted">
                                                                                    {"("}{sample.organization.clone()}{")"}
                                                                                </span>
                                                                            </div>
                                                                        }
                                                                    })
                                                                    .collect_view()}
                                                            </td>
                                                        </tr>
                                                    }
                                                })
                                                .collect_view()}
                                        </tbody>
                                    </table>
                                }
                                .into_any()
                            }}
                        }
                        .into_any()
                    }
                })
            }}
        </Suspense>
    }
}

/// The resource's read. `LocalResource` only ever runs in the browser; the SSR body is
/// a placeholder so the page compiles for the server too.
#[cfg(feature = "hydrate")]
async fn load_reconciliation(token: AdminToken) -> Result<ReconciliationView, String> {
    admin_call(
        token,
        browser::get(
            &token.0.get_untracked().unwrap_or_default(),
            "/api/v1/admin/reconciliation",
        ),
    )
    .await
}

#[cfg(not(feature = "hydrate"))]
async fn load_reconciliation(_token: AdminToken) -> Result<ReconciliationView, String> {
    Err(String::new())
}

/// The closing page: seal a finished month across every organization's ledger, and
/// the closing records the ledgers hold — each a seal that commits to the log's
/// tree head and the period's closing trial balance, chained onto the seal before
/// it.
#[component]
pub fn AdminClosingPage() -> impl IntoView {
    let token = admin_token();
    let closings = LocalResource::new(move || async move { load_closings(token).await });
    let notice = RwSignal::new(Option::<(String, &'static str)>::None);

    view! {
        <h1>"Closing"</h1>
        <p class="muted">
            "Closing a month seals it in every organization's ledger: no entry may \
             carry a booking date in it again, and the seal — the log's tree head and \
             the month's closing trial balance, chained onto the seal before it — is \
             the closing record. Only a month that has fully ended can close."
        </p>
        {move || {
            notice
                .get()
                .map(|(message, class)| view! { <p class=class role="status">{message}</p> })
        }}
        <ClosingForm closings=closings notice=notice/>
        <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
            {move || {
                closings.get().map(|result| match result {
                    Err(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
                    Ok(closings) if closings.is_empty() => view! {
                        <p class="muted">"No month has been closed yet."</p>
                    }
                    .into_any(),
                    Ok(closings) => view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>"Month"</th>
                                    <th>"Organization"</th>
                                    <th class="num">"Entries"</th>
                                    <th class="num">"Log size"</th>
                                    <th>"Tree head"</th>
                                    <th>"Trial balance"</th>
                                    <th>"Seal"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {closings
                                    .into_iter()
                                    .map(|closing| {
                                        view! {
                                            <tr>
                                                <td class="mono">{closing.period}</td>
                                                <td>{closing.organization}</td>
                                                <td class="num">{closing.entry_count}</td>
                                                <td class="num">{closing.tree_size}</td>
                                                <td class="mono">{closing.tree_root}</td>
                                                <td class="mono">{closing.trial_balance_root}</td>
                                                <td class="mono">{closing.seal_hash}</td>
                                            </tr>
                                        }
                                    })
                                    .collect_view()}
                            </tbody>
                        </table>
                    }
                    .into_any()
                })
            }}
        </Suspense>
    }
}

/// The resource's read. `LocalResource` only ever runs in the browser; the SSR body is
/// a placeholder so the page compiles for the server too.
#[cfg(feature = "hydrate")]
async fn load_closings(token: AdminToken) -> Result<Vec<ClosingView>, String> {
    admin_call(
        token,
        browser::get(
            &token.0.get_untracked().unwrap_or_default(),
            "/api/v1/admin/closings",
        ),
    )
    .await
}

#[cfg(not(feature = "hydrate"))]
async fn load_closings(_token: AdminToken) -> Result<Vec<ClosingView>, String> {
    Err(String::new())
}

/// The close-a-month form: `YYYY-MM`, validated client-side for shape only — the
/// server refuses a month that has not fully ended, and re-closing one is
/// idempotent.
#[component]
fn ClosingForm(
    closings: LocalResource<Result<Vec<ClosingView>, String>>,
    notice: RwSignal<Option<(String, &'static str)>>,
) -> impl IntoView {
    let token = admin_token();
    let (month, set_month) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_busy.set(true);
            set_error.set(None);
            notice.set(None);
            let month = month.get_untracked();
            if month.len() != 7
                || month.as_bytes().get(4) != Some(&b'-')
                || !month
                    .chars()
                    .enumerate()
                    .all(|(i, c)| i == 4 || c.is_ascii_digit())
            {
                set_error.set(Some("The month is YYYY-MM.".to_owned()));
                set_busy.set(false);
                return;
            }
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Body {
                month: String,
            }
            match admin_call(
                token,
                browser::post(
                    &token.0.get_untracked().unwrap_or_default(),
                    "/api/v1/admin/closings",
                    &Body {
                        month: month.clone(),
                    },
                ),
            )
            .await
            {
                Ok(answer) => {
                    let count = answer.as_array().map(|rows| rows.len()).unwrap_or(0);
                    notice.set(Some((
                        format!("{month} closed — {count} closing record(s)."),
                        "success",
                    )));
                    closings.refetch();
                }
                Err(message) => set_error.set(Some(message)),
            }
            set_busy.set(false);
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = (
            month, set_month, set_error, set_busy, notice, closings, token.0,
        );
    };

    view! {
        <form class="row" method="post" on:submit=submit aria-label="Close a month">
            <label>
                "Month"
                <input
                    type="month"
                    required
                    on:input=move |ev| set_month.set(event_target_value(&ev))
                />
            </label>
            <button type="submit" disabled=move || busy.get()>
                {move || if busy.get() { "Closing…" } else { "Close month" }}
            </button>
        </form>
        {move || {
            error
                .get()
                .map(|message| view! { <p class="error" role="alert">{message}</p> })
        }}
    }
}

/// Browser-only calls to the admin endpoints, carrying the operator token.
#[cfg(feature = "hydrate")]
mod browser {
    use gloo_net::http::Request;
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};

    const TOKEN_STORAGE: &str = "oxsum-admin-token";

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn load_token() -> Option<String> {
        storage()?.get_item(TOKEN_STORAGE).ok()?
    }

    pub fn save_token(token: &str) {
        if let Some(storage) = storage() {
            let _ = storage.set_item(TOKEN_STORAGE, token);
        }
    }

    pub fn forget_token() {
        if let Some(storage) = storage() {
            let _ = storage.remove_item(TOKEN_STORAGE);
        }
    }

    /// How an admin call failed: the credential was refused, or the call itself did not
    /// complete. The first forgets the token; the second is reported.
    pub enum FetchError {
        Unauthorized,
        Failed(String),
    }

    /// Reads the server's error body when it carries one, else the status.
    async fn failure(response: gloo_net::http::Response) -> FetchError {
        match response.status() {
            401 => FetchError::Unauthorized,
            status => {
                let body = response.text().await.unwrap_or_default();
                FetchError::Failed(if body.is_empty() {
                    format!("the request failed ({status})")
                } else {
                    body
                })
            }
        }
    }

    pub async fn get<T: DeserializeOwned>(token: &str, path: &str) -> Result<T, FetchError> {
        let response = Request::get(path)
            .header("Authorization", &format!("Bearer {token}"))
            .send()
            .await
            .map_err(|_| FetchError::Failed("the server could not be reached".to_owned()))?;
        if !response.ok() {
            return Err(failure(response).await);
        }
        response
            .json::<Envelope<T>>()
            .await
            .map(|envelope| envelope.data)
            .map_err(|_| FetchError::Failed("the answer could not be read".to_owned()))
    }

    /// The success envelope every `/api/v1` answer wears: `{ "data": ... }`.
    #[derive(Deserialize)]
    struct Envelope<T> {
        data: T,
    }

    /// POSTs a JSON body and answers what it answered inside the envelope, unparsed:
    /// a price append carries the new `version`, a channel save the channel itself.
    pub async fn post<B: Serialize>(
        token: &str,
        path: &str,
        body: &B,
    ) -> Result<serde_json::Value, FetchError> {
        let response = Request::post(path)
            .header("Authorization", &format!("Bearer {token}"))
            .json(body)
            .map_err(|_| FetchError::Failed("could not build the request".to_owned()))?
            .send()
            .await
            .map_err(|_| FetchError::Failed("the server could not be reached".to_owned()))?;
        if !response.ok() {
            return Err(failure(response).await);
        }
        response
            .json::<Envelope<serde_json::Value>>()
            .await
            .map(|envelope| envelope.data)
            .map_err(|_| FetchError::Failed("the answer could not be read".to_owned()))
    }

    /// PATCHes a JSON body — the credit-limit write's method — and answers the
    /// envelope's data unparsed, like `post`.
    pub async fn patch<B: Serialize>(
        token: &str,
        path: &str,
        body: &B,
    ) -> Result<serde_json::Value, FetchError> {
        let response = Request::patch(path)
            .header("Authorization", &format!("Bearer {token}"))
            .json(body)
            .map_err(|_| FetchError::Failed("could not build the request".to_owned()))?
            .send()
            .await
            .map_err(|_| FetchError::Failed("the server could not be reached".to_owned()))?;
        if !response.ok() {
            return Err(failure(response).await);
        }
        response
            .json::<Envelope<serde_json::Value>>()
            .await
            .map(|envelope| envelope.data)
            .map_err(|_| FetchError::Failed("the answer could not be read".to_owned()))
    }
}
