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
    AdminAnomaliesPage, AdminChannelsPage, AdminClosingPage, AdminInFlightPage, AdminLayout,
    AdminOrganizationsPage, AdminReconciliationPage,
};
use crate::api::{
    CreatedKeyView, DashboardData, EntryView, HoldView, InvitationView, KeyView, MemberView,
    MembersView, OrgView, TransferView, add_member, change_member_role, create_invitation,
    create_key, create_team_org, get_bills, get_dashboard, get_entry_bundle, get_keys, get_log,
    get_members, get_requests, get_usage, list_organizations, remove_member, revoke_key,
    switch_organization, transfer_ownership,
};
use crate::bills::BillView;
use crate::chat::ChatPage;
use crate::requests::{REQUESTS_PATH, RequestFilters, RequestView};
use crate::usage::UsageDayView;

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
                // Public: the invitee follows a link an owner or admin handed them.
                <Route path=path!("/register") view=RegisterPage/>
                <Route path=path!("/logout") view=LogoutPage/>
                // Public: the mailed links land here, so they take no session.
                <Route path=path!("/verify-email") view=VerifyEmailPage/>
                <Route path=path!("/forgot-password") view=ForgotPasswordPage/>
                <Route path=path!("/reset-password") view=ResetPasswordPage/>
                // Public: anyone holding a bill and its content hash can verify it, no
                // session needed. Verification runs in the browser, not on the server.
                <Route path=path!("/verify") view=VerifyPage/>
                // The device grant's approval page: a tool's `userCode` lands here —
                // session-gated inside the page, which sends visitors to /login first.
                <Route path=path!("/device") view=DevicePage/>
                // The deployment's surface: the operator token gates it in the browser,
                // and the admin endpoints refuse what the gate missed (issue #57).
                <ParentRoute path=path!("/admin") view=AdminLayout>
                    <Route path=path!("/") view=|| view! { <Redirect path="/admin/channels"/> }/>
                    <Route path=path!("/channels") view=AdminChannelsPage/>
                    <Route path=path!("/organizations") view=AdminOrganizationsPage/>
                    <Route path=path!("/in-flight") view=AdminInFlightPage/>
                    <Route path=path!("/anomalies") view=AdminAnomaliesPage/>
                    <Route path=path!("/reconciliation") view=AdminReconciliationPage/>
                    <Route path=path!("/closing") view=AdminClosingPage/>
                </ParentRoute>
                <ParentRoute path=path!("/dashboard") view=DashboardLayout>
                    <Route path=path!("/") view=OverviewPage/>
                    <Route path=path!("/chat") view=ChatPage/>
                    <Route path=path!("/bills") view=BillsPage/>
                    <Route path=path!("/keys") view=KeysPage/>
                    <Route path=path!("/members") view=MembersPage/>
                    <Route path=path!("/log") view=LogPage/>
                    <Route path=path!("/requests") view=RequestsPage/>
                    <Route path=path!("/usage") view=UsagePage/>
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

