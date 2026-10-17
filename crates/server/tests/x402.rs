//! HTTP tests for the x402-shaped deposit rail (issue #264).
//!
//! `POST /api/v1/topups/x402` accepts a signed payment authorization
//! (`{scheme, network, asset, amountMinor, payTo, nonce, payer, signature}`,
//! base64url'd) and credits the amount through the same deposit bookkeeping
//! every rail shares. The verifier is the seam — a real EIP-712/x402 verifier
//! would drop into the same trait — while these tests exercise today's
//! stand-in Ed25519 test scheme: a signed authorization credits once, a
//! replayed one writes nothing twice, and a bad signature is refused.
//!
//! The tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The seed of the payer key this suite signs authorizations under; the rail
/// is configured to trust exactly its public half.
const PAYER_SEED: [u8; 32] = [23; 32];

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("x402")
}

/// A deployment with the x402 rail configured: the verifier pins the public
/// half of `PAYER_SEED`.
async fn world() -> Option<(Router, Db)> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let verifying = SigningKey::from_bytes(&PAYER_SEED).verifying_key();
    let config =
        Config::new(Signup::Open, None).with_x402(oxsum_core::Ed25519RailVerifier::new(verifying));
    Some((oxsum_server::app(db.clone(), config), db))
}

/// One plain HTTP call against the app; returns (status, json).
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A registered organization; returns its API key secret.
async fn register(app: &Router) -> String {
    let email = format!(
        "x402_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration answers an API key")
        .to_owned()
}

/// The org's ledger balance, read back through the API.
async fn balance(app: &Router, key: &str) -> i64 {
    let (status, body) = call(app, "GET", "/api/v1/balance", None, Some(key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["data"]["availableMinor"].as_i64().unwrap()
}

/// Builds a wire authorization so tests can push at the verifier's shape
/// checks as well as its signature check: the canonical body (unsigned fields
/// in declaration order) is Ed25519-signed under `signing`, the signature
/// appended as `signature`, and the whole JSON base64url'd.
#[allow(clippy::too_many_arguments)]
fn authorization(
    signing: &SigningKey,
    scheme: &str,
    network: &str,
    asset: &str,
    amount_minor: i64,
    pay_to: &str,
    nonce: &str,
    payer: &str,
) -> String {
    let body = format!(
        "{{\"scheme\":\"{scheme}\",\"network\":\"{network}\",\"asset\":\"{asset}\",\
         \"amountMinor\":{amount_minor},\"payTo\":\"{pay_to}\",\"nonce\":\"{nonce}\",\
         \"payer\":\"{payer}\"}}"
    );
    let sig = signing.sign(body.as_bytes());
    let wire = format!(
        "{},\"signature\":\"{}\"}}",
        &body[..body.len() - 1],
        BASE64URL.encode(sig.to_bytes())
    );
    BASE64URL.encode(wire)
}

/// A correctly signed authorization the way the payer presents it.
fn signed_authorization(
    signing: &SigningKey,
    amount_minor: i64,
    pay_to: &str,
    nonce: &str,
    payer: &str,
) -> String {
    authorization(
        signing,
        oxsum_core::SCHEME_EXACT,
        oxsum_core::NETWORK_ED25519,
        oxsum_core::ASSET_CREDIT,
        amount_minor,
        pay_to,
        nonce,
        payer,
    )
}

/// A correctly signed authorization credits its amount to the purchased pool.
#[tokio::test]
async fn a_signed_authorization_credits_the_pool() {
    let Some((app, _db)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let signing = SigningKey::from_bytes(&PAYER_SEED);
    let payment = signed_authorization(&signing, 5_000_000, "org-wallet", "nonce-1", "payer-7");
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/x402",
        Some(json!({"payment": payment})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["isNew"].as_bool(), Some(true));
    let after = balance(&app, &key).await;
    assert_eq!(after - before, 5_000_000);
}

/// The same authorization presented twice credits once: the signed nonce is
/// the deposit's idempotency anchor and the ledger credit replays.
#[tokio::test]
async fn a_replayed_authorization_credits_once() {
    let Some((app, _db)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let signing = SigningKey::from_bytes(&PAYER_SEED);
    let payment = signed_authorization(&signing, 3_000_000, "org-wallet", "nonce-2", "payer-7");
    for expected_new in [true, false, false] {
        let (status, body) = call(
            &app,
            "POST",
            "/api/v1/topups/x402",
            Some(json!({"payment": payment})),
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["isNew"].as_bool(),
            Some(expected_new),
            "{body}"
        );
    }
    let after = balance(&app, &key).await;
    assert_eq!(after - before, 3_000_000);
}

/// An authorization signed under a different key is refused: the signature
/// does not check against the pinned payer key.
#[tokio::test]
async fn a_wrong_signature_is_refused() {
    let Some((app, _db)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let impostor = SigningKey::from_bytes(&[9; 32]);
    let payment = signed_authorization(&impostor, 4_000_000, "org-wallet", "nonce-3", "payer-9");
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/x402",
        Some(json!({"payment": payment})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(balance(&app, &key).await, before);
}

/// A payload that is not a decodable authorization is refused the same way:
/// malformed is indistinguishable from unverified at the seam.
#[tokio::test]
async fn a_malformed_payload_is_refused() {
    let Some((app, _db)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/x402",
        Some(json!({"payment": "not-a-payment"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// An authorization for a scheme, network or asset the stand-in does not
/// serve is refused even when correctly signed — the shape check runs before
/// the signature counts.
#[tokio::test]
async fn a_foreign_scheme_network_or_asset_is_refused() {
    let Some((app, _db)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;
    let signing = SigningKey::from_bytes(&PAYER_SEED);

    for (scheme, network, asset) in [
        ("exact", "eip155:1", oxsum_core::ASSET_CREDIT),
        (
            "upto",
            oxsum_core::NETWORK_ED25519,
            oxsum_core::ASSET_CREDIT,
        ),
        ("exact", oxsum_core::NETWORK_ED25519, "USDC"),
    ] {
        let payment = authorization(
            &signing,
            scheme,
            network,
            asset,
            2_000_000,
            "org-wallet",
            "nonce-4",
            "payer-7",
        );
        let (status, _body) = call(
            &app,
            "POST",
            "/api/v1/topups/x402",
            Some(json!({"payment": payment})),
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{scheme}/{network}/{asset}");
    }
    assert_eq!(balance(&app, &key).await, before);
}

/// An unconfigured deployment answers 503: no pinned payer key, no rail.
#[tokio::test]
async fn an_unconfigured_deployment_refuses_cleanly() {
    let Some(u) = url() else {
        return;
    };
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let app = oxsum_server::app(db, Config::new(Signup::Open, None));
    let key = register(&app).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/x402",
        Some(json!({"payment": "whatever"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
