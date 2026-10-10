//! The whole bill path against a real provider: register → top-up → model list →
//! non-streaming and streaming completions → settlement ≤ freeze → billing record →
//! proof bundle verified by `oxsum_verify`, the same functions the browser runs.
//!
//! Opt-in and network-bound: every test is `#[ignore]`d, and even then returns
//! early unless `OXSUM_E2E_UPSTREAM_BASE_URL`, `OXSUM_E2E_UPSTREAM_API_KEY` and
//! `OXSUM_E2E_MODEL` are all set — plain `cargo test` and CI never touch the
//! network. Run it with:
//!
//! ```bash
//! OXSUM_E2E_UPSTREAM_BASE_URL=https://api.stepfun.com/step_plan/v1 \
//! OXSUM_E2E_UPSTREAM_API_KEY=sk-... \
//! OXSUM_E2E_MODEL=step-5-preview \
//! DATABASE_URL=postgres://... cargo test -p oxsum-server --test real_upstream -- --ignored
//! ```
//!
//! The provider key lives in the environment only — nothing here is written to a
//! tracked file. Like the other server suites the test also needs DATABASE_URL and
//! skips without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt;
use oxsum_core::{Db, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
/// Minor units per million tokens — the deployment's own price, not the
/// provider's: what oxsum bills is what it was configured to bill.
const INPUT_PRICE: i64 = 1_000_000;
const OUTPUT_PRICE: i64 = 4_000_000;
const MAX_OUTPUT: i64 = 8_192;

/// The deployment the env describes, or an early return when the test is not
/// opted in. `#[ignore]` already keeps it out of normal runs; the env check is
/// the second half: `-- --ignored` alone must not reach the network either.
macro_rules! world {
    () => {
        match env_world() {
            Some(Ok(world)) => world,
            Some(Err(why)) => {
                eprintln!("real-upstream E2E misconfigured, skipping: {why}");
                return;
            }
            None => {
                eprintln!(
                    "OXSUM_E2E_UPSTREAM_BASE_URL / _API_KEY / OXSUM_E2E_MODEL unset, skipping"
                );
                return;
            }
        }
    };
}

fn env_world() -> Option<Result<World, String>> {
    let _ = dotenvy::dotenv();
    let base_url = std::env::var("OXSUM_E2E_UPSTREAM_BASE_URL").ok()?;
    let api_key = std::env::var("OXSUM_E2E_UPSTREAM_API_KEY").ok()?;
    let model = std::env::var("OXSUM_E2E_MODEL").ok()?;
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set".to_owned());
    Some(url.map(|url| World {
        base_url,
        api_key,
        model,
        url,
    }))
}

struct World {
    base_url: String,
    api_key: String,
    model: String,
    url: String,
}

struct Fixture {
    app: Router,
    tenants: Tenants,
    tenant_id: String,
    key: String,
}

async fn fixture(world: &World) -> Fixture {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&world.url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");

    // A channel name unique to this run, pointing at the real provider. Prices are
    // the deployment's choice; the model name is the env's.
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("e2e-{suffix}");
    let secret = oxsum_core::SecretKey::from_bytes([9; 32]);
    db.set_channel(&channel, &world.base_url, &world.api_key, "openai", &secret)
        .await
        .expect("the channel is written");
    let book = oxsum_core::PriceBook::from_json(
        &channel,
        &json!({
            &world.model: {
                "inputPricePerMillion": INPUT_PRICE,
                "outputPricePerMillion": OUTPUT_PRICE,
                "maxOutputTokens": MAX_OUTPUT,
            }
        })
        .to_string(),
    )
    .expect("prices parse");
    for (model, price) in book.models() {
        db.append_price(&channel, model, price.clone(), 100)
            .await
            .expect("the price is written");
    }

    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_head_signing_seed([5; 32]);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, _billing, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    let tenants = Tenants::new(pool);

    let email = format!("e2e_{}@example.com", suffix);
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let key = body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    let tenant_id = body["data"]["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();

    Fixture {
        app,
        tenants,
        tenant_id,
        key,
    }
}

/// One JSON HTTP call against the app, authenticated with the API key.
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
        .expect("the test's own request");
    let res = app.clone().oneshot(req).await.expect("the router answers");
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// One chat completion, returning the response headers and the raw body — the
/// body's shape differs between streaming and not.
async fn chat(fixture: &Fixture, body: Value) -> (axum::http::HeaderMap, String) {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", fixture.key))
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    let res = fixture
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    assert_eq!(res.status(), StatusCode::OK, "the turn answered");
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (headers, String::from_utf8(bytes.to_vec()).expect("text"))
}

fn header_i64(headers: &axum::http::HeaderMap, name: &str) -> i64 {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.parse().ok())
        .unwrap_or_else(|| panic!("the response carries {name}"))
}

/// The settlement a turn wrote, found through the billing-records read the user
/// gets — not by reaching into the ledger.
async fn billing_row(fixture: &Fixture, request_id: &str) -> Value {
    let (status, body) = call(
        &fixture.app,
        "GET",
        "/api/v1/billing-records",
        None,
        Some(&fixture.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["data"]["rows"]
        .as_array()
        .expect("the rows")
        .iter()
        .find(|row| row["requestId"].as_str() == Some(request_id))
        .cloned()
        .unwrap_or_else(|| panic!("the turn {request_id} settled into a record"))
}

/// Register, fund, call the real model both ways, and prove the bill end to end.
#[tokio::test]
#[ignore = "reaches a real provider; needs OXSUM_E2E_UPSTREAM_* and DATABASE_URL"]
async fn real_provider_bill_verifies() {
    let world = world!();
    let fixture = fixture(&world).await;

    // Fund the wallet through the manual rail — the only rail a deployment has
    // without a payment integration.
    let (status, body) = call(
        &fixture.app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "e2e-topup", "amountMinor": 10_000_000})),
        Some(&fixture.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the top-up lands: {body}");
    let topup_entry = body["data"]["entryId"].as_str().unwrap().to_owned();
    let topup_hash = body["data"]["contentHash"].as_str().unwrap().to_owned();

    // The model the deployment serves answers on the catalog.
    let (status, body) = call(&fixture.app, "GET", "/v1/models", None, Some(&fixture.key)).await;
    assert_eq!(status, StatusCode::OK, "the catalog answers: {body}");
    assert!(
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"].as_str() == Some(world.model.as_str())),
        "the real model is listed: {body}"
    );

    // A real non-streaming turn. max_tokens stays tiny — the provider's own
    // usage report settles the turn.
    let (headers, body) = chat(
        &fixture,
        json!({
            "model": world.model,
            "messages": [{"role": "user", "content": "Reply with exactly: OK"}],
            "max_tokens": 32,
            "stream": false,
        }),
    )
    .await;
    let freeze = header_i64(&headers, "x-oxsum-freeze-minor");
    let charged = header_i64(&headers, "x-oxsum-charged-minor");
    let request_id = headers["x-oxsum-request-id"].to_str().unwrap().to_owned();
    assert!(
        freeze > 0 && charged <= freeze,
        "charged {charged} ≤ freeze {freeze}"
    );
    let answer: Value = serde_json::from_str(&body).expect("a JSON completion");
    assert_eq!(answer["object"], "chat.completion");
    assert!(
        answer["usage"]["prompt_tokens"].as_i64().unwrap_or(0) > 0,
        "upstream reported usage: {body}"
    );

    // The settlement the user can read back.
    let row = billing_row(&fixture, &request_id).await;
    assert_eq!(row["kind"], "usage", "upstream reported usage: {row}");
    assert_eq!(
        row["chargedMinor"], charged,
        "the record matches the headers"
    );
    assert_eq!(row["upstreamAttempts"], 1);

    // A real streaming turn: SSE frames, a [DONE], and the same cap.
    let (headers, body) = chat(
        &fixture,
        json!({
            "model": world.model,
            "messages": [{"role": "user", "content": "Say hi"}],
            "max_tokens": 32,
            "stream": true,
        }),
    )
    .await;
    let stream_freeze = header_i64(&headers, "x-oxsum-freeze-minor");
    let stream_request_id = headers["x-oxsum-request-id"].to_str().unwrap().to_owned();
    assert!(
        body.contains("data: [DONE]"),
        "the stream terminated: {body}"
    );
    let row = billing_row(&fixture, &stream_request_id).await;
    assert_eq!(row["kind"], "usage", "the provider reported usage: {row}");
    assert!(
        row["chargedMinor"].as_i64().unwrap() <= stream_freeze,
        "streamed charge {} ≤ freeze {stream_freeze}",
        row["chargedMinor"]
    );

    // The bill proves itself: inclusion in the log, the recorded content hash,
    // and the charge recomputed from the versioned description — the three
    // checks the browser runs.
    let wallet = fixture
        .tenants
        .get(&fixture.tenant_id)
        .await
        .expect("the organization's ledger opens");
    let settlement_entry = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!(
        "req-{request_id}:hold"
    )));
    for (label, entry_id) in [
        (
            "topup",
            oxsum_core::EntryId::from_uuid(uuid::Uuid::parse_str(&topup_entry).expect("a uuid")),
        ),
        ("settlement", settlement_entry),
    ] {
        let (status, body) = call(
            &fixture.app,
            "GET",
            &format!("/api/v1/entries/{entry_id}/proof"),
            None,
            Some(&fixture.key),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the {label} proof serves: {body}");
        let bundle_json = serde_json::to_string(&body["data"]).unwrap();
        let bundle = wallet
            .receipt_proof(entry_id)
            .await
            .expect("the proof builds")
            .expect("the entry exists");
        let recorded = if label == "topup" {
            oxsum_core::Hash::parse_hex(&topup_hash).unwrap()
        } else {
            bundle.entry.content_hash()
        };
        assert!(
            oxsum_core::verify_bundle(&bundle_json, &recorded).expect("the bundle parses"),
            "the {label} bundle verifies"
        );
        match label {
            "settlement" => assert_eq!(
                oxsum_core::verify_charge(body["data"]["entry"]["description"].as_str().unwrap()),
                oxsum_core::ChargeCheck::Recomputed,
                "the charge recomputes"
            ),
            _ => assert_eq!(
                oxsum_core::verify_charge(
                    body["data"]["entry"]["description"].as_str().unwrap_or("")
                ),
                oxsum_core::ChargeCheck::NotASettlement,
            ),
        }
    }

    // The operator-signed tree head, checked the way the browser checks it.
    let (status, body) = call(
        &fixture.app,
        "GET",
        "/api/v1/log/head",
        None,
        Some(&fixture.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the signed head serves: {body}");
    let served = oxsum_core::TreeHead {
        size: body["data"]["size"].as_u64().unwrap(),
        root: oxsum_core::Hash::parse_hex(body["data"]["root"].as_str().unwrap()).unwrap(),
    };
    let raw_key = base64::engine::general_purpose::STANDARD
        .decode(body["data"]["publicKey"].as_str().unwrap())
        .unwrap();
    let public_hex = hex(raw_key);
    let key = oxsum_verify::PublishedKey {
        key_name: body["data"]["keyName"].as_str().unwrap(),
        public_key: &public_hex,
        key_hash: body["data"]["keyHash"].as_str().unwrap(),
    };
    oxsum_verify::verify_signed_head(
        body["data"]["note"].as_str().unwrap(),
        &served,
        body["data"]["origin"].as_str().unwrap(),
        &key,
    )
    .expect("the operator's signature on the head verifies");
}

/// Hex-encodes bytes for `PublishedKey::public_key`, whose field is hex.
fn hex(bytes: Vec<u8>) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