/// A pending device request as the approval page shows it (issue #156). Lives
/// outside `mod browser` because the page's signal type compiles for SSR too.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRequestInfo {
    pub user_code: String,
    pub expires_at: String,
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

    /// What `GET /api/v1/auth/methods` answers — the auth surface's only
    /// unauthenticated read (issues #152, #154).
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AuthMethods {
        pub oauth_github: bool,
        pub turnstile_site_key: Option<String>,
    }

    /// The methods on offer, or `None` when the read itself failed — callers
    /// treat it as "not configured", because the surface it gates is a
    /// courtesy, not the flow.
    pub async fn auth_methods() -> Option<AuthMethods> {
        #[derive(serde::Deserialize)]
        struct Envelope {
            data: AuthMethods,
        }
        match Request::get("/api/v1/auth/methods").send().await {
            Ok(response) if response.ok() => response.json::<Envelope>().await.ok().map(|e| e.data),
            _ => None,
        }
    }

    /// The session's current organization name — what a device grant's key
    /// would land in (issue #156). `None` without a live session.
    pub async fn session_organization() -> Option<String> {
        #[derive(serde::Deserialize)]
        struct Envelope {
            data: SessionData,
        }
        #[derive(serde::Deserialize)]
        struct SessionData {
            organization: OrgData,
        }
        #[derive(serde::Deserialize)]
        struct OrgData {
            name: String,
        }
        match Request::get("/api/v1/session").send().await {
            Ok(response) if response.ok() => response
                .json::<Envelope>()
                .await
                .ok()
                .map(|e| e.data.organization.name),
            _ => None,
        }
    }

    use crate::app::DeviceRequestInfo;

    /// The pending request a user code names; the server's own message on 404.
    pub async fn device_request(code: &str) -> Result<DeviceRequestInfo, String> {
        let response = Request::get(&format!("/api/v1/device/request?code={code}"))
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if response.ok() {
            #[derive(serde::Deserialize)]
            struct Envelope {
                data: DeviceRequestInfo,
            }
            return response
                .json::<Envelope>()
                .await
                .map(|e| e.data)
                .map_err(|_| "the answer could not be read".to_owned());
        }
        match response.json::<ErrorBody>().await {
            Ok(body) => Err(body.error.message),
            Err(_) => Err("the request failed".to_owned()),
        }
    }

    /// The verdict on a device request: approve mints the key on the tool's
    /// next poll; deny is terminal.
    pub async fn device_authorize(code: &str, approve: bool) -> Result<DeviceRequestInfo, String> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct AuthorizeBody<'a> {
            user_code: &'a str,
            approve: bool,
        }
        let response = Request::post("/api/v1/device/authorize")
            .json(&AuthorizeBody {
                user_code: code,
                approve,
            })
            .map_err(|_| "could not build the request".to_owned())?
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if response.ok() {
            #[derive(serde::Deserialize)]
            struct Envelope {
                data: DeviceRequestInfo,
            }
            return response
                .json::<Envelope>()
                .await
                .map(|e| e.data)
                .map_err(|_| "the answer could not be read".to_owned());
        }
        match response.json::<ErrorBody>().await {
            Ok(body) => Err(body.error.message),
            Err(_) => Err("the request failed".to_owned()),
        }
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RedeemBody<'a> {
        token: &'a str,
        email: &'a str,
        password: &'a str,
        turnstile_token: Option<&'a str>,
    }

    /// The parts of a redeemed registration the page shows: where the account landed,
    /// and its first API key — shown once, like every secret the API mints.
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Redeemed {
        pub organization: RedeemedOrganization,
        pub api_key: RedeemedKey,
        #[serde(default)]
        pub verification_sent: bool,
    }

    #[derive(serde::Deserialize)]
    pub struct RedeemedOrganization {
        pub name: String,
    }

    #[derive(serde::Deserialize)]
    pub struct RedeemedKey {
        pub secret: String,
    }

    /// The API's error envelope, read for its message so the form can repeat the
    /// server's own words ("this invitation has already been used", and so on).
    #[derive(serde::Deserialize)]
    struct ErrorBody {
        error: ErrorDetail,
    }

    #[derive(serde::Deserialize)]
    struct ErrorDetail {
        message: String,
    }

    /// Registers through an invitation link: the token is the credential.
    pub async fn redeem(
        token: &str,
        email: &str,
        password: &str,
        turnstile_token: Option<&str>,
    ) -> Result<Redeemed, String> {
        let response = Request::post("/api/v1/invitations/redeem")
            .json(&RedeemBody {
                token,
                email,
                password,
                turnstile_token,
            })
            .map_err(|_| "could not build the request".to_owned())?
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if response.ok() {
            return response
                .json::<Redeemed>()
                .await
                .map_err(|_| "the server answered in an unexpected shape".to_owned());
        }
        match response.json::<ErrorBody>().await {
            Ok(body) => Err(body.error.message),
            Err(_) => Err("registration failed; try again.".to_owned()),
        }
    }

    /// The email flows, called with the mailed token or an address — all public
    /// endpoints, so they run as browser fetches like the session calls above.

    #[derive(Serialize)]
    struct TokenBody<'a> {
        token: &'a str,
    }

    /// Consumes a verification token. `Ok` means the address is verified; the
    /// error is the endpoint's own message (unknown, spent and expired all read
    /// the same).
    pub async fn verify_email(token: &str) -> Result<(), String> {
        let response = Request::post("/api/v1/auth/verify")
            .json(&TokenBody { token })
            .map_err(|_| "could not build the request".to_owned())?
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if response.ok() {
            return Ok(());
        }
        match response.json::<ErrorBody>().await {
            Ok(body) => Err(body.error.message),
            Err(_) => Err("the link could not be verified; try again.".to_owned()),
        }
    }

    #[derive(Serialize)]
    struct ForgotBody<'a> {
        email: &'a str,
    }

    /// Asks for a reset mail. Always `Ok` when the call completes — the endpoint
    /// is deliberately indistinguishable about whether the address has an
    /// account, and the page's message says so.
    pub async fn forgot_password(email: &str) -> Result<(), String> {
        let response = Request::post("/api/v1/auth/password/forgot")
            .json(&ForgotBody { email })
            .map_err(|_| "could not build the request".to_owned())?
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if response.ok() {
            return Ok(());
        }
        match response.json::<ErrorBody>().await {
            Ok(body) => Err(body.error.message),
            Err(_) => Err("the request failed; try again.".to_owned()),
        }
    }

    #[derive(Serialize)]
    struct ResetBody<'a> {
        token: &'a str,
        password: &'a str,
    }

    /// Consumes a reset token and sets the new password.
    pub async fn reset_password(token: &str, password: &str) -> Result<(), String> {
        let response = Request::post("/api/v1/auth/password/reset")
            .json(&ResetBody { token, password })
            .map_err(|_| "could not build the request".to_owned())?
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if response.ok() {
            return Ok(());
        }
        match response.json::<ErrorBody>().await {
            Ok(body) => Err(body.error.message),
            Err(_) => Err("the reset failed; try again.".to_owned()),
        }
    }

    /// The resend banner's call: mails the session user a fresh verification
    /// link. The answer distinguishes the three outcomes the contract names.
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct VerifyRequestData {
        sent: bool,
        already_verified: bool,
    }

    /// The API envelope: `{"data": …}`.
    #[derive(serde::Deserialize)]
    struct DataWrap {
        data: VerifyRequestData,
    }

    /// `Some(message)` is what the banner says; `Err` only when the call itself
    /// failed, since the endpoint's own answers are all 200 here.
    pub async fn request_verification() -> Result<Option<String>, String> {
        let response = Request::post("/api/v1/auth/verify/request")
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if response.status() == 503 {
            return Err("email is not configured on this deployment".to_owned());
        }
        if !response.ok() {
            return match response.json::<ErrorBody>().await {
                Ok(body) => Err(body.error.message),
                Err(_) => Err("the request failed; try again.".to_owned()),
            };
        }
        let data = response
            .json::<DataWrap>()
            .await
            .map_err(|_| "the server answered in an unexpected shape".to_owned())?;
        Ok(Some(if data.data.already_verified {
            "This address is already verified.".to_owned()
        } else if data.data.sent {
            "Sent — check your inbox.".to_owned()
        } else {
            "A verification mail went out moments ago — check your inbox.".to_owned()
        }))
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
    let query = use_query_map();
    // Whether the deployment offers GitHub OAuth: the unauthenticated
    // `auth/methods` read, so the button only exists where the flow does.
    let (github, set_github) = signal(false);

    // A bounced OAuth callback lands back here with `?error=oauth`; the detail
    // stays in the server's logs, the page just says the flow failed.
    Effect::new(move |_| {
        if query.read().get("error").as_deref() == Some("oauth") {
            set_error.set(Some(
                "GitHub sign-in did not complete — try again, or use your password.".to_owned(),
            ));
        }
    });

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

    Effect::new(move |_| {
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_github.set(
                browser::auth_methods()
                    .await
                    .map(|methods| methods.oauth_github)
                    .unwrap_or(false),
            );
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = &set_github;
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
                {move || github.get().then(|| view! {
                    <p class="divider muted">"or"</p>
                    <a class="oauth" href="/api/v1/auth/oauth/github">
                        "Continue with GitHub"
                    </a>
                })}
                <p class="muted">
                    <A href="/forgot-password">"Forgot your password?"</A>
                </p>
            </form>
        </main>
    }
}

/// The verification mail's landing page: reads `?token=` and consumes it once,
/// on mount — a refresh answers the token's spent state, which is honest.
#[component]
fn VerifyEmailPage() -> impl IntoView {
    let query = use_query_map();
    let token = move || query.read().get("token").unwrap_or_default();
    let (message, set_message) = signal("Verifying…".to_owned());

    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let token = token();
        leptos::task::spawn_local(async move {
            if token.is_empty() {
                set_message.set(
                    "This link carries no token — open the whole link from the mail.".to_owned(),
                );
                return;
            }
            match browser::verify_email(&token).await {
                Ok(()) => set_message.set("Email verified.".to_owned()),
                Err(_) => set_message.set(
                    "This link is not usable — it was already used, it expired, or it is not a \
                     verification link."
                        .to_owned(),
                ),
            }
        });
    });
    #[cfg(not(feature = "hydrate"))]
    let _ = (&set_message, &token);

    view! {
        <main class="center">
            <section class="card" aria-label="Verify email">
                <h1>"Verify email"</h1>
                <p>{move || message.get()}</p>
                <p class="muted">
                    <A href="/login">"Log in"</A>
                </p>
            </section>
        </main>
    }
}

/// The forgot-password page: an address in, the indistinguishable answer out —
/// the mail either went out or it did not, and the page cannot and does not say
/// which.
#[component]
fn ForgotPasswordPage() -> impl IntoView {
    let (email, set_email) = signal(String::new());
    let (done, set_done) = signal(false);
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_busy.set(true);
            set_error.set(None);
            match browser::forgot_password(&email.get_untracked()).await {
                Ok(()) => set_done.set(true),
                Err(message) => {
                    set_error.set(Some(message));
                    set_busy.set(false);
                }
            }
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = (&set_done, &set_busy, &set_error, &email);
    };

    view! {
        <main class="center">
            <form class="card" method="post" on:submit=submit aria-label="Forgot password">
                <h1>"Reset password"</h1>
                {move || {
                    if done.get() {
                        view! {
                            <p>
                                "If that address has an account, a reset link is on its way — it works once, for one hour."
                            </p>
                            <p class="muted">
                                <A href="/login">"Back to log in"</A>
                            </p>
                        }
                            .into_any()
                    } else {
                        view! {
                            <p class="muted">"The account's email address."</p>
                            {move || {
                                error.get().map(|message| {
                                    view! { <p class="error" role="alert">{message}</p> }
                                })
                            }}
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
                            <button type="submit" prop:disabled=move || busy.get()>
                                {move || if busy.get() { "Sending…" } else { "Send reset link" }}
                            </button>
                        }
                            .into_any()
                    }
                }}
            </form>
        </main>
    }
}

