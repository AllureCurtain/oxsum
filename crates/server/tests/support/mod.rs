//! Explicit test-only ledger fixtures. Production routers never mount these paths.
//! Existing gateway/read tests need purchased funding, not the removed customer
//! ability to declare money received. Financial authority tests use the real
//! production router and the authorized manual-receipt API instead.
#![allow(dead_code)]

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use oxsum_core::{Db, Tenants, WalletError};
use oxsum_server::{BillingEvent, Config, Metrics};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

pub fn app(db: Db, config: Config) -> Router {
    let router = oxsum_server::app(db.clone(), config);
    with_funding(router, db)
}

pub fn app_with_billing(
    db: Db,
    config: Config,
) -> (Router, broadcast::Sender<BillingEvent>, Metrics) {
    let (router, billing, metrics) = oxsum_server::app_with_billing(db.clone(), config);
    (with_funding(router, db), billing, metrics)
}

fn with_funding(router: Router, db: Db) -> Router {
    router.merge(
        Router::new()
            .route("/__test/funding", post(fund))
            .route("/__test/holds", post(fixture_hold))
            .route("/__test/settlements", post(fixture_settle))
            .with_state(db),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Funding {
    idempotency_key: String,
    amount_minor: i64,
}

async fn fund(
    State(db): State<Db>,
    headers: HeaderMap,
    Json(body): Json<Funding>,
) -> (StatusCode, Json<Value>) {
    match fund_inner(db, headers, body).await {
        Ok(receipt) => (StatusCode::OK, Json(json!({"data": receipt}))),
        Err(error) => {
            let (status, code) = match &error {
                WalletError::Conflict(_) => (StatusCode::CONFLICT, "CONFLICT"),
                WalletError::InvalidInput(_) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
                WalletError::Forbidden(_) => (StatusCode::FORBIDDEN, "FORBIDDEN"),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
            };
            (
                status,
                Json(json!({"error": {"code": code, "message": error.to_string()}})),
            )
        }
    }
}

async fn fund_inner(
    db: Db,
    headers: HeaderMap,
    body: Funding,
) -> Result<oxsum_core::Receipt, WalletError> {
    // The credential only identifies the fixture's target customer; this route
    // is intentionally test-only and is not an authorization example.
    let org = if let Some(secret) = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        db.authenticate(secret).await?.map(|(org, _)| org)
    } else if let Some(secret) = headers
        .get("cookie")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("oxsum_session="))
    {
        db.authenticate_session(secret)
            .await?
            .map(|p| p.organization)
    } else {
        None
    }
    .ok_or_else(|| WalletError::Forbidden("fixture customer is unknown".into()))?;
    let wallet = Tenants::new(db.pool().clone()).get(&org.tenant_id).await?;
    let today = time::OffsetDateTime::now_utc().date();
    let receipt = wallet
        .top_up(&body.idempotency_key, body.amount_minor, today)
        .await?;
    db.record_manual_deposit(
        org.id,
        &body.idempotency_key,
        body.amount_minor,
        *receipt.entry_id.as_uuid(),
        wallet.available().await.ok(),
    )
    .await?;
    db.reconcile_statements(org.id, &wallet, today).await?;
    Ok(receipt)
}

