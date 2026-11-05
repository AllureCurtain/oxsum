//! Explicit test-only funding fixture. Production routers never mount this path.
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