/// The reset mail's landing page: `?token=` plus the new password. A spent or
/// expired token fails on submit with the endpoint's own message.
#[component]
fn ResetPasswordPage() -> impl IntoView {
    let query = use_query_map();
    let token = move || query.read().get("token").unwrap_or_default();
    let (password, set_password) = signal(String::new());
    let (done, set_done) = signal(false);
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            set_busy.set(true);
            set_error.set(None);
            match browser::reset_password(&token(), &password.get_untracked()).await {
                Ok(()) => set_done.set(true),
                Err(message) => {
                    set_error.set(Some(message));
                    set_busy.set(false);
                }
            }
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = (&set_done, &set_busy, &set_error, &password, &token);
    };

    view! {
        <main class="center">
            <form class="card" method="post" on:submit=submit aria-label="Reset password">
                <h1>"Choose a new password"</h1>
                {move || {
                    if done.get() {
                        view! {
                            <p>"The password is changed; every existing session was logged out."</p>
                            <p class="muted">
                                <A href="/login">"Log in"</A>
                            </p>
                        }
                            .into_any()
                    } else {
                        view! {
                            {move || {
                                error.get().map(|message| {
                                    view! { <p class="error" role="alert">{message}</p> }
                                })
                            }}
                            <label>
                                "New password"
                                <input
                                    type="password"
                                    name="password"
                                    autocomplete="new-password"
                                    required
                                    minlength="12"
                                    prop:value=move || password.get()
                                    on:input=move |ev| set_password.set(event_target_value(&ev))
                                />
                            </label>
                            <button type="submit" prop:disabled=move || busy.get()>
                                {move || if busy.get() { "Resetting…" } else { "Reset password" }}
                            </button>
                        }
                            .into_any()
                    }
                }}
            </form>
        </main>
    }
}

/// Loads the Turnstile script once — tagged with a marker attribute, so a
/// revisit does not stack another copy (issue #154).
#[cfg(feature = "hydrate")]
fn inject_turnstile() {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    if document
        .query_selector("script[data-turnstile]")
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    if let (Ok(script), Some(head)) = (document.create_element("script"), document.head()) {
        let _ = script.set_attribute(
            "src",
            "https://challenges.cloudflare.com/turnstile/v0/api.js",
        );
        let _ = script.set_attribute("async", "");
        let _ = script.set_attribute("defer", "");
        let _ = script.set_attribute("data-turnstile", "");
        let _ = head.append_child(&script);
    }
}

/// The widget's current answer: the hidden input it maintains inside its div.
/// `None` when the check is not configured or the widget has not answered yet.
#[cfg(feature = "hydrate")]
fn turnstile_response() -> Option<String> {
    use wasm_bindgen::JsCast;
    let input = web_sys::window()?
        .document()?
        .query_selector("input[name=\"cf-turnstile-response\"]")
        .ok()??
        .dyn_into::<web_sys::HtmlInputElement>()
        .ok()?;
    // The widget keeps its answer on the element's `value` property, not the
    // attribute — `get_attribute` would always read the empty initial markup.
    let value = input.value();
    (!value.is_empty()).then_some(value)
}

/// Registers through an invitation link (`/register?invite=<token>`), then offers the
/// way to the login page. The link carries the credential, so this page works without
/// a session — in an `invite`-mode deployment it is the only way in.
#[component]
fn RegisterPage() -> impl IntoView {
    let query = use_query_map();
    let token = move || query.read().get("invite").unwrap_or_default();
    let (email, set_email) = signal(String::new());
    let (password, set_password) = signal(String::new());
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);
    // The Turnstile site key when the deployment runs the anti-bot check —
    // `auth/methods` answers it, and the widget renders only where the check
    // exists (issue #154).
    let (turnstile, set_turnstile) = signal(Option::<String>::None);
    // The account once it exists: the organization it joined and its first API key.
    // The secret is shown here and nowhere else, like every key the API mints.
    #[cfg(feature = "hydrate")]
    let (joined, set_joined) = signal(Option::<(String, String, bool)>::None);

    Effect::new(move |_| {
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            if let Some(key) = browser::auth_methods()
                .await
                .and_then(|methods| methods.turnstile_site_key)
            {
                set_turnstile.set(Some(key));
                // The widget's script goes in after the div lands: api.js
                // implicit-renders `.cf-turnstile` elements present when it
                // executes, and the signal set above is synchronous.
                inject_turnstile();
            }
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = &set_turnstile;
    });

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        {
            let token = token();
            leptos::task::spawn_local(async move {
                set_busy.set(true);
                set_error.set(None);
                match browser::redeem(
                    &token,
                    &email.get_untracked(),
                    &password.get_untracked(),
                    turnstile_response().as_deref(),
                )
                .await
                {
                    Ok(redeemed) => set_joined.set(Some((
                        redeemed.organization.name,
                        redeemed.api_key.secret,
                        redeemed.verification_sent,
                    ))),
                    Err(message) => {
                        set_error.set(Some(message));
                        set_busy.set(false);
                    }
                }
            });
        }
        // SSR emits the inert form; the submit handler runs in the browser. `method`
        // stays absent so a native submit can only ever `GET` this page — no password
        // in the URL, the same reason as the login form.
        #[cfg(not(feature = "hydrate"))]
        let _ = (&set_busy, &set_error);
    };

    view! {
        <main class="center">
            <form class="card" method="post" on:submit=submit aria-label="Register">
                <h1>"oxsum"</h1>
                {move || {
                    #[cfg(feature = "hydrate")]
                    if let Some((organization, secret, verification_sent)) = joined.get() {
                        return view! {
                            <p class="success" role="status">
                                "Your account is part of " {organization} "."
                            </p>
                            {verification_sent.then(|| view! {
                                <p class="muted">
                                    "A verification link is on its way to your inbox — open it within seven days."
                                </p>
                            })}
                            <p class="muted">
                                "Your first API key — keep it, it is shown only once:"
                            </p>
                            <p><code>{secret}</code></p>
                            <p><A href="/login">"Log in"</A></p>
                        }
                        .into_any();
                    }
                    if token().is_empty() {
                        return view! {
                            <p class="muted">"Register through an invitation link."</p>
                            <p class="error" role="alert">
                                "This link is missing its invitation token. Ask the person who invited you to send the whole link again."
                            </p>
                        }
                        .into_any();
                    }
                    view! {
                        <p class="muted">"You were invited to join an organization."</p>
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
                            "Password (at least 12 characters)"
                            <input
                                type="password"
                                name="password"
                                autocomplete="new-password"
                                required
                                prop:value=move || password.get()
                                on:input=move |ev| set_password.set(event_target_value(&ev))
                            />
                        </label>
                        {move || turnstile.get().map(|key| view! {
                            // The widget implicit-renders into this div and
                            // writes its answer to a hidden input it owns.
                            <div class="cf-turnstile" data-sitekey=key></div>
                        })}
                        <button type="submit" prop:disabled=move || busy.get()>
                            {move || if busy.get() { "Registering…" } else { "Register" }}
                        </button>
                    }
                    .into_any()
                }}
            </form>
        </main>
    }
}

