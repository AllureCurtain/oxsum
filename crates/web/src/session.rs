//! The session behind a dashboard request: the database, the tenants, and who is
//! logged in.
//!
//! The mechanism is the existing one: the `HttpOnly` session cookie names a
//! session row, and `Db::authenticate_session` resolves it to a
//! [`SessionPrincipal`]. Login and logout go through `Db::login`/`Db::logout`
//! directly — the same core calls the axum session endpoints use — so the
//! cookie the pages set names a real session row either way.
//!
//! A missing or dead session is "not logged in", whatever the cause — the same
//! indistinguishability the auth middleware keeps. A storage failure is an
//! error, not a login state.

use oxsum_core::{Db, SESSION_COOKIE, SessionPrincipal, Tenants, WalletError};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::cookie::{Cookie, Cookies, SameSite, cookies};
use topcoat::router::error::{see_other, unauthorized};

use crate::{db, tenants, web_config};

/// The session behind the request: the database, the tenants, and who is logged in.
pub(crate) struct SessionCtx {
    pub db: Db,
    pub tenants: Tenants,
    pub principal: SessionPrincipal,
}

/// Resolves the request's session cookie, or redirects to `/login` when there
/// is no live session.
pub(crate) async fn session_ctx(cx: &Cx) -> Result<SessionCtx> {
    let db = db(cx)?;
    let tenants = tenants(cx)?;
    let token = session_token(cx).ok_or_else(unauthorized)?;
    let principal = db
        .authenticate_session(&token)
        .await
        .map_err(|_| internal("the session could not be checked"))?
        .ok_or_else(unauthorized)?;
    Ok(SessionCtx {
        db,
        tenants,
        principal,
    })
}

/// The session behind a page: like [`session_ctx`], but a missing session
/// redirects to `/login` instead of answering 401 — pages are navigation, not
/// API calls.
pub(crate) async fn page_ctx(cx: &Cx) -> Result<SessionCtx> {
    session_ctx(cx)
        .await
        .map_err(|_| see_other("/login").into())
}

/// The session cookie's value, if the request carries one.
pub(crate) fn session_token(cx: &Cx) -> Option<String> {
    cookies(cx)
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned())
        .filter(|value| !value.is_empty())
}

/// Logs in: validates the credentials through the core and sets the session
/// cookie, exactly the cookie the axum login endpoint sets
/// (`crates/server/src/routes.rs`).
pub(crate) async fn login(cx: &Cx, email: &str, password: &str) -> Result<SessionPrincipal> {
    let db = db(cx)?;
    let created = db
        .login(email, password)
        .await
        .map_err(|error| match error {
            // A wrong credential fails in words; anything else stays out of the response.
            // `InvalidCredentials` deliberately names nothing — the caller must not learn
            // whether the email or the password was wrong.
            WalletError::InvalidInput(_) | WalletError::InvalidCredentials => unauthorized().into(),
            _ => internal("the login could not be completed"),
        })?;
    let secure = web_config(cx)?.session_cookie_secure;
    let mut builder = Cookie::build((SESSION_COOKIE, created.token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax);
    if secure {
        builder = builder.secure(true);
    }
    cookies(cx).add(builder.build());
    Ok(created.principal)
}

/// Logs out: revokes the session the cookie names and clears the cookie. Always
/// succeeds — logging out twice is not an error.
pub(crate) async fn logout(cx: &Cx) -> Result<()> {
    let db = db(cx)?;
    if let Some(token) = session_token(cx) {
        db.logout(&token)
            .await
            .map_err(|_| internal("the logout could not be completed"))?;
    }
    let secure = web_config(cx)?.session_cookie_secure;
    let mut builder = Cookie::build((SESSION_COOKIE, ""))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        // `Max-Age=0`: one mechanism, no date formatting — the same cookie the
        // axum logout endpoint clears with (`crates/server/src/routes.rs`).
        .max_age(topcoat::cookie::time::Duration::ZERO);
    if secure {
        builder = builder.secure(true);
    }
    cookies(cx).add(builder.build());
    Ok(())
}

fn internal(message: &str) -> topcoat::Error {
    topcoat::router::error::internal_server_error(topcoat::Error::msg(message.to_owned())).into()
}
