//! The browser contract: the assets the server serves and the HTML the browser
//! needs.
//!
//! Every other server suite stops at what a router answers. The defect that
//! reached `main` on 2026-10-03 was invisible from there and visible from the
//! served HTML: the login form fell back to a native GET that put the
//! credentials in the URL (issue #46).
//!
//! No build step is needed anymore: the Topcoat pages render server-side with
//! no wasm bundle, so these tests run unignored. No database is needed either:
//! `/login`, the stylesheet and the scripts are answered without one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use regex::Regex;
use tower::ServiceExt;

/// The app over a pool that never connects: the pages these tests hit do not
/// reach the database.
fn app() -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL");
    oxsum_server::app(Db::from_pool(pool), Config::new(Signup::Open, None))
}

/// One response, with its body kept as bytes.
struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Res {
    /// The body as text; for the HTML and JS assertions.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn content_type(&self) -> String {
        self.headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned()
    }
}

/// One GET against the app.
async fn get(app: &Router, path: &str) -> Res {
    let req = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("the request is valid");
    let res = app.clone().oneshot(req).await.expect("the app answers");
    let status = res.status();
    let headers = res.headers().clone();
    let body = res
        .into_body()
        .collect()
        .await
        .expect("the body is readable")
        .to_bytes()
        .to_vec();
    Res {
        status,
        headers,
        body,
    }
}

/// The shell's stylesheet is served: the pages link it, so a 404 leaves them unstyled.
#[tokio::test]
async fn the_stylesheet_is_served() {
    let app = app();
    let res = get(&app, "/style/main.css").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /style/main.css answered {}",
        res.status
    );
    assert!(
        res.content_type().contains("text/css"),
        "the stylesheet is CSS, got {}",
        res.content_type()
    );
    assert!(!res.body.is_empty(), "the stylesheet is empty");
}

/// The admin pages' script is served as JavaScript.
#[tokio::test]
async fn the_admin_script_is_served() {
    let app = app();
    let res = get(&app, "/admin.js").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /admin.js answered {}",
        res.status
    );
    assert!(
        res.content_type().contains("javascript"),
        "the admin script is JavaScript, got {}",
        res.content_type()
    );
    assert!(!res.body.is_empty(), "the admin script is empty");
}

/// The chat page's script is served as JavaScript.
#[tokio::test]
async fn the_chat_script_is_served() {
    let app = app();
    let res = get(&app, "/chat.js").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /chat.js answered {}",
        res.status
    );
    assert!(
        res.content_type().contains("javascript"),
        "the chat script is JavaScript, got {}",
        res.content_type()
    );
    assert!(!res.body.is_empty(), "the chat script is empty");
}

/// `/login` carries what the browser contract needs from the HTML: a login form
/// whose method is `post`.
///
/// The method is what a browser falls back to when JavaScript never runs: a
/// form without one issues a native `GET`, which puts the email and the
/// password in the query string (issue #46).
#[tokio::test]
async fn the_login_page_carries_a_post_form() {
    let app = app();
    let res = get(&app, "/login").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /login answered {}",
        res.status
    );
    let html = res.text();

    let form = Regex::new(r#"(?s)<form[^>]*\bmethod="post"[^>]*>.*?name="password""#)
        .expect("the login form pattern is valid");
    assert!(
        form.is_match(&html),
        "the login form is not a POST form: a native submit puts the credentials in the URL (issue #46)"
    );
}

/// `/verify` is public: anyone holding a bill can load it, no session needed.
#[tokio::test]
async fn the_verify_page_is_public() {
    let app = app();
    let res = get(&app, "/verify").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /verify answered {}",
        res.status
    );
    assert!(
        res.text().contains("Proof bundle"),
        "the verify page names the bundle field"
    );
}

/// The dashboard redirects an anonymous visitor to `/login` instead of
/// rendering: pages are navigation, not API calls.
#[tokio::test]
async fn the_dashboard_redirects_an_anonymous_visitor_to_login() {
    let app = app();
    let res = get(&app, "/dashboard").await;
    assert_eq!(
        res.status,
        StatusCode::SEE_OTHER,
        "GET /dashboard answered {}",
        res.status
    );
    let location = res
        .headers
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        location, "/login",
        "the dashboard sends visitors to {location}"
    );
}
