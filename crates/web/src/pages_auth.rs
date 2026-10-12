//! The public auth pages: `/`, `/login`, `/register`, `/logout`,
//! `/verify-email`, `/forgot-password`, `/reset-password`.
//!
//! Login and logout run server-side against `Db::login`/`Db::logout` — the same
//! core calls the axum session endpoints use — so no browser JavaScript is
//! needed to set or clear the `HttpOnly` session cookie.

use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::{SeeOther, not_found, see_other};
use topcoat::router::{RouterBuilder, page, parse_query_params, route};
use topcoat::view::{View, view};

use crate::session::{login, logout, session_token};
use crate::views::{error_note, plain};
use crate::{db, web_config};

/// Registers the auth pages on the router builder.
pub(crate) fn register(builder: RouterBuilder) -> RouterBuilder {
    builder
        .route(root_redirect)
        .page(login_page)
        .route(login_submit)
        .page(register_page)
        .page(register_submit)
        .route(logout_submit)
        .page(verify_email_page)
        .page(forgot_password_page)
        .page(forgot_password_submit)
        .page(reset_password_page)
        .page(reset_password_submit)
}

/// `/` redirects to the dashboard, as before.
#[route(GET "/")]
async fn root_redirect() -> Result<SeeOther> {
    Ok(see_other("/dashboard"))
}

/// The login form.
#[page("/login")]
async fn login_page(cx: &Cx) -> Result<impl View> {
    // Already logged in: nothing to do here.
    if session_token(cx).is_some() {
        return Err(see_other("/dashboard").into());
    }
    let oauth = web_config(cx)?.oauth_github;
    let query_error = parse_query_params::<LoginQuery>(cx)
        .map(|query| query.error)
        .unwrap_or_default();
    let error: Option<&'static str> = match query_error.as_deref() {
        Some("oauth") => Some("GitHub sign-in did not complete — try again, or use your password."),
        Some("failed") => Some("Invalid email or password."),
        _ => None,
    };
    Ok(view! {
        plain(
            title: "Log in",
            <form class="card" method="post" action="/login" aria-label="Log in">
                <h1>"oxsum"</h1>
                <p class="muted">"Log in to the dashboard."</p>
                if let Some(message) = error {
                    error_note(message: message.to_owned())
                }
                <label>
                    "Email"
                    <input type="email" name="email" autocomplete="username" required="required" />
                </label>
                <label>
                    "Password"
                    <input type="password" name="password" autocomplete="current-password" required="required" />
                </label>
                <button type="submit">"Log in"</button>
                if oauth {
                    <p class="divider muted">"or"</p>
                    <a class="oauth" href="/api/v1/auth/oauth/github">"Continue with GitHub"</a>
                }
                <p class="muted">
                    <a href="/forgot-password">"Forgot your password?"</a>
                </p>
            </form>
        )
    })
}

#[derive(Deserialize, Default)]
struct LoginQuery {
    error: Option<String>,
}

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    password: String,
}

/// Handles the login form: validates the credentials and sets the session cookie.
#[route(POST "/login")]
async fn login_submit(cx: &Cx, Form(form): Form<LoginForm>) -> Result<SeeOther> {
    match login(cx, &form.email, &form.password).await {
        Ok(_) => Ok(see_other("/dashboard")),
        Err(_) => Ok(see_other("/login?error=failed")),
    }
}

/// The registration page: the invitee follows a link an owner or admin handed them.
#[page("/register")]
async fn register_page(cx: &Cx) -> Result<impl View> {
    let query = parse_query_params::<RegisterQuery>(cx).unwrap_or_default();
    let invite = query.invite.unwrap_or_default();
    let error: Option<String> = query.error;
    Ok(view! {
        plain(
            title: "Register",
            <form class="card" method="post" action="/register" aria-label="Register">
                <h1>"oxsum"</h1>
                <p class="muted">"Create your account with an invitation."</p>
                if let Some(message) = error {
                    error_note(message: message)
                }
                <input type="hidden" name="invite" value=(invite) />
                <label>
                    "Email"
                    <input type="email" name="email" autocomplete="username" required="required" />
                </label>
                <label>
                    "Password"
                    <input type="password" name="password" autocomplete="new-password" required="required" />
                </label>
                <button type="submit">"Create account"</button>
            </form>
        )
    })
}

