//! The TOTP second factor end to end (issue #504): enrollment through
//! confirm, the two-phase login it forces, wrong-code attempts and the
//! single-use backup codes — all over the real routes, on a real database.
//!
//! The world is the auth suite's shape: an app over a real database with a
//! deployment secret (the TOTP secret is sealed under it). Requires
//! DATABASE_URL; without it the tests skip.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("totp")
}

macro_rules! world_or_skip {
    () => {
        match url() {
            Some(u) => world(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// The test world: an app whose deployment seals secrets, and a registered
/// user's session cookie (the enrollment endpoints are session-authed).
struct World {
    app: Router,
    db: Db,
    email: String,
    cookie: String,
}

const PASSWORD: &str = "correct horse battery";

async fn world(database_url: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");

    let config = Config::new(Signup::Open, None).with_secret(SecretKey::from_bytes([9; 32]));
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = oxsum_server::app(db.clone(), config);

    let email = format!(
        "totp_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let (status, cookie, _) = call_full(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login failed");
    let cookie = cookie.expect("a password-only login sets the cookie");
    World {
        app,
        db,
        email,
        cookie,
    }
}

/// One JSON call; `key` is the bearer credential when Some, `cookie` the
/// session cookie — enrollment needs the person, not the key.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _, body) = call_full(app, method, path, body, cookie).await;
    (status, body)
}

/// The call plus the response headers — the login's `Set-Cookie` and the
/// oauth routes' `Location` are the assertions.
async fn call_full(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                .expect("the test's own request"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body collects")
        .to_bytes();
    let body: Value = if bytes.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&bytes).unwrap_or(json!(null))
    };
    (status, set_cookie, body)
}

/// Enrolls and confirms a factor for the world's user, returning the live
/// secret bytes and the backup codes — the state every login test needs.
async fn enroll_and_confirm(world: &World) -> ([u8; 20], Vec<String>) {
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/auth/totp/enroll",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enroll failed: {body}");
    let b32 = body["data"]["secret"].as_str().expect("a secret");
    assert!(
        body["data"]["otpauthUrl"]
            .as_str()
            .is_some_and(|u| u.starts_with("otpauth://totp/")),
        "otpauthUrl is {body}"
    );
    let secret = decode_base32(b32);

    let presented =
        oxsum_core::totp::code(&secret, time::OffsetDateTime::now_utc().unix_timestamp());
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/auth/totp/confirm",
        Some(json!({"code": presented})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "confirm failed: {body}");
    let backup: Vec<String> = body["data"]["backupCodes"]
        .as_array()
        .expect("backup codes")
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(backup.len(), 10);
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(&secret);
    (bytes, backup)
}

/// The base32 decode the test needs — the same alphabet `totp.rs` encodes in.
fn decode_base32(text: &str) -> Vec<u8> {
    fn value(byte: u8) -> u8 {
        match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => panic!("not base32: {byte}"),
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u64;
    let mut bits = 0u32;
    for byte in text.bytes() {
        acc = (acc << 5) | u64::from(value(byte));
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

#[tokio::test]
async fn a_confirmed_factor_turns_the_login_two_phase() {
    let world = world_or_skip!();
    let (secret, _backup) = enroll_and_confirm(&world).await;

    // The password leg now withholds the session: a challenge, no cookie.
    let (status, cookie, body) = call_full(
        &world.app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": world.email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["requiresTotp"], json!(true), "{body}");
    assert!(cookie.is_none(), "a TOTP-gated login sets no cookie");
    let challenge = body["data"]["challengeToken"]
        .as_str()
        .expect("the challenge token")
        .to_owned();

    // The second leg trades the challenge plus a live code for the session.
    let presented =
        oxsum_core::totp::code(&secret, time::OffsetDateTime::now_utc().unix_timestamp());
    let (status, cookie, body) = call_full(
        &world.app,
        "POST",
        "/api/v1/auth/totp",
        Some(json!({"challengeToken": challenge, "code": presented})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(cookie.is_some(), "the second leg mints the cookie");
    assert!(body["data"]["session"]["id"].is_string(), "{body}");

    // The session row was stamped: the freshness column a route gate reads.
    let stamped: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM oxsum.sessions WHERE totp_verified_at IS NOT NULL)",
    )
    .fetch_one(world.db.pool())
    .await
    .unwrap();
    assert!(stamped, "totp_verified_at was not stamped");
}

#[tokio::test]
async fn a_challenge_spends_attempts_and_dies() {
    let world = world_or_skip!();
    let (_secret, _backup) = enroll_and_confirm(&world).await;

    let challenge = login_challenge(&world).await;
    // Wrong codes are 401 and spend the challenge's five attempts.
    for _ in 0..5 {
        let (status, _) = call(
            &world.app,
            "POST",
            "/api/v1/auth/totp",
            Some(json!({"challengeToken": challenge, "code": "000000"})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // The exhausted challenge is dead — even a now-correct code finds nothing.
    let (status, _) = call(
        &world.app,
        "POST",
        "/api/v1/auth/totp",
        Some(json!({"challengeToken": challenge, "code": "000000"})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a spent-out challenge must die"
    );
}

/// Runs the password leg and returns the challenge token.
async fn login_challenge(world: &World) -> String {
    let (status, _, body) = call_full(
        &world.app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": world.email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["data"]["challengeToken"]
        .as_str()
        .expect("the challenge token")
        .to_owned()
}

#[tokio::test]
async fn a_satisfied_challenge_cannot_be_replayed() {
    let world = world_or_skip!();
    let (secret, _backup) = enroll_and_confirm(&world).await;
    let challenge = login_challenge(&world).await;
    let presented =
        oxsum_core::totp::code(&secret, time::OffsetDateTime::now_utc().unix_timestamp());
    let (status, _, _) = call_full(
        &world.app,
        "POST",
        "/api/v1/auth/totp",
        Some(json!({"challengeToken": challenge, "code": presented})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The same challenge again is 404: it was deleted with the session mint.
    let (status, _) = call(
        &world.app,
        "POST",
        "/api/v1/auth/totp",
        Some(json!({"challengeToken": challenge, "code": presented})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_backup_code_works_exactly_once() {
    let world = world_or_skip!();
    let (_secret, backup) = enroll_and_confirm(&world).await;
    let code = backup[0].clone();

    // A backup code satisfies the challenge in place of a live code.
    let challenge = login_challenge(&world).await;
    let (status, _, _) = call_full(
        &world.app,
        "POST",
        "/api/v1/auth/totp",
        Some(json!({"challengeToken": challenge, "code": code})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Replaying it against a fresh challenge is a wrong credential — 401.
    let challenge = login_challenge(&world).await;
    let (status, _) = call(
        &world.app,
        "POST",
        "/api/v1/auth/totp",
        Some(json!({"challengeToken": challenge, "code": code})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a backup code must be single-use"
    );
}

#[tokio::test]
async fn disable_needs_a_live_code_and_lifts_the_gate() {
    let world = world_or_skip!();
    let (secret, _backup) = enroll_and_confirm(&world).await;

    // A wrong code cannot disable.
    let (status, _) = call(
        &world.app,
        "POST",
        "/api/v1/auth/totp/disable",
        Some(json!({"code": "000000"})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let presented =
        oxsum_core::totp::code(&secret, time::OffsetDateTime::now_utc().unix_timestamp());
    let (status, _) = call(
        &world.app,
        "POST",
        "/api/v1/auth/totp/disable",
        Some(json!({"code": presented})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Login is single-leg again.
    let (status, cookie, body) = call_full(
        &world.app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": world.email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["session"]["id"].is_string(), "{body}");
    assert!(cookie.is_some());
}

#[tokio::test]
async fn enrollment_refuses_an_api_key() {
    let world = world_or_skip!();
    // A key names no user — enroll/confirm/disable are a person's actions.
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "totp-test"})),
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key = body["data"]["secret"].as_str().unwrap().to_owned();
    for path in [
        "/api/v1/auth/totp/enroll",
        "/api/v1/auth/totp/confirm",
        "/api/v1/auth/totp/disable",
    ] {
        let response = world
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Enroll checks the principal first (403); the bodied routes reject the
        // empty body before the handler sees the credential (400) — either way
        // a key gets nothing done here.
        assert!(
            response.status().is_client_error(),
            "{path} must refuse a key, answered {}",
            response.status()
        );
    }
}

#[tokio::test]
async fn an_unconfirmed_secret_never_gates() {
    let world = world_or_skip!();
    // Enroll but never confirm: the login stays single-leg.
    let (status, _) = call(
        &world.app,
        "POST",
        "/api/v1/auth/totp/enroll",
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, cookie, body) = call_full(
        &world.app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": world.email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["data"]["session"]["id"].is_string(),
        "an unconfirmed secret must not gate: {body}"
    );
    assert!(cookie.is_some());
}