/// The device grant's approval page (`/device?code=…`): a CLI or other tool
/// printed a user code and is polling; the signed-in user confirms the key it
/// will receive (issue #156). Without a session the page defers to `/login` —
/// approving is a person's action.
#[component]
fn DevicePage() -> impl IntoView {
    let query = use_query_map();
    let navigate = use_navigate();
    let (code, set_code) = signal(query.read().get("code").unwrap_or_default());
    let (organization, set_organization) = signal(Option::<String>::None);
    // None while looking the code up, Some((request, decided)) once it answers.
    let (request, set_request) = signal(Option::<(DeviceRequestInfo, bool)>::None);
    let (error, set_error) = signal(Option::<String>::None);
    let (busy, set_busy) = signal(false);

    Effect::new(move |_| {
        #[cfg(feature = "hydrate")]
        {
            let navigate = navigate.clone();
            leptos::task::spawn_local(async move {
                if !browser::session_ok().await {
                    navigate("/login", Default::default());
                    return;
                }
                set_organization.set(browser::session_organization().await);
                let typed = query.read().get("code").unwrap_or_default();
                if !typed.is_empty() {
                    match browser::device_request(&typed).await {
                        Ok(info) => set_request.set(Some((info, false))),
                        Err(message) => set_error.set(Some(message)),
                    }
                }
            });
        }
        #[cfg(not(feature = "hydrate"))]
        let _ = (&navigate, &set_organization, &set_request, &set_error);
    });

    let lookup = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        {
            let code = code.get_untracked();
            leptos::task::spawn_local(async move {
                set_busy.set(true);
                set_error.set(None);
                set_request.set(None);
                match browser::device_request(&code).await {
                    Ok(info) => set_request.set(Some((info, false))),
                    Err(message) => set_error.set(Some(message)),
                }
                set_busy.set(false);
            });
        }
        #[cfg(not(feature = "hydrate"))]
        let _ = (&set_busy, &set_error, &set_request);
    };

    let decide = move |approve: bool| {
        #[cfg(feature = "hydrate")]
        {
            let Some((info, _)) = request.get_untracked() else {
                return;
            };
            let user_code = info.user_code.clone();
            leptos::task::spawn_local(async move {
                set_busy.set(true);
                set_error.set(None);
                match browser::device_authorize(&user_code, approve).await {
                    Ok(info) => set_request.set(Some((info, true))),
                    Err(message) => set_error.set(Some(message)),
                }
                set_busy.set(false);
            });
        }
        #[cfg(not(feature = "hydrate"))]
        let _ = approve;
    };

    view! {
        <main class="center">
            <div class="card">
                <h1>"Authorize a device"</h1>
                {move || {
                    if let Some((info, decided)) = request.get() {
                        if decided {
                            return view! {
                                <p class="success" role="status">
                                    "Done — the tool receives its API key when it next polls, or nothing if you denied it. The key is listed under"
                                    {organization.get().unwrap_or_else(|| "the organization".to_owned())}
                                    " on the keys page, where you can revoke it."
                                </p>
                            }
                            .into_any();
                        }
                        return view! {
                            <p class="muted">
                                "A tool asked for an API key under code "
                                <code>{info.user_code.clone()}</code>
                                ". Approving grants it a key for "
                                {organization.get().unwrap_or_else(|| "your current organization".to_owned())}
                                " — the key lands when the tool polls, and is revocable like any other."
                            </p>
                            <p class="muted">"The request lapses at " {info.expires_at.clone()} "."</p>
                            {move || error.get().map(|message| view! { <p class="error" role="alert">{message}</p> })}
                            <div class="row">
                                <button
                                    prop:disabled=move || busy.get()
                                    on:click=move |_| decide(true)
                                >
                                    "Approve"
                                </button>
                                <button
                                    class="link"
                                    prop:disabled=move || busy.get()
                                    on:click=move |_| decide(false)
                                >
                                    "Deny"
                                </button>
                            </div>
                        }
                        .into_any();
                    }
                    view! {
                        <form method="post" on:submit=lookup>
                            <p class="muted">
                                "Enter the code the tool printed, like "
                                <code>"ABCD-EFGH"</code>
                                "."
                            </p>
                            {move || error.get().map(|message| view! { <p class="error" role="alert">{message}</p> })}
                            <label>
                                "Code"
                                <input
                                    name="code"
                                    autocomplete="off"
                                    required
                                    prop:value=move || code.get()
                                    on:input=move |ev| set_code.set(event_target_value(&ev))
                                />
                            </label>
                            <button type="submit" prop:disabled=move || busy.get()>
                                {move || if busy.get() { "Checking…" } else { "Continue" }}
                            </button>
                        </form>
                    }
                    .into_any()
                }}
            </div>
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
                <A href="/dashboard/usage">"Usage"</A>
                // Top-level and public, like /logout: verification needs no session.
                <A href="/verify">"Verify a bill"</A>
                <A href="/logout">"Log out"</A>
            </nav>
            <main class="content">
                <OrgSwitcher/>
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

/// Reloads the page: after the acting organization switches or a new one is created,
/// every page's data is read fresh against the session's new organization — a reload is
/// how the whole dashboard follows at once.
fn reload() {
    #[cfg(feature = "hydrate")]
    if let Some(window) = web_sys::window() {
        let _ = window.location().reload();
    }
}

/// The acting organization, top right as product.md places it: the select switches
/// between the organizations the user belongs to, and "New" makes a team organization
/// with the user as its owner.
///
/// A switch updates the session row, so it survives a reload — and the reload is what
/// applies it here, re-reading every page against the new organization. An unreadable
/// list renders nothing rather than an error row: the dashboard's own guard already
/// reports a dead session.
#[component]
fn OrgSwitcher() -> impl IntoView {
    let organizations = Resource::new(|| (), |_| async { list_organizations().await });
    let (creating, set_creating) = signal(false);
    let (name, set_name) = signal(String::new());
    let (busy, set_busy) = signal(false);
    let (error, set_error) = signal(Option::<String>::None);

    let switch = move |ev: web_sys::Event| {
        let organization_id = event_target_value(&ev);
        leptos::task::spawn_local(async move {
            if switch_organization(organization_id).await.is_ok() {
                reload();
            }
        });
    };

    let create = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        set_busy.set(true);
        set_error.set(None);
        let name = name.get();
        leptos::task::spawn_local(async move {
            match create_team_org(name).await {
                Ok(()) => reload(),
                Err(err) => {
                    set_busy.set(false);
                    set_error.set(Some(err.to_string()));
                }
            }
        });
    };

    view! {
        <div class="topbar">
            <Suspense>
                {move || {
                    organizations.get().and_then(Result::ok).map(|(orgs, active)| {
                        view! {
                            <label class="org-switch">
                                "Organization"
                                <select
                                    prop:value=active.clone()
                                    on:change=switch
                                >
                                    {orgs
                                        .iter()
                                        .map(|org: &OrgView| {
                                            view! {
                                                <option value=org.id.clone() selected=org.id == active>
                                                    {format!("{} ({})", org.name, org.kind)}
                                                </option>
                                            }
                                        })
                                        .collect_view()}
                                </select>
                            </label>
                            <button
                                type="button"
                                class="quiet"
                                on:click=move |_| set_creating.update(|c| *c = !*c)
                            >
                                {move || if creating.get() { "Cancel" } else { "New" }}
                            </button>
                        }
                    })
                }}
            </Suspense>
        </div>
        {move || {
            creating.get().then(|| {
                view! {
                    <form class="card org-create" method="post" on:submit=create aria-label="New organization">
                        <h2>"New team organization"</h2>
                        {move || {
                            error
                                .get()
                                .map(|message| view! { <p class="error" role="alert">{message}</p> })
                        }}
                        <label>
                            "Name"
                            <input
                                type="text"
                                name="name"
                                maxlength="80"
                                required
                                prop:value=move || name.get()
                                on:input=move |ev| set_name.set(event_target_value(&ev))
                            />
                        </label>
                        <button type="submit" prop:disabled=move || busy.get()>
                            {move || if busy.get() { "Creating…" } else { "Create" }}
                        </button>
                    </form>
                }
            })
        }}
    }
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

/// The unverified-address reminder: one muted line with a resend, shown only
/// when a mail can actually go out (`email_flows` — a deployment without a
/// mailer never nags about a mail it cannot send). The button's result replaces
/// the line, including the cooldown answer.
#[component]
fn VerifyBanner(show: bool) -> impl IntoView {
    let (message, set_message) = signal(Option::<String>::None);
    let resend = move |_| {
        #[cfg(feature = "hydrate")]
        leptos::task::spawn_local(async move {
            match browser::request_verification().await {
                Ok(message) => set_message.set(message),
                Err(error) => set_message.set(Some(error)),
            }
        });
        #[cfg(not(feature = "hydrate"))]
        let _ = &set_message;
    };
    view! {
        {move || {
            if let Some(text) = message.get() {
                view! { <p class="muted">{text}</p> }.into_any()
            } else if show {
                view! {
                    <p class="muted">
                        "This email is not verified. "
                        <button type="button" class="link" on:click=resend>
                            "Resend the verification mail"
                        </button>
                    </p>
                }
                    .into_any()
            } else {
                ().into_any()
            }
        }}
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
            {data
                .credit_limit_minor
                .gt(&0)
                .then(|| {
                    view! {
                        <p class="balance">
                            <span class="muted">"Credit line (used of limit)"</span>
                            <strong class="mono">
                                {format!(
                                    "{} / {}",
                                    credits(data.credit_used_minor),
                                    credits(data.credit_limit_minor),
                                )}
                            </strong>
                        </p>
                    }
                })}
            <VerifyBanner show=!data.email_verified && data.email_flows/>
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

/// The `before` bound a list page's URL carries: the log index the page resumes
/// below (issue #93). A missing or unparsable value reads as the first page — a
/// hand-edited query degrades to the newest slice rather than an error.
fn page_position(query: &leptos_router::params::ParamsMap) -> Option<u64> {
    query
        .get("before")
        .and_then(|value| value.parse::<u64>().ok())
}

/// A ledger list's pager (issue #93): "older" resumes the walk where this page's
/// `nextCursor` pointed, "newest" back at the first page. The position is the URL's
/// own `before` parameter — a plain link, the way the filter form's `GET` is — so a
/// browser that never hydrates turns pages the same way one that does, and a middle
/// page is a link that survives a reload.
#[component]
fn Pager(older_href: Option<String>, newest_href: Option<String>) -> impl IntoView {
    (older_href.is_some() || newest_href.is_some()).then(|| {
        view! {
            <p class="pager">
                {newest_href.map(|href| {
                    view! { <a href=href>"Newest"</a>" · " }
                })}
                {older_href.map(|href| view! { <a href=href>"Older"</a> })}
            </p>
        }
    })
}

/// The bills page: every transaction, newest first — top-ups, adjustments and settled
/// requests — each with what it moved, in credits, and the content hash its proof
/// verifies against. Every row links to `/verify` with its entry named, so a bill
/// verifies straight from the page (issue #90).
///
/// A member's table is scoped the way the requests page scopes: only what their own
/// keys paid, plus the organization's shared history — top-ups and adjustments carry
/// no key, so every member sees them.
///
/// The exports are links rather than buttons: `GET /dashboard/bills/export.csv` answers
/// with `Content-Disposition: attachment`, so the browser saves the file without any
/// script of ours, and a command-line client can fetch it the same way
/// (docs/decisions.md). The amount is the ledger's signed integer in both files and
/// travels through the row type unformatted; the table renders that same integer with
/// the dashboard's [`credits()`], so the page and the files cannot disagree about an
/// amount.
#[component]
fn BillsPage() -> impl IntoView {
    let query = use_query_map();
    // Read once, from the URL: SSR and the browser agree on the position, and the
    // pager's plain links re-render this component with the new query string.
    let before = StoredValue::new(page_position(&query.get_untracked()));
    let bills = Resource::new(
        || (),
        move |_| async move { get_bills(before.get_value()).await },
    );
    view! {
        <section class="card" aria-label="Bills">
            <h1>"Bills"</h1>
            <p class="muted">
                "This organization's transactions, newest first: when each was booked, what it moved in credits, and the content hash its proof verifies against. The two downloads carry the same rows, with the amount as the ledger's signed integer in minor units."
            </p>
            <p>
                <a href="/dashboard/bills/export.csv">"Download CSV"</a>
                " · "
                <a href="/dashboard/bills/export.json">"Download JSON"</a>
            </p>
            <HeadArchive/>
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || bills.get().map(|result| match result {
                    Ok(page) => view! {
                        <BillTable bills=page.rows/>
                        <Pager
                            older_href=page.next_cursor.map(|cursor| {
                                format!("/dashboard/bills?before={cursor}")
                            })
                            newest_href=before
                                .get_value()
                                .map(|_| "/dashboard/bills".to_owned())
                        />
                    }
                        .into_any(),
                    Err(error) => view! { <p class="error" role="alert">{error.to_string()}</p> }.into_any(),
                })}
            </Suspense>
        </section>
    }
}

/// The kind, as the column reads it.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "topUp" => "Top-up",
        "adjustment" => "Adjustment",
        _ => "Settlement",
    }
}

