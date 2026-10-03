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
    name: String,
    kind: String,
    members: i64,
    created_at: String,
    available_minor: i64,
    reserved_minor: i64,
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
    let organizations = LocalResource::new(move || async move { load_organizations(token).await });

    view! {
        <h1>"Organizations"</h1>
        <p class="muted">
            "Every organization the ledger holds money for, oldest first. Available is              settled minus unsettled holds; frozen is the sum of the organization's              outstanding holds. Topping up or adjusting one is planned (#60)."
        </p>
        <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
            {move || {
                organizations.get().map(|result| match result {
                    Err(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
                    Ok(organizations) if organizations.is_empty() => view! {
                        <p class="muted">"No organizations yet."</p>
                    }
                    .into_any(),
                    Ok(organizations) => view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>"Name"</th>
                                    <th>"Kind"</th>
                                    <th class="num">"Members"</th>
                                    <th class="num">"Available"</th>
                                    <th class="num">"Frozen"</th>
                                    <th>"Created"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {organizations
                                    .into_iter()
                                    .map(|organization| {
                                        view! {
                                            <tr>
                                                <td>{organization.name}</td>
                                                <td>{organization.kind}</td>
                                                <td class="num">{organization.members}</td>
                                                <td class="mono num">
                                                    {crate::app::credits(organization.available_minor)}
                                                </td>
                                                <td class="mono num pending">
                                                    {crate::app::credits(organization.reserved_minor)}
                                                </td>
                                                <td class="mono">{organization.created_at}</td>
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
async fn load_organizations(token: AdminToken) -> Result<Vec<OrganizationView>, String> {
    admin_call(
        token,
        browser::get(
            &token.0.get_untracked().unwrap_or_default(),
            "/api/v1/admin/organizations",
        ),
    )
    .await
}

#[cfg(not(feature = "hydrate"))]
async fn load_organizations(_token: AdminToken) -> Result<Vec<OrganizationView>, String> {
    Err(String::new())
}

/// Browser-only calls to the admin endpoints, carrying the operator token.
#[cfg(feature = "hydrate")]
mod browser {
    use gloo_net::http::Request;
    use serde::Serialize;
    use serde::de::DeserializeOwned;

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
            .json()
            .await
            .map_err(|_| FetchError::Failed("the answer could not be read".to_owned()))
    }

    /// POSTs a JSON body and answers what it answered, unparsed: a price append carries
    /// the new `version`, a channel save the channel itself.
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
            .json()
            .await
            .map_err(|_| FetchError::Failed("the answer could not be read".to_owned()))
    }
}