/// A real loopback provider and flat-priced model for admission tests. Calls go
/// through the production gateway; the helper does not implement authorization.
pub async fn flat_model(db: &Db, secret: &oxsum_core::SecretKey, minor: i64) -> String {
    let upstream = Router::new().route("/v1/chat/completions", post(|| async {
        Json(json!({"id":"fixture", "object":"chat.completion", "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let name = format!("fixture-{}", uuid::Uuid::new_v4().simple());
    db.set_channel_pool_policy(
        &name,
        &base,
        Some(&["fixture-secret".to_owned()]),
        "openai",
        secret,
        Some(&oxsum_core::ProviderDestinationPolicy {
            network: oxsum_core::ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }),
    )
    .await
    .unwrap();
    let prices = oxsum_core::PriceBook::from_json(&name, &json!({name.clone(): {"inputPricePerMillion":0,"outputPricePerMillion":0,"costPerRequest":minor,"maxOutputTokens":1}}).to_string()).unwrap();
    for (model, price) in prices.models() {
        db.append_price(&name, model, price.clone(), 100)
            .await
            .unwrap();
    }
    name
}

pub fn chat_body(model: &str) -> Value {
    json!({"model":model,"messages":[{"role":"user","content":"hello"}],"max_tokens":1,"stream":false})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settlement {
    hold_key: String,
    actual_minor: i64,
}

/// Historical read/sweeper tests may seed ledger rows explicitly. These routes
/// are test fixtures, never a model of production billing authorization.
async fn fixture_hold(
    State(db): State<Db>,
    headers: HeaderMap,
    Json(body): Json<Funding>,
) -> (StatusCode, Json<Value>) {
    fixture_response(
        async {
            let principal = fixture_principal(&db, &headers).await?;
            let wallet = Tenants::new(db.pool().clone())
                .get(&principal.organization().tenant_id)
                .await?;
            let day = time::OffsetDateTime::now_utc().date();
            let receipt = match principal.acting_key() {
                Some(key) => {
                    wallet
                        .hold_for_key(
                            key,
                            None,
                            &body.idempotency_key,
                            "",
                            body.amount_minor,
                            day,
                            None,
                        )
                        .await?
                }
                None => {
                    wallet
                        .hold(&body.idempotency_key, "", body.amount_minor, day)
                        .await?
                }
            };
            db.note_open_hold(&oxsum_core::OpenHold {
                hold_key: body.idempotency_key,
                tenant_id: principal.organization().tenant_id.clone(),
                request_id: String::new(),
                model: String::new(),
                channel: String::new(),
                price_version: 0,
                input_price: 0,
                output_price: 0,
                freeze_minor: body.amount_minor,
                key_id: principal.acting_key().map(|k| k.key_id),
                mandate_id: None,
                end_user: None,
                service_tier: None,
                tags: Default::default(),
                sweep_attempts: 0,
                last_error: None,
                dead_at: None,
            })
            .await?;
            Ok(receipt)
        }
        .await,
    )
}

async fn fixture_settle(
    State(db): State<Db>,
    headers: HeaderMap,
    Json(body): Json<Settlement>,
) -> (StatusCode, Json<Value>) {
    fixture_response(
        async {
            let principal = fixture_principal(&db, &headers).await?;
            let wallet = Tenants::new(db.pool().clone())
                .get(&principal.organization().tenant_id)
                .await?;
            let receipt = wallet
                .settle(
                    &body.hold_key,
                    "",
                    body.actual_minor,
                    time::OffsetDateTime::now_utc().date(),
                )
                .await?;
            db.clear_open_hold(&body.hold_key).await?;
            Ok(receipt)
        }
        .await,
    )
}

async fn fixture_principal(
    db: &Db,
    headers: &HeaderMap,
) -> Result<oxsum_core::Principal, WalletError> {
    if let Some(token) = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    {
        return db
            .authenticate(token)
            .await?
            .map(|(organization, key)| {
                oxsum_core::Principal::Key(oxsum_core::KeyPrincipal { organization, key })
            })
            .ok_or_else(|| WalletError::Forbidden("fixture key unknown".into()));
    }
    let token = headers
        .get("cookie")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("oxsum_session="))
        .unwrap_or_default();
    db.authenticate_session(token)
        .await?
        .map(oxsum_core::Principal::Session)
        .ok_or_else(|| WalletError::Forbidden("fixture session unknown".into()))
}

fn fixture_response(result: Result<oxsum_core::Receipt, WalletError>) -> (StatusCode, Json<Value>) {
    match result {
        Ok(receipt) => (StatusCode::OK, Json(json!({"data":receipt}))),
        Err(error) => {
            let (status, code) = match error {
                WalletError::InsufficientFunds => {
                    (StatusCode::PAYMENT_REQUIRED, "INSUFFICIENT_FUNDS")
                }
                WalletError::Conflict(_) => (StatusCode::CONFLICT, "CONFLICT"),
                WalletError::HoldNotFound(_) | WalletError::NotFound(_) => {
                    (StatusCode::NOT_FOUND, "NOT_FOUND")
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
            };
            (
                status,
                Json(json!({"error":{"code":code,"message":error.to_string()}})),
            )
        }
    }
}

/// Attach the explicitly chosen credential. A consumption key is never upgraded
/// to a personal session; each fixture must log in when it needs account rights.
pub fn credential(
    request: axum::http::request::Builder,
    token: &str,
) -> axum::http::request::Builder {
    if token.starts_with("oxsess-") {
        request.header("cookie", format!("oxsum_session={token}"))
    } else {
        request.header("authorization", format!("Bearer {token}"))
    }
}

/// Obtain a real personal session through the production password login route.
pub async fn login(app: &Router, email: &str, password: &str) -> String {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"email":email,"password":password}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "fixture login failed");
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .unwrap()
        .to_owned()
}