/// What the row moved, in the entry's own terms: a settled gateway turn reads as its
/// model, token counts, freeze and settlement kind — the fields the bill's content
/// hash covers — while a top-up or an adjustment shows the reason it was written.
/// Entries that record no words of their own — a top-up, a settlement written through
/// the wallet API — read as a dash rather than a blank cell.
fn detail(bill: &BillView) -> String {
    match &bill.request {
        Some(request) => format!(
            "{} · {} · {}→{} tok · froze {} · {}",
            request.request_id,
            request.model,
            request.input_tokens,
            request.output_tokens,
            credits(request.freeze_minor),
            request.kind,
        ),
        None if bill.description.is_empty() => "—".to_owned(),
        None => bill.description.clone(),
    }
}

/// The verify link for one row: the entry id and content hash travel as query
/// parameters, and `/verify` fetches the proof bundle for the named entry and runs
/// the check on load.
fn verify_href(bill: &BillView) -> String {
    format!(
        "/verify?entry={}&contentHash={}",
        bill.entry_id, bill.content_hash
    )
}

/// The amount with its sign, so a top-up reads apart from a charge at a glance.
fn signed_credits(minor: i64) -> String {
    if minor >= 0 {
        format!("+{}", credits(minor))
    } else {
        credits(minor)
    }
}

/// The transactions: booked on, kind, what it was, the signed amount in credits, the
/// content hash and the per-row verify link. The amount is the one column rendered for
/// a reader rather than for a machine, so it goes through [`credits()`] like every
/// other amount in the dashboard.
#[component]
fn BillTable(#[prop(into)] bills: Vec<BillView>) -> impl IntoView {
    view! {
        {if bills.is_empty() {
            view! { <p class="muted">"No transactions yet."</p> }.into_any()
        } else {
            view! {
                <table>
                    <thead>
                        <tr>
                            <th scope="col">"Booked on"</th>
                            <th scope="col">"Kind"</th>
                            <th scope="col">"Detail"</th>
                            <th scope="col" class="num">"Amount (credits)"</th>
                            <th scope="col">"Content hash"</th>
                            <th scope="col"><span class="muted">"Verify"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each=move || bills.clone() key=|bill| bill.entry_id.clone() let(bill)>
                            <tr>
                                <td class="mono">{bill.booked_on.clone()}</td>
                                <td>{kind_label(&bill.kind)}</td>
                                <td>{detail(&bill)}</td>
                                <td class="mono num">{signed_credits(bill.amount_minor)}</td>
                                <td class="mono">{bill.content_hash.clone()}</td>
                                <td><A href=verify_href(&bill)>"verify"</A></td>
                            </tr>
                        </For>
                    </tbody>
                </table>
            }
                .into_any()
        }}
    }
}

/// What the archive check found, rendered as one line above the bills table.
// Most variants are only constructed in the browser, where the check actually runs.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
#[derive(Clone)]
enum ArchiveStatus {
    /// Still running.
    Checking,
    /// This deployment signs no heads: no archive to keep.
    NotConfigured,
    /// The head or proof could not be fetched or parsed.
    Unavailable(String),
    /// First visit: the signed head was verified and stored.
    Recorded(u64),
    /// The archived head is the served head; the signature re-verified.
    Unchanged(u64),
    /// The log grew and the consistency proof plus signature verified.
    Verified {
        /// The archived head's size.
        from: u64,
        /// The served head's size.
        to: u64,
    },
    /// The served log does not extend the archive: history may have been rewritten.
    Failed(String),
}

