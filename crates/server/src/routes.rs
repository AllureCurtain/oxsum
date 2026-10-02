use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use oxsum_core::{Tenants, Wallet};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::auth::{ApiToken, require_token};
use crate::error::ApiError;

pub fn router(tenants: Tenants, token: ApiToken) -> Router {
    let api = Router::new()
        .route("/tenants/{tenant}/topups", post(top_up))
        .route("/tenants/{tenant}/holds", post(hold))
        .route("/tenants/{tenant}/settlements", post(settle))
        .route("/tenants/{tenant}/balance", get(balance))
        .route("/tenants/{tenant}/entries/{entry_id}/proof", get(proof))
        .with_state(tenants)
        .layer(middleware::from_fn_with_state(token, require_token));

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", api)
}

/// The uniform success envelope: { "data": ... }.
#[derive(Serialize)]
struct Data<T> {
    data: T,
}

type ApiResult<T> = Result<Json<Data<T>>, ApiError>;

fn ok<T>(data: T) -> ApiResult<T> {
    Ok(Json(Data { data }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AmountReq {
    idempotency_key: String,
    amount_minor: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettleReq {
    idempotency_key: String,
    held_minor: i64,
    actual_minor: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BalanceRes {
    available_minor: i64,
}

async fn wallet(tenants: &Tenants, tenant: &str) -> Result<Arc<Wallet>, ApiError> {
    Ok(tenants.get(tenant).await?)
}

/// The posting date is the server's current UTC date.
fn today() -> time::Date {
    OffsetDateTime::now_utc().date()
}

async fn top_up(
    State(t): State<Tenants>,
    Path(tenant): Path<String>,
    Json(r): Json<AmountReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&t, &tenant).await?;
    ok(w.top_up(&r.idempotency_key, r.amount_minor, today())
        .await?)
}

async fn hold(
    State(t): State<Tenants>,
    Path(tenant): Path<String>,
    Json(r): Json<AmountReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&t, &tenant).await?;
    ok(w.hold(&r.idempotency_key, r.amount_minor, today()).await?)
}

async fn settle(
    State(t): State<Tenants>,
    Path(tenant): Path<String>,
    Json(r): Json<SettleReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&t, &tenant).await?;
    ok(
        w.settle(&r.idempotency_key, r.held_minor, r.actual_minor, today())
            .await?,
    )
}

async fn balance(State(t): State<Tenants>, Path(tenant): Path<String>) -> ApiResult<BalanceRes> {
    let w = wallet(&t, &tenant).await?;
    ok(BalanceRes {
        available_minor: w.available().await?,
    })
}

async fn proof(
    State(t): State<Tenants>,
    Path((tenant, entry_id)): Path<(String, uuid::Uuid)>,
) -> ApiResult<oxsum_core::ProofBundle> {
    let w = wallet(&t, &tenant).await?;
    match w
        .receipt_proof(oxsum_core::EntryId::from_uuid(entry_id))
        .await?
    {
        Some(bundle) => ok(bundle),
        None => Err(ApiError::NotFound),
    }
}
