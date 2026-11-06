//! `GET /api/v1/branding` (issue #614, T23): the seller branding the customer
//! portal applies. The endpoint is a pure config read — public, no database —
//! so the suite builds the app over a lazy pool and drives it with a oneshot
//! request the way `browser_contract` does. The `Config::with_branding` seam
//! stands in for the environment, the same way the other optional config tests
//! do.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Branding, Config, Signup};
use serde_json::Value;
use tower::ServiceExt;

/// The app over a pool that never connects: branding reads only the config.
fn app(config: Config) -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL");
    oxsum_server::app(Db::from_pool(pool), config)
}

async fn get(app: &Router, path: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("the request is valid");
    let res = app.clone().oneshot(req).await.expect("the app answers");
    let status = res.status();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("the body is readable")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// An unconfigured deployment answers the compiled-in default — the name the
/// binary ships with, no logo, no accent — and asks for no credential.
#[tokio::test]
async fn the_default_branding_needs_no_credential() {
    let app = app(Config::new(Signup::Open, None));
    let (status, body) = get(&app, "/api/v1/branding").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["name"], "oxsum");
    assert!(body["data"]["logoUrl"].is_null());
    assert!(body["data"]["accentColor"].is_null());
}

/// A configured deployment serves the seller's name, logo and accent verbatim —
/// the portal owns how they render; the endpoint only carries them.
#[tokio::test]
async fn the_configured_branding_is_served() {
    let config = Config::new(Signup::Open, None).with_branding(Branding {
        name: "Acme AI".to_owned(),
        logo_url: Some("https://cdn.example.com/logo.svg".to_owned()),
        accent_color: Some("#2d5bd8".to_owned()),
    });
    let app = app(config);
    let (status, body) = get(&app, "/api/v1/branding").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["name"], "Acme AI");
    assert_eq!(body["data"]["logoUrl"], "https://cdn.example.com/logo.svg");
    assert_eq!(body["data"]["accentColor"], "#2d5bd8");
}