/// The archive check itself, browser-only: it reads and writes `localStorage`.
///
/// Trust order, matching `oxsum_verify`: the served head's signature is checked before
/// anything is stored; when the log has grown, the consistency proof must anchor the
/// *archived* head — not just any older head — before the new one replaces it. A
/// failure never overwrites the archive, so a rewritten history stays detectable on
/// the next visit too.
#[cfg(feature = "hydrate")]
async fn check_head_archive() -> ArchiveStatus {
    use crate::api::{get_log_consistency, get_log_head};
    use crate::heads::{ArchiveStep, archive, decide};
    let served = match get_log_head().await {
        Ok(Some(head)) => head,
        Ok(None) => return ArchiveStatus::NotConfigured,
        Err(error) => return ArchiveStatus::Unavailable(error.to_string()),
    };
    let Some(head) = served.head() else {
        return ArchiveStatus::Unavailable("the served head did not parse".to_owned());
    };
    if let Err(error) = oxsum_verify::verify_signed_head(
        &served.note,
        &head,
        &served.origin,
        &served.published_key(),
    ) {
        return ArchiveStatus::Failed(error.to_string());
    }
    let held = archive::read(&served.origin);
    match decide(held, &head) {
        ArchiveStep::Record => {
            archive::write(&served.origin, &head);
            ArchiveStatus::Recorded(head.size)
        }
        ArchiveStep::Unchanged => ArchiveStatus::Unchanged(head.size),
        ArchiveStep::Shrank => ArchiveStatus::Failed(format!(
            "the served log is shorter (size {}) than the archived head",
            head.size
        )),
        ArchiveStep::Forked => ArchiveStatus::Failed(
            "the served head at the archived size has a different root".to_owned(),
        ),
        ArchiveStep::Grow { from } => {
            // `held` is `Some` here: `decide` only picks Grow when an archive exists.
            let Some(held) = held else {
                return ArchiveStatus::Unavailable(
                    "the archived head disappeared mid-check".to_owned(),
                );
            };
            let consistency = match get_log_consistency(from).await {
                Ok(Some(c)) => c,
                Ok(None) => return ArchiveStatus::NotConfigured,
                Err(error) => return ArchiveStatus::Unavailable(error.to_string()),
            };
            let (Some(old), Some(new), Some(proof)) = (
                consistency.old_head.head(),
                consistency.signed.head(),
                consistency.consistency_proof(),
            ) else {
                return ArchiveStatus::Unavailable(
                    "the consistency proof did not parse".to_owned(),
                );
            };
            match oxsum_verify::verify_consistency(
                &held,
                &old,
                &proof,
                &consistency.signed.note,
                &new,
                &consistency.signed.origin,
                &consistency.signed.published_key(),
            ) {
                Ok(()) => {
                    archive::write(&consistency.signed.origin, &new);
                    ArchiveStatus::Verified {
                        from: held.size,
                        to: new.size,
                    }
                }
                Err(error) => ArchiveStatus::Failed(error.to_string()),
            }
        }
    }
}

