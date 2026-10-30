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
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
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

// --- The facilitator-delegated rail (issue #282) ---
//
// A scripted x402-rs facilitator stub stands in for the sidecar: `/verify`
// answers the validity it is told, `/settle` the transaction hash. The wire
// the verifier speaks is the V1 protocol — `{x402Version, paymentPayload,
// paymentRequirements}` camelCase in, `{isValid, payer}` and `{success,
// transaction, network, error_reason}` (snake_case on settle) out.

/// What a scripted leg answers: (outcome flag, payload, reason).
type ScriptedReply = (bool, Option<String>, Option<String>);

#[derive(Clone, Default)]
struct FacilitatorStub {
    /// What `/verify` answers: (isValid, payer, invalidReason).
    verify: Arc<Mutex<ScriptedReply>>,
    /// What `/settle` answers: (success, transaction, error_reason).
    settle: Arc<Mutex<ScriptedReply>>,
    /// The last paymentRequirements the verifier stated, for assertions.
    requirements: Arc<Mutex<Value>>,
    /// How many times `/settle` was called — the one-settle-per-nonce proof.
    settle_calls: Arc<Mutex<usize>>,
}

async fn stub_facilitator() -> (String, FacilitatorStub) {
    let stub = FacilitatorStub::default();
    let app = Router::new()
        .route(
            "/verify",
            post(
                |State(stub): State<FacilitatorStub>, body: String| async move {
                    let v: Value = serde_json::from_str(&body).unwrap();
                    *stub.requirements.lock().unwrap() = v["paymentRequirements"].clone();
                    let (valid, payer, reason) = stub.verify.lock().unwrap().clone();
                    axum::Json(json!({
                        "isValid": valid,
                        "payer": payer,
                        "invalidReason": reason,
                    }))
                },
            ),
        )
        .route(
            "/settle",
            post(
                |State(stub): State<FacilitatorStub>, _body: String| async move {
                    *stub.settle_calls.lock().unwrap() += 1;
                    let (success, tx, reason) = stub.settle.lock().unwrap().clone();
                    axum::Json(json!({
                        "success": success,
                        "transaction": tx,
                        "network": "base-sepolia",
                        "error_reason": reason,
                    }))
                },
            ),
        )
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), stub)
}

/// A deployment with the facilitator rail configured: the verifier delegates
/// to the stub sidecar on `base-sepolia`.
async fn facilitator_world() -> Option<(Router, Db, FacilitatorStub)> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let (base_url, stub) = stub_facilitator().await;
    *stub.verify.lock().unwrap() = (true, Some("0xPAYER".to_owned()), None);
    *stub.settle.lock().unwrap() = (true, Some("0xTXHASH".to_owned()), None);
    let config = Config::new(Signup::Open, None).with_x402(
        oxsum_server::x402_facilitator::X402FacilitatorVerifier::new(
            oxsum_server::facilitator::Facilitator::new(base_url),
            reqwest::Client::new(),
            "base-sepolia".to_owned(),
            "0x036CbD53842c5426634e7929541eC2318f3dCF7e".to_owned(),
            "0xRECEIVER".to_owned(),
        ),
    );
    Some((oxsum_server::app(db.clone(), config), db, stub))
}

/// A V1 `PaymentPayload` the way a wallet presents it — the `X-PAYMENT`
/// header's base64url decode. `nonce` doubles as the deposit's payment_ref.
fn x402_payload(nonce: &str, value: &str) -> String {
    BASE64URL.encode(format!(
        "{{\"x402Version\":1,\"scheme\":\"exact\",\"network\":\"base-sepolia\",\
         \"payload\":{{\"signature\":\"0xSIG\",\"authorization\":{{\
         \"from\":\"0xPAYER\",\"to\":\"0xRECEIVER\",\"value\":\"{value}\",\
         \"validAfter\":\"0\",\"validBefore\":\"9999999999\",\"nonce\":\"{nonce}\"}}}}}}"
    ))
}

