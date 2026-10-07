//! HTTP tests for GitHub OAuth (issue #152): the authorize redirect, the
//! callback's failure cases, and the full three-legged login against a stub
//! provider — the state's one-spend rule and the session the callback mints.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::{Json, Router};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// What the stub provider answers, and what it saw. Tests set the emails the
/// provider reports and read back the exchange's form fields.
#[derive(Clone, Default)]
struct Stub {
    inner: Arc<Mutex<StubInner>>,
}

#[derive(Default)]
struct StubInner {
    /// The id `/user` reports.
    id: u64,
    /// The rows `/user/emails` reports: `(email, primary, verified)`.
    emails: Vec<(String, bool, bool)>,
    /// The `code` the token exchange received, when it ran.
    code: Option<String>,
    /// When set, the token exchange answers an error body instead of a token.
    refuse: bool,
}

/// Serves the three endpoints the flow calls, on a loopback port. The user id
/// is random per stub: the shared database keeps `oauth_accounts` links across
/// runs, and a fixed id would resolve to a previous run's account.
async fn github_stub() -> (Stub, String) {
    let stub = Stub::default();
    stub.inner.lock().unwrap().id = (Uuid::new_v4().as_u128() % 1_000_000_000) as u64 + 1;
    let seen = stub.clone();
    let app = Router::new()
        .route(
            "/login/oauth/access_token",
            axum::routing::post(move |body: String| {
                let seen = seen.clone();
                async move {
                    // The exchange posts a form; keep the code it carried.
                    let code = body
                        .split('&')
                        .find_map(|pair| pair.strip_prefix("code=").map(str::to_owned));
                    let mut inner = seen.inner.lock().unwrap();
                    inner.code = code;
                    if inner.refuse {
                        return Json(json!({"error": "bad_verification_code"})).into_response();
                    }
                    Json(json!({
                        "access_token": "gho-stub",
                        "token_type": "bearer",
                        "scope": "read:user user:email",
                    }))
                    .into_response()
                }
            }),
        )
        .route(
            "/user",
            axum::routing::get({
                let stub = stub.clone();
                move || {
                    let stub = stub.clone();
                    async move { Json(json!({"id": stub.inner.lock().unwrap().id})) }
                }
            }),
        )
        .route(
            "/user/emails",
            axum::routing::get({
                let stub = stub.clone();
                move || {
                    let stub = stub.clone();
                    async move {
                        let emails: Vec<Value> = stub
                            .inner
                            .lock()
                            .unwrap()
                            .emails
                            .iter()
                            .map(|(email, primary, verified)| {
                                json!({"email": email, "primary": primary, "verified": verified})
                            })
                            .collect();
                        Json(json!(emails))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a stub port");
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, app).into_future());
    (stub, url)
}

/// An app over a real database, with GitHub OAuth pointed at the stub when the
/// test gives one.
async fn online_app(
    url: &str,
    signup: Signup,
    github: Option<oxsum_server::oauth::GitHub>,
) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let mut config = Config::new(signup, None);
    if let Some(github) = github {
        config = config.with_github(github);
    }
    (oxsum_server::app(db, config), pool)
}

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

/// The stub, an app wired to it, and the pool — the standard test world.
async fn oauth_app(url: &str) -> (Router, PgPool, Stub) {
    let (stub, stub_url) = github_stub().await;
    let github = oxsum_server::oauth::GitHub::new(
        "client-id".to_owned(),
        "client-secret".to_owned(),
        "http://oxsum.test".to_owned(),
        stub_url.clone(),
        stub_url,
    );
    let (app, pool) = online_app(url, Signup::Open, Some(github)).await;
    (app, pool, stub)
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => oauth_app(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

struct Res {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
    location: Option<String>,
}

/// A GET, the only verb the OAuth surface speaks.
async fn get(app: &Router, path: &str, cookie: Option<&str>) -> Res {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let header = |name: &str| {
        res.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let set_cookie = header("set-cookie");
    let location = header("location");
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        body,
        set_cookie,
        location,
    }
}

/// The `state` out of an authorize redirect's query string.
fn state_from(location: &str) -> String {
    let pair = location
        .split('&')
        .find(|p| p.starts_with("state="))
        .expect("the authorize URL carries a state");
    pair.trim_start_matches("state=").to_owned()
}

/// A fresh verified email the stub can report, unique per call.
fn stub_email(tag: &str) -> String {
    format!(
        "{tag}_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    )
}

/// The token out of a `Set-Cookie` value.
fn cookie_token(set_cookie: &str) -> &str {
    set_cookie
        .strip_prefix("oxsum_session=")
        .and_then(|rest| rest.split(';').next())
        .expect("the cookie carries the token")
}

#[tokio::test]
async fn the_methods_endpoint_reports_the_provider() {
    let Some(u) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let (app, _pool) = online_app(&u, Signup::Open, None).await;
    let res = get(&app, "/api/v1/auth/methods", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["data"]["oauthGithub"], false);

    let (app, _pool, _stub) = oauth_app(&u).await;
    let res = get(&app, "/api/v1/auth/methods", None).await;
    assert_eq!(res.body["data"]["oauthGithub"], true);
}

#[tokio::test]
async fn an_unconfigured_start_answers_not_found() {
    let Some(u) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let (app, _pool) = online_app(&u, Signup::Open, None).await;
    let res = get(&app, "/api/v1/auth/oauth/github", None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_start_redirects_with_a_minted_state() {
    let (app, _pool, _stub) = app_or_skip!();

    let res = get(&app, "/api/v1/auth/oauth/github", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let location = res.location.expect("the redirect carries a target");
    assert!(location.contains("/login/oauth/authorize?"));
    assert!(location.contains("client_id=client-id"));
    assert!(location.contains("scope=read%3Auser%20user%3Aemail"));
    assert!(location.contains("redirect_uri=http%3A%2F%2Foxsum.test"));
    let state = state_from(&location);
    assert!(
        state.starts_with("oxo-"),
        "the minted state carries its mark"
    );
}

#[tokio::test]
async fn the_full_flow_mints_a_session() {
    let (app, _pool, stub) = app_or_skip!();
    let email = stub_email("flow");
    {
        let mut inner = stub.inner.lock().unwrap();
        inner.emails = vec![(email.clone(), true, true)];
    }

    let res = get(&app, "/api/v1/auth/oauth/github", None).await;
    let state = state_from(&res.location.unwrap());
    let res = get(
        &app,
        &format!("/api/v1/auth/oauth/github/callback?code=fine&state={state}"),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location.as_deref(), Some("/dashboard"));
    let cookie = res.set_cookie.expect("the session cookie is set");
    assert!(cookie.contains("HttpOnly"));

    // The token exchange ran, and the session it minted answers like a
    // password login's.
    assert_eq!(stub.inner.lock().unwrap().code.as_deref(), Some("fine"));
    let res = get(&app, "/api/v1/session", Some(cookie_token(&cookie))).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["data"]["user"]["email"], email);
}

#[tokio::test]
async fn a_bad_state_bounces_back_to_login() {
    let (app, _pool, _stub) = app_or_skip!();

    for path in [
        "/api/v1/auth/oauth/github/callback?code=x&state=oxo-forged",
        "/api/v1/auth/oauth/github/callback?error=access_denied",
        "/api/v1/auth/oauth/github/callback",
    ] {
        let res = get(&app, path, None).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{path}");
        assert_eq!(
            res.location.as_deref(),
            Some("/login?error=oauth"),
            "{path}"
        );
        assert!(res.set_cookie.is_none(), "{path}");
    }
}

#[tokio::test]
async fn a_spent_state_cannot_log_in_twice() {
    let (app, _pool, stub) = app_or_skip!();
    {
        let mut inner = stub.inner.lock().unwrap();
        inner.emails = vec![(stub_email("replay"), true, true)];
    }
    let res = get(&app, "/api/v1/auth/oauth/github", None).await;
    let state = state_from(&res.location.unwrap());
    let callback = format!("/api/v1/auth/oauth/github/callback?code=fine&state={state}");

    let first = get(&app, &callback, None).await;
    assert_eq!(first.location.as_deref(), Some("/dashboard"));
    // The replay: same code, same state — spent is spent.
    let second = get(&app, &callback, None).await;
    assert_eq!(second.location.as_deref(), Some("/login?error=oauth"));
}

#[tokio::test]
async fn an_unverified_email_completes_nothing() {
    let (app, _pool, stub) = app_or_skip!();
    {
        let mut inner = stub.inner.lock().unwrap();
        // A public email the provider never verified must not open an
        // account — this is the linking rule the flow exists to enforce.
        inner.emails = vec![("victim@example.com".to_owned(), true, false)];
    }
    let res = get(&app, "/api/v1/auth/oauth/github", None).await;
    let state = state_from(&res.location.unwrap());
    let res = get(
        &app,
        &format!("/api/v1/auth/oauth/github/callback?code=fine&state={state}"),
        None,
    )
    .await;
    assert_eq!(res.location.as_deref(), Some("/login?error=oauth"));
    assert!(res.set_cookie.is_none());
}

#[tokio::test]
async fn a_refused_exchange_bounces() {
    let (app, _pool, stub) = app_or_skip!();
    stub.inner.lock().unwrap().refuse = true;

    let res = get(&app, "/api/v1/auth/oauth/github", None).await;
    let state = state_from(&res.location.unwrap());
    let res = get(
        &app,
        &format!("/api/v1/auth/oauth/github/callback?code=bad&state={state}"),
        None,
    )
    .await;
    assert_eq!(res.location.as_deref(), Some("/login?error=oauth"));
}

#[tokio::test]
async fn an_invite_deployment_refuses_new_accounts_but_logs_existing_in() {
    let Some(u) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let (stub, stub_url) = github_stub().await;
    let github = oxsum_server::oauth::GitHub::new(
        "client-id".to_owned(),
        "client-secret".to_owned(),
        "http://oxsum.test".to_owned(),
        stub_url.clone(),
        stub_url,
    );
    let (app, pool) = online_app(&u, Signup::Invite, Some(github)).await;

    // A stranger: invite mode refuses the registration the callback would run.
    {
        let mut inner = stub.inner.lock().unwrap();
        inner.emails = vec![(stub_email("stranger"), true, true)];
    }
    let res = get(&app, "/api/v1/auth/oauth/github", None).await;
    let state = state_from(&res.location.unwrap());
    let res = get(
        &app,
        &format!("/api/v1/auth/oauth/github/callback?code=fine&state={state}"),
        None,
    )
    .await;
    assert_eq!(res.location.as_deref(), Some("/login?error=oauth"));

    // An account that exists still signs in: the mode gates registration, not
    // the credential. The account is made through core — the open signup
    // endpoint is exactly what invite mode closes.
    let email = stub_email("member");
    Db::from_pool(pool.clone())
        .register(oxsum_core::NewUser {
            email: email.clone(),
            password: "a long enough password".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registering the member");
    {
        let mut inner = stub.inner.lock().unwrap();
        inner.emails = vec![(email.clone(), true, true)];
    }
    let res = get(&app, "/api/v1/auth/oauth/github", None).await;
    let state = state_from(&res.location.unwrap());
    let res = get(
        &app,
        &format!("/api/v1/auth/oauth/github/callback?code=fine&state={state}"),
        None,
    )
    .await;
    assert_eq!(res.location.as_deref(), Some("/dashboard"));
    let res = get(
        &app,
        "/api/v1/session",
        Some(cookie_token(&res.set_cookie.unwrap())),
    )
    .await;
    assert_eq!(res.body["data"]["user"]["email"], email);
}