/// The ledger's append-only check: one status line, in words rather than a color —
/// the verdict is never something styling alone carries.
#[component]
#[cfg_attr(not(feature = "hydrate"), allow(unused_variables))]
fn HeadArchive() -> impl IntoView {
    let (status, set_status) = signal(ArchiveStatus::Checking);
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        leptos::task::spawn_local(async move {
            set_status.set(check_head_archive().await);
        });
    });
    view! {
        {move || match status.get() {
            ArchiveStatus::Checking => view! {
                <p class="muted">"Checking the archived ledger head…"</p>
            }.into_any(),
            ArchiveStatus::NotConfigured => view! {
                <p class="muted">
                    "This deployment does not sign ledger heads, so the append-only check is unavailable."
                </p>
            }.into_any(),
            ArchiveStatus::Unavailable(why) => view! {
                <p class="muted">
                    "The archived-head check could not run: "{why}
                </p>
            }.into_any(),
            ArchiveStatus::Recorded(size) => view! {
                <p class="muted">
                    {format!(
                        "Ledger head archived at size {size} — verified against the operator's signature. Later visits prove the log only grows."
                    )}
                </p>
            }.into_any(),
            ArchiveStatus::Unchanged(size) => view! {
                <p class="muted">
                    {format!("Ledger head unchanged since the last visit (size {size}); signature verified.")}
                </p>
            }.into_any(),
            ArchiveStatus::Verified { from, to } => view! {
                <p class="muted">
                    {format!(
                        "Ledger verified append-only in this browser: entries {from} → {to}, signature and consistency proof checked."
                    )}
                </p>
            }.into_any(),
            ArchiveStatus::Failed(why) => view! {
                <p class="error" role="alert">
                    <strong>"Ledger archive check failed: "</strong>
                    {why}
                    " — the history behind these bills may have been rewritten."
                </p>
            }.into_any(),
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
    let invite = Action::new(|_: &()| async move { create_invitation().await });
    // The link just minted: shown once, with its expiry — the server keeps only its
    // hash, so this page is the only place the token exists.
    let (invitation, set_invitation) = signal(Option::<InvitationView>::None);

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
    Effect::new(move |_| match invite.value().get() {
        Some(Ok(created)) => {
            set_invitation.set(Some(created));
            set_notice.set(Some(Notice::done(
                "Invitation link created — pass it on, it works once within seven days.".to_owned(),
            )));
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
                        <Members data=data add=add remove=remove set_role=set_role transfer=transfer invite=invite invitation=invitation email=email/>
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

/// The URL the invitation link points at, on this deployment's own origin.
/// SSR never reaches it — the link only exists after a click in the browser.
fn invite_link(token: &str) -> String {
    #[cfg(feature = "hydrate")]
    {
        let origin = leptos::web_sys::window()
            .and_then(|window| window.location().origin().ok())
            .unwrap_or_default();
        return format!("{origin}/register?invite={token}");
    }
    #[cfg(not(feature = "hydrate"))]
    format!("/register?invite={token}")
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
    invite: Action<(), Result<InvitationView, ServerFnError>>,
    invitation: ReadSignal<Option<InvitationView>>,
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
                "The account must already exist. For someone new, hand them an invitation link instead."
            </p>
            <p class="row">
                <button
                    type="button"
                    class="quiet"
                    prop:disabled=move || invite.pending().get()
                    on:click=move |_| drop(invite.dispatch(()))
                >
                    {move || if invite.pending().get() { "Creating…" } else { "Invite by link" }}
                </button>
            </p>
            {move || invitation.get().map(|created| view! {
                <p class="muted">
                    "The link registers one account and expires on " {created.expires_at} ". It is shown only here:"
                </p>
                <p><code>{invite_link(&created.token)}</code></p>
            })}
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
    let query = use_query_map();
    let before = StoredValue::new(page_position(&query.get_untracked()));
    let log = Resource::new(
        || (),
        move |_| async move { get_log(before.get_value()).await },
    );
    view! {
        <section class="card" aria-label="Transaction log">
            <h1>"Transaction log"</h1>
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || log.get().map(|result| match result {
                    Ok(page) => view! {
                        <EntryTable entries=page.rows/>
                        <Pager
                            older_href=page.next_cursor.map(|cursor| {
                                format!("/dashboard/log?before={cursor}")
                            })
                            newest_href=before.get_value().map(|_| "/dashboard/log".to_owned())
                        />
                    }
                        .into_any(),
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
    let before = StoredValue::new(page_position(&query.get_untracked()));
    let requests = Resource::new(
        || (),
        move |_| async move {
            get_requests(
                filters.get_value().key.clone(),
                filters.get_value().model.clone(),
                before.get_value(),
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
                    Ok(page) => view! {
                        <RequestTable
                            requests=page.rows
                            filters=filters.get_value()
                            filtered=!filters.get_value().is_unfiltered()
                        />
                        <Pager
                            older_href=page.next_cursor.map(|cursor| {
                                filters.get_value().href_before(cursor)
                            })
                            newest_href=before
                                .get_value()
                                .map(|_| filters.get_value().href())
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

/// The usage dashboard: what the organization spent per day over the trailing 30 UTC
/// days, drawn as a bar chart, and per channel and model, summed into a table
/// (issue #126).
///
/// The days are the settlement entries' booking dates — the ledger's day, the same one
/// a statement counts a turn under — and a member's page covers the keys they may see
/// plus the organization's unattributed usage, the bills page's rule.
#[component]
fn UsagePage() -> impl IntoView {
    let usage = Resource::new(|| (), |_| async { get_usage().await });
    view! {
        <section class="card" aria-label="Usage">
            <h1>"Usage"</h1>
            <p class="muted">
                "This organization's settled usage over the last 30 days, by ledger booking date: the chart sums each day's charge in credits (1 credit = 1,000,000), the token mix splits the billed volume into input, cached read, output and reasoning, and the tables sum the window by key and by channel and model. Members see their own keys' usage plus the organization's shared rows."
            </p>
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || usage.get().map(|result| match result {
                    Ok(usage) => view! {
                        <UsageChart days=usage.days.clone() rows=usage.rows.clone()/>
                        <UsageTokenMix rows=usage.rows.clone()/>
                        <UsageKeysTable rows=usage.rows.clone()/>
                        <UsageTable rows=usage.rows/>
                    }
                        .into_any(),
                    Err(error) => view! { <p class="error" role="alert">{error.to_string()}</p> }
                        .into_any(),
                })}
            </Suspense>
        </section>
    }
}

/// The daily bar chart: one bar per day of the window, height scaled to the busiest
/// day's charge. Every bar carries its day and charge as text too — the chart is
/// decoration over the same numbers the table sums, never the only carrier
/// (DESIGN.md: nothing is colour alone).
#[component]
fn UsageChart(days: Vec<String>, rows: Vec<UsageDayView>) -> impl IntoView {
    let mut per_day: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    for row in &rows {
        *per_day.entry(row.day.as_str()).or_default() += row.charged_minor;
    }
    let max = per_day.values().copied().max().unwrap_or(0).max(1);
    view! {
        <div
            class="usage-chart"
            role="img"
            aria-label="Daily charged credits over the last 30 days"
        >
            {days
                .iter()
                .map(|day| {
                    let charged = per_day.get(day.as_str()).copied().unwrap_or(0);
                    let height = (charged as u64 * 100 / max as u64).max(if charged > 0 {
                        4
                    } else {
                        0
                    });
                    view! {
                        <div class="day" title=format!("{day}: {} credits", credits(charged))>
                            <div class="bar" style=format!("height:{height}%")></div>
                            <span class="visually-hidden">
                                {format!("{day}: {} credits", credits(charged))}
                            </span>
                        </div>
                    }
                })
                .collect::<Vec<_>>()}
        </div>
        <div class="usage-axis muted">
            <span class="mono">{days.first().cloned().unwrap_or_default()}</span>
            <span class="mono">{days.last().cloned().unwrap_or_default()}</span>
        </div>
    }
}

/// The window's rollup by channel and model: turns, the token sums, and what it
/// charged, rendered as credits like every other amount. With nothing to show the
/// table keeps its header and says why it is empty.
#[component]
fn UsageTable(rows: Vec<UsageDayView>) -> impl IntoView {
    let mut by_pair: std::collections::BTreeMap<(&str, &str), UsageDayView> =
        std::collections::BTreeMap::new();
    for row in &rows {
        by_pair
            .entry((row.channel.as_str(), row.model.as_str()))
            .and_modify(|sum| {
                sum.turns += row.turns;
                sum.input_tokens += row.input_tokens;
                sum.output_tokens += row.output_tokens;
                sum.cached_tokens += row.cached_tokens;
                sum.reasoning_tokens += row.reasoning_tokens;
                sum.charged_minor += row.charged_minor;
            })
            .or_insert_with(|| row.clone());
    }
    let rows: Vec<UsageDayView> = by_pair.into_values().collect();
    view! {
        <h2>"By channel and model"</h2>
        <table>
            <thead>
                <tr>
                    <th scope="col">"Channel"</th>
                    <th scope="col">"Model"</th>
                    <th scope="col" class="num">"Turns"</th>
                    <th scope="col" class="num">"Input tokens"</th>
                    <th scope="col" class="num">"Cached"</th>
                    <th scope="col" class="num">"Output tokens"</th>
                    <th scope="col" class="num">"Reasoning"</th>
                    <th scope="col" class="num">"Cost (credits)"</th>
                </tr>
            </thead>
            <tbody>
                {if rows.is_empty() {
                    view! {
                        <tr>
                            <td colspan="8" class="muted">
                                "No usage in the last 30 days."
                            </td>
                        </tr>
                    }
                        .into_any()
                } else {
                    view! {
                        <For
                            each=move || rows.clone()
                            key=|row| format!("{}:{}", row.channel, row.model)
                            let(row)
                        >
                            <tr>
                                <td>{row.channel.clone()}</td>
                                <td class="mono">{row.model.clone()}</td>
                                <td class="num">{row.turns}</td>
                                <td class="num">{row.input_tokens}</td>
                                <td class="num">{row.cached_tokens}</td>
                                <td class="num">{row.output_tokens}</td>
                                <td class="num">{row.reasoning_tokens}</td>
                                <td class="num mono">{credits(row.charged_minor)}</td>
                            </tr>
                        </For>
                    }
                        .into_any()
                }}
            </tbody>
        </table>
    }
}

/// The window's token mix: what share of the billed volume was fresh input,
/// cached reads, output and reasoning — a stacked bar beside its own numbers,
/// so the split is readable without the colour (issue #148).
#[component]
fn UsageTokenMix(rows: Vec<UsageDayView>) -> impl IntoView {
    let (mut input, mut cached, mut output, mut reasoning) = (0_i64, 0_i64, 0_i64, 0_i64);
    for row in &rows {
        input += row.input_tokens;
        cached += row.cached_tokens;
        output += row.output_tokens;
        reasoning += row.reasoning_tokens;
    }
    // Cached input is a subset of the input total, so the mix shows the
    // non-cached share — four disjoint parts that sum to the billed volume.
    let fresh = input - cached;
    let total = (fresh + cached + output + reasoning).max(1);
    let segments = [
        ("Input", fresh, "mix-input"),
        ("Cached read", cached, "mix-cached"),
        ("Output", output, "mix-output"),
        ("Reasoning", reasoning, "mix-reasoning"),
    ];
    // Cumulative rounding: each width is the share's running-total difference, so
    // the four always fill the bar to exactly 100% and no segment is more than a
    // rounding step off.
    let mut cumulative = 0_i64;
    let bars: Vec<(&str, i64, &str, u64)> = segments
        .iter()
        .map(|(name, count, class)| {
            let before = cumulative * 100 / total;
            cumulative += count;
            let width = (cumulative * 100 / total - before) as u64;
            (*name, *count, *class, width)
        })
        .collect();
    view! {
        <h2>"Token mix"</h2>
        {if rows.is_empty() {
            view! { <p class="muted">"No usage in the last 30 days."</p> }.into_any()
        } else {
            view! {
                <div
                    class="token-mix"
                    role="img"
                    aria-label="Token mix over the last 30 days"
                >
                    {bars
                        .iter()
                        .map(|(name, count, class, width)| {
                            view! {
                                <div
                                    class=format!("segment {class}")
                                    style=format!("width:{width}%")
                                    title=format!("{name}: {count} tokens")
                                >
                                    <span class="visually-hidden">
                                        {format!("{name}: {count} tokens")}
                                    </span>
                                </div>
                            }
                        })
                        .collect::<Vec<_>>()}
                </div>
                <ul class="token-mix-legend">
                    {segments
                        .iter()
                        .map(|(name, count, class)| {
                            let share = *count * 100 / total;
                            view! {
                                <li>
                                    <span class=format!("swatch {class}")></span>
                                    {format!("{name}: {count} tokens ({share}%)")}
                                </li>
                            }
                        })
                        .collect::<Vec<_>>()}
                </ul>
            }
                .into_any()
        }}
    }
}

/// The window's rollup by key: the same per-bucket sums the model table shows,
/// grouped by the key that paid so an owner sees which key spends. Shared
/// unattributed usage is a row of its own — it is nobody's key (issue #148).
#[component]
fn UsageKeysTable(rows: Vec<UsageDayView>) -> impl IntoView {
    #[derive(Clone)]
    struct KeySum {
        id: String,
        label: String,
        turns: i64,
        input: i64,
        output: i64,
        charged_minor: i64,
    }
    let mut by_key: std::collections::BTreeMap<String, KeySum> = Default::default();
    for row in &rows {
        let id = row.key_id.clone().unwrap_or_default();
        let entry = by_key.entry(id.clone()).or_insert_with(|| KeySum {
            id,
            label: row
                .key_label
                .clone()
                .unwrap_or_else(|| "Shared (no key)".to_owned()),
            turns: 0,
            input: 0,
            output: 0,
            charged_minor: 0,
        });
        entry.turns += row.turns;
        entry.input += row.input_tokens;
        entry.output += row.output_tokens;
        entry.charged_minor += row.charged_minor;
    }
    // The table answers "which key spends most", so it sorts by charge, not name.
    let mut rows: Vec<KeySum> = by_key.into_values().collect();
    rows.sort_by_key(|sum| std::cmp::Reverse(sum.charged_minor));
    view! {
        <h2>"By key"</h2>
        <table>
            <thead>
                <tr>
                    <th scope="col">"Key"</th>
                    <th scope="col" class="num">"Turns"</th>
                    <th scope="col" class="num">"Input tokens"</th>
                    <th scope="col" class="num">"Output tokens"</th>
                    <th scope="col" class="num">"Cost (credits)"</th>
                </tr>
            </thead>
            <tbody>
                {if rows.is_empty() {
                    view! {
                        <tr>
                            <td colspan="5" class="muted">
                                "No usage in the last 30 days."
                            </td>
                        </tr>
                    }
                        .into_any()
                } else {
                    view! {
                        <For
                            each=move || rows.clone()
                            key=|row| row.id.clone()
                            let(row)
                        >
                            <tr>
                                <td>{row.label.clone()}</td>
                                <td class="num">{row.turns}</td>
                                <td class="num">{row.input}</td>
                                <td class="num">{row.output}</td>
                                <td class="num mono">{credits(row.charged_minor)}</td>
                            </tr>
                        </For>
                    }
                        .into_any()
                }}
            </tbody>
        </table>
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
    /// The bundle matches the content hash and the proof links it to the tree head;
    /// carries what recomputing the entry's settlement description concluded —
    /// recomputed itself, or skipped because the record is not a settlement's or
    /// is written in a schema this verifier does not know.
    Passed(oxsum_verify::ChargeCheck),
    /// The bundle does not match the hash, or the proof does not link to the head.
    Failed,
    /// Inclusion passed but the charge does not recompute from the record's own
    /// fields — an honest settlement never fails this.
    ChargeMismatch,
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
/// The chat page links here with both halves prefilled (`?bundle=…&contentHash=…`),
/// and the bills page with the entry named (`?entry=…&contentHash=…`) — the bundle is
/// then fetched from the reader's own organization, which needs a session, so a
/// logged-out visitor still pastes by hand. Either way the check runs on load.
#[component]
fn VerifyPage() -> impl IntoView {
    let query = use_query_map();
    // Prefilled from the query string by the chat page's "verify this bill" links.
    // Read once, from the URL: SSR and hydration agree on the values.
    let (bundle, set_bundle) = signal(query.get().get("bundle").unwrap_or_default());
    let (content_hash, set_content_hash) =
        signal(query.get().get("contentHash").unwrap_or_default());
    let (outcome, set_outcome) = signal(Option::<VerifyOutcome>::None);
    let (load_error, set_load_error) = signal(Option::<String>::None);

    // `?entry=<id>` fetches the bundle in the browser — the server function answers
    // only for a logged-in reader of the entry's own organization. `LocalResource`
    // never runs during SSR, so the link cannot leak an entry to a logged-out render.
    let entry = StoredValue::new(query.get().get("entry"));
    let fetched = LocalResource::new(move || async move {
        match entry.get_value() {
            Some(id) => Some(get_entry_bundle(id).await),
            None => None,
        }
    });
    Effect::new(move |_| {
        if let Some(Some(result)) = fetched.get() {
            match result {
                Ok(json) => set_bundle.set(json),
                Err(error) => set_load_error.set(Some(error.to_string())),
            }
        }
    });

    let ready = move || !(bundle.get().trim().is_empty() || content_hash.get().trim().is_empty());

    let run = move || {
        let bundle_text = bundle.get_untracked();
        let outcome = match oxsum_verify::Hash::parse_hex(content_hash.get_untracked().trim()) {
            Err(_) => VerifyOutcome::BadHash,
            Ok(expected) => match oxsum_verify::verify_bundle(&bundle_text, &expected) {
                Err(error) => VerifyOutcome::BadBundle(error.to_string()),
                Ok(true) => {
                    // Inclusion proven; now whether the charge inside the entry is
                    // what its own usage and rates add up to. The description is the
                    // entry's — the same bytes the content hash already covered.
                    let description = serde_json::from_str::<serde_json::Value>(&bundle_text)
                        .ok()
                        .and_then(|bundle| {
                            bundle
                                .get("entry")?
                                .get("description")?
                                .as_str()
                                .map(str::to_owned)
                        })
                        .unwrap_or_default();
                    match oxsum_verify::verify_charge(&description) {
                        oxsum_verify::ChargeCheck::Mismatch => VerifyOutcome::ChargeMismatch,
                        check => VerifyOutcome::Passed(check),
                    }
                }
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
                {move || {
                    // `?entry=` fetched the bundle but the hash stays empty on purpose: it
                    // is the half of the check that must come from the verifier's own
                    // record, because a hash this page fetched itself would prove nothing
                    // about the server that answered (issue #183).
                    (entry.get_value().is_some()
                        && !bundle.get().trim().is_empty()
                        && content_hash.get().trim().is_empty())
                    .then(|| view! {
                        <p class="muted" role="note">
                            "The bundle was fetched for you; the content hash was not, on purpose — "
                            "it is the half of the check that has to come from your own record, "
                            "not from this server. Paste the hash saved with the bill."
                        </p>
                    })
                }}
                {move || outcome.get().map(|outcome| view! { <Verdict outcome=outcome/> })}
                {move || load_error.get().map(|error| view! {
                    <p class="error" role="alert">
                        "The entry's proof could not be loaded — log in to the organization it belongs to, or paste the bundle by hand. ("{error}")"
                    </p>
                })}
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
        VerifyOutcome::Passed(check) => {
            use oxsum_verify::ChargeCheck;
            let charge = match check {
                ChargeCheck::Recomputed => {
                    " The recorded charge itself recomputes from the usage and rates inside the entry — the arithmetic checks, not only the bytes."
                }
                ChargeCheck::NotASettlement => {
                    " The entry is not a usage settlement, so there is no charge to recompute."
                }
                ChargeCheck::OlderSchema => {
                    " The record predates charge verification — the entry is proven, but its arithmetic is too old to recompute."
                }
                ChargeCheck::NewerSchema { .. } => {
                    " The record is newer than this verifier — the entry is proven, but its arithmetic is skipped rather than guessed at."
                }
                ChargeCheck::Mismatch => unreachable!("a mismatch is not a pass"),
            };
            (
                "success",
                "✓",
                "Verification passed.",
                format!(
                    "The bundle matches the content hash, and the inclusion proof links it to the tree head.{charge}"
                ),
            )
        }
        VerifyOutcome::ChargeMismatch => (
            "error",
            "✗",
            "The charge does not add up.",
            "The entry is proven unaltered, but the charge it records does not recompute from the usage and rates it carries — something was written wrong."
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