/// A facilitator-verified payment settles on-chain and credits, its tx hash
/// landing on the deposit row.
#[tokio::test]
async fn a_facilitator_payment_settles_and_credits() {
    let Some((app, db, stub)) = facilitator_world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let payment = x402_payload("nonce-f1", "5000000");
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/x402",
        Some(json!({"payment": payment, "amountMinor": 5_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["isNew"].as_bool(), Some(true));
    assert_eq!(balance(&app, &key).await - before, 5_000_000);

    // The deployment's requirements reached the facilitator: the declared
    // amount, the configured network's payTo/asset.
    let req = stub.requirements.lock().unwrap().clone();
    assert_eq!(req["network"], "base-sepolia");
    assert_eq!(req["maxAmountRequired"], "5000000");
    assert_eq!(req["payTo"], "0xRECEIVER");
    assert_eq!(*stub.settle_calls.lock().unwrap(), 1);

    // The chain's transaction hash is on the deposit row for ops.
    let tx_hash: Option<String> = sqlx::query_scalar(
        "SELECT facilitator_tx_hash FROM oxsum.deposits WHERE payment_ref = 'nonce-f1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(tx_hash.as_deref(), Some("0xTXHASH"));
}

/// The same authorization presented twice credits once — and settles once:
/// the row lock makes the second call wait out the first's settle, see the
/// recorded hash, and skip the chain.
#[tokio::test]
async fn a_replayed_facilitator_payment_settles_once() {
    let Some((app, _db, stub)) = facilitator_world().await else {
        return;
    };
    let key = register(&app).await;
    let payment = x402_payload("nonce-f2", "3000000");
    for expected_new in [true, false] {
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
    assert_eq!(*stub.settle_calls.lock().unwrap(), 1);
}

/// A facilitator refusal to verify is the same 403 the stand-in's bad
/// signature earns — which leg failed is the rail's business.
#[tokio::test]
async fn a_facilitator_refusal_is_forbidden() {
    let Some((app, _db, stub)) = facilitator_world().await else {
        return;
    };
    *stub.verify.lock().unwrap() = (false, None, Some("invalid signature".to_owned()));
    let key = register(&app).await;
    let payment = x402_payload("nonce-f3", "2000000");
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/x402",
        Some(json!({"payment": payment})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(*stub.settle_calls.lock().unwrap(), 0);
}

/// A payment the chain cannot land — the facilitator's settle says
/// `success: false` — refuses the top-up and books nothing.
#[tokio::test]
async fn an_unsettled_payment_is_refused() {
    let Some((app, db, stub)) = facilitator_world().await else {
        return;
    };
    *stub.settle.lock().unwrap() = (false, None, Some("insufficient funds".to_owned()));
    let key = register(&app).await;
    let before = balance(&app, &key).await;
    let payment = x402_payload("nonce-f4", "2000000");
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
    // The refused settle leaves the deposit un-credited — a retry can land it —
    // with the facilitator's `error_reason` on `meta` for reconciliation
    // (issue #439): an operator reads *why* the rail refused without the
    // sidecar's logs.
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT status, meta->>'settleRefusal' FROM oxsum.deposits \
         WHERE payment_ref = 'nonce-f4'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "confirmed");
    assert_eq!(row.1.as_deref(), Some("insufficient funds"));
}

/// A payload on a foreign network or scheme is refused before the facilitator
/// is asked — the deployment's own gate runs first.
#[tokio::test]
async fn a_foreign_network_is_refused_before_the_facilitator() {
    let Some((app, _db, stub)) = facilitator_world().await else {
        return;
    };
    let key = register(&app).await;
    let payment = BASE64URL.encode(
        "{\"x402Version\":1,\"scheme\":\"exact\",\"network\":\"eip155:1\",\
         \"payload\":{\"signature\":\"0x\",\"authorization\":{\"from\":\"0xA\",\
         \"to\":\"0xB\",\"value\":\"1000\",\"validAfter\":\"0\",\"validBefore\":\"9\",\
         \"nonce\":\"n\"}}}",
    );
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/x402",
        Some(json!({"payment": payment})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(*stub.settle_calls.lock().unwrap(), 0);
}