#[derive(Deserialize, Default)]
struct RegisterQuery {
    invite: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct RegisterForm {
    invite: String,
    email: String,
    password: String,
}

/// Handles registration: redeems the invitation and creates the account.
#[page(POST "/register")]
async fn register_submit(cx: &Cx, Form(form): Form<RegisterForm>) -> Result<impl View> {
    let db = db(cx)?;
    let registration = db
        .redeem_invitation(
            &form.invite,
            oxsum_core::NewUser {
                email: form.email.clone(),
                password: form.password.clone(),
                organization_name: None,
            },
        )
        .await;
    match registration {
        Ok(registration) => {
            let org_name = registration.organization.name;
            let secret = registration.api_key.secret;
            Ok(view! {
                plain(
                    title: "Registered",
                    <div class="card">
                        <h1>"Welcome"</h1>
                        <p>
                            "You joined "
                            (org_name)
                            "."
                        </p>
                        <p class="muted">
                            "Your API key — it is shown here and nowhere else:"
                        </p>
                        <p><code>(secret)</code></p>
                        <p><a href="/login">"Log in"</a></p>
                    </div>
                )
            })
        }
        Err(error) => {
            let message = match error {
                oxsum_core::WalletError::NotFound(message)
                | oxsum_core::WalletError::InvalidInput(message)
                | oxsum_core::WalletError::Conflict(message) => message,
                _ => "the registration could not be completed".to_owned(),
            };
            let back = format!(
                "/register?invite={}&error={}",
                url_encode(&form.invite),
                url_encode(&message)
            );
            Err(see_other(back).into())
        }
    }
}

/// Logs out: revokes the session and clears the cookie, then back to `/login`.
///
/// Always succeeds — logging out twice is not an error, the same promise the
/// axum logout endpoint keeps.
#[route(POST "/logout")]
async fn logout_submit(cx: &Cx) -> Result<SeeOther> {
    logout(cx).await?;
    Ok(see_other("/login"))
}

/// The verification mail's landing page: reads `?token=` and consumes it once.
/// A refresh answers the token's spent state, which is honest.
#[page("/verify-email")]
async fn verify_email_page(cx: &Cx) -> Result<impl View> {
    let query = parse_query_params::<TokenQuery>(cx).unwrap_or_default();
    let db = db(cx)?;
    let verified = match query.token {
        Some(token) if !token.is_empty() => {
            let user_id = db
                .consume_email_token(&token, oxsum_core::EmailPurpose::Verify)
                .await
                .map_err(|_| not_found())?;
            match user_id {
                Some(user_id) => db
                    .mark_email_verified(user_id)
                    .await
                    .map(|_| true)
                    .unwrap_or(false),
                None => false,
            }
        }
        _ => false,
    };
    Ok(view! {
        plain(
            title: "Verify email",
            <div class="card">
                <h1>"Email verification"</h1>
                if verified {
                    <p>"Your email is verified. You can now log in."</p>
                    <p><a href="/login">"Log in"</a></p>
                } else {
                    <p class="error">"This verification link is not valid or has already been used."</p>
                }
            </div>
        )
    })
}

#[derive(Deserialize, Default)]
struct TokenQuery {
    token: Option<String>,
}

/// The forgot-password form.
#[page("/forgot-password")]
async fn forgot_password_page() -> Result<impl View> {
    Ok(view! {
        plain(
            title: "Forgot password",
            <form class="card" method="post" action="/forgot-password" aria-label="Forgot password">
                <h1>"Reset your password"</h1>
                <p class="muted">"We will mail you a reset link."</p>
                <label>
                    "Email"
                    <input type="email" name="email" autocomplete="username" required="required" />
                </label>
                <button type="submit">"Send reset link"</button>
            </form>
        )
    })
}

#[derive(Deserialize)]
struct ForgotForm {
    email: String,
}

/// Handles the forgot-password form: mints a reset token and mails it, when the
/// deployment can send mail at all.
#[page(POST "/forgot-password")]
async fn forgot_password_submit(cx: &Cx, Form(form): Form<ForgotForm>) -> Result<impl View> {
    let db = db(cx)?;
    // The endpoint answers the same either way: whether the address exists is
    // not the caller's business.
    let _ = db
        .mint_email_token_for_address(&form.email, oxsum_core::EmailPurpose::Reset)
        .await;
    Ok(view! {
        plain(
            title: "Forgot password",
            <div class="card">
                <h1>"Check your mail"</h1>
                <p>"If that address has an account, a reset link is on its way."</p>
                <p><a href="/login">"Back to log in"</a></p>
            </div>
        )
    })
}

/// The reset-password form: the mailed `?token=` names the reset.
#[page("/reset-password")]
async fn reset_password_page(cx: &Cx) -> Result<impl View> {
    let query = parse_query_params::<TokenQuery>(cx).unwrap_or_default();
    let token = query.token.unwrap_or_default();
    Ok(view! {
        plain(
            title: "Reset password",
            <form class="card" method="post" action="/reset-password" aria-label="Reset password">
                <h1>"Choose a new password"</h1>
                <input type="hidden" name="token" value=(token) />
                <label>
                    "New password"
                    <input type="password" name="password" autocomplete="new-password" required="required" />
                </label>
                <button type="submit">"Reset password"</button>
            </form>
        )
    })
}

#[derive(Deserialize)]
struct ResetForm {
    token: String,
    password: String,
}

/// Handles the reset: consumes the token and sets the new password.
#[page(POST "/reset-password")]
async fn reset_password_submit(cx: &Cx, Form(form): Form<ResetForm>) -> Result<impl View> {
    let db = db(cx)?;
    let user_id = db
        .consume_email_token(&form.token, oxsum_core::EmailPurpose::Reset)
        .await
        .map_err(|_| not_found())?;
    let done = match user_id {
        Some(user_id) => db.reset_password(user_id, &form.password).await.is_ok(),
        None => false,
    };
    Ok(view! {
        plain(
            title: "Reset password",
            <div class="card">
                <h1>"Password reset"</h1>
                if done {
                    <p>"Your password is reset. You can now log in."</p>
                    <p><a href="/login">"Log in"</a></p>
                } else {
                    <p class="error">"This reset link is not valid or has already been used."</p>
                }
            </div>
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
