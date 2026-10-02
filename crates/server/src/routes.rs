use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router, middleware};
use oxsum_core::{ApiKey, CreatedApiKey, NewUser, Organization, Registration, Tenants, Wallet};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::require_key;
use crate::error::ApiError;
use crate::{AppState, Signup, today};

/// The `/api/v1` surface, plus the two routes served outside it: health, and the gateway.
pub fn router(state: AppState) -> Router {
    // Behind a key: everything that touches an organization, its ledger or its credentials.
    let authenticated = Router::new()
        .route("/org", get(organization))
        .route("/org/keys", get(list_keys).post(create_key))
        .route("/org/keys/{key_id}", delete(revoke_key))
        .route("/topups", post(top_up))
        .route("/holds", post(hold))
        .route("/settlements", post(settle))
        .route("/balance", get(balance))
        .route("/entries/{entry_id}/proof", get(proof))
        .layer(middleware::from_fn_with_state(state.clone(), require_key));

    // No key: health, and signup while the deployment allows it.
    let open = Router::new().route("/auth/register", post(register));

    // The platform admin: an operator token, not an organization key, and its own middleware.
    let admin = crate::admin::router(state.clone());

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", open.merge(authenticated).nest("/admin", admin))
        // The OpenAI-compatible surface, which brings its own auth and its own error format.
        .nest("/v1", crate::gateway::router(state.clone()))
        .with_state(state)
}

/// The uniform success envelope: { "data": ... }.
#[derive(Serialize)]
pub(crate) struct Data<T> {
    data: T,
}

pub(crate) type ApiResult<T> = Result<Json<Data<T>>, ApiError>;

pub(crate) fn ok<T>(data: T) -> ApiResult<T> {
    Ok(Json(Data { data }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterReq {
    email: String,
    password: String,
    organization_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateKeyReq {
    name: Option<String>,
    /// RFC 3339; absent means a key that never expires.
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
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

/// The ledger facade of the organization the credential belongs to.
async fn wallet(tenants: &Tenants, organization: &Organization) -> Result<Arc<Wallet>, ApiError> {
    Ok(tenants.get(&organization.tenant_id).await?)
}

async fn register(
    State(state): State<AppState>,
    Json(r): Json<RegisterReq>,
) -> ApiResult<Registration> {
    if state.config.signup() != Signup::Open {
        return Err(ApiError::Forbidden(
            "this deployment registers by invitation only".into(),
        ));
    }
    ok(state
        .db
        .register(NewUser {
            email: r.email,
            password: r.password,
            organization_name: r.organization_name,
        })
        .await?)
}

async fn organization(Extension(organization): Extension<Organization>) -> ApiResult<Organization> {
    ok(organization)
}

async fn create_key(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    body: Option<Json<CreateKeyReq>>,
) -> ApiResult<CreatedApiKey> {
    let (name, expires_at) = body.map_or((None, None), |Json(r)| (r.name, r.expires_at));
    // `created_by` stays empty: a key is not a person and no user acts through it yet, so
    // product.md's per-member rules attach when sessions make a user the acting principal.
    ok(state
        .db
        .create_key(organization.id, name, expires_at, None)
        .await?)
}

async fn list_keys(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
) -> ApiResult<Vec<ApiKey>> {
    ok(state.db.list_keys(organization.id).await?)
}

async fn revoke_key(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Path(key_id): Path<Uuid>,
) -> ApiResult<ApiKey> {
    // A key id of another organization is not found, not forbidden: an id should not be
    // probeable for existence.
    match state.db.revoke_key(organization.id, key_id).await? {
        Some(key) => ok(key),
        None => Err(ApiError::NotFound),
    }
}

async fn top_up(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Json(r): Json<AmountReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, &organization).await?;
    ok(w.top_up(&r.idempotency_key, r.amount_minor, today())
        .await?)
}

async fn hold(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Json(r): Json<AmountReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, &organization).await?;
    // No description: this endpoint takes an amount, not a reason. The gateway, which knows what the
    // hold is for, records one.
    ok(w.hold(&r.idempotency_key, "", r.amount_minor, today())
        .await?)
}

async fn settle(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Json(r): Json<SettleReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, &organization).await?;
    ok(w.settle(
        &r.idempotency_key,
        "",
        r.held_minor,
        r.actual_minor,
        today(),
    )
    .await?)
}

async fn balance(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
) -> ApiResult<BalanceRes> {
    let w = wallet(&state.tenants, &organization).await?;
    ok(BalanceRes {
        available_minor: w.available().await?,
    })
}

async fn proof(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Path(entry_id): Path<Uuid>,
) -> ApiResult<oxsum_core::ProofBundle> {
    let w = wallet(&state.tenants, &organization).await?;
    match w
        .receipt_proof(oxsum_core::EntryId::from_uuid(entry_id))
        .await?
    {
        Some(bundle) => ok(bundle),
        None => Err(ApiError::NotFound),
    }
}
