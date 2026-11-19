//! Exact-ID customer-safe charge receipt reads for customers and Business staff.
use axum::extract::{Extension, Path, State};
use serde::Deserialize;
use uuid::Uuid;

use oxsum_core::{ChargeReceipt, CustomerRecordScope, Principal, Wallet};

use crate::AppState;
use crate::error::{ApiError, ApiQuery};
use crate::routes::{ApiResult, ok};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    identifier: String,
}

pub(crate) async fn customer(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(query): ApiQuery<Query>,
) -> ApiResult<ChargeReceipt> {
    let scope = state.db.customer_record_scope(&principal).await?;
    read(
        &state,
        principal.organization().id,
        &principal.organization().tenant_id,
        &query.identifier,
        &scope,
    )
    .await
}

pub(crate) async fn business(
    State(state): State<AppState>,
    Extension(token): Extension<crate::auth::PersonalToken>,
    Path(id): Path<Uuid>,
    ApiQuery(query): ApiQuery<Query>,
) -> ApiResult<ChargeReceipt> {
    state.db.business_customer(&token, id).await?;
    let org = state.db.organization_by_id(id).await?;
    read(
        &state,
        id,
        &org.tenant_id,
        &query.identifier,
        &CustomerRecordScope {
            key_ids: None,
            include_unattributed: true,
        },
    )
    .await
}

async fn read(
    state: &AppState,
    id: Uuid,
    tenant: &str,
    identifier: &str,
    scope: &CustomerRecordScope,
) -> ApiResult<ChargeReceipt> {
    // A read must not bootstrap a new financial ledger.
    require_ledger(state, tenant).await?;
    let wallet = Wallet::open_existing(state.db.pool().clone(), tenant).await?;
    match wallet
        .charge_receipt(&state.db, id, identifier, scope)
        .await?
    {
        Some(receipt) => ok(receipt),
        None => Err(ApiError::NotFound("charge receipt not found".into())),
    }
}

pub(crate) async fn discover(
    State(state): State<AppState>,
    Extension(token): Extension<crate::auth::PersonalToken>,
    ApiQuery(query): ApiQuery<Query>,
) -> ApiResult<oxsum_core::BusinessChargeReceipt> {
    match state
        .db
        .business_charge_receipt(&token, &query.identifier)
        .await?
    {
        Some(found) => ok(found),
        None => Err(ApiError::not_found()),
    }
}

pub(crate) async fn proof(
    State(state): State<AppState>,
    Extension(token): Extension<crate::auth::PersonalToken>,
    Path(id): Path<Uuid>,
    ApiQuery(query): ApiQuery<Query>,
) -> ApiResult<oxsum_core::ProofBundle> {
    state.db.business_customer(&token, id).await?;
    let org = state.db.organization_by_id(id).await?;
    require_ledger(&state, &org.tenant_id).await?;
    let wallet = Wallet::open_existing(state.db.pool().clone(), &org.tenant_id).await?;
    let scope = CustomerRecordScope {
        key_ids: None,
        include_unattributed: true,
    };
    let receipt = wallet
        .charge_receipt(&state.db, id, &query.identifier, &scope)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let entry = receipt
        .entry_id
        .and_then(|s| s.parse::<Uuid>().ok())
        .ok_or_else(ApiError::not_found)?;
    let key = state
        .config
        .head_signing_seed()
        .map(oxsum_core::signing_key)
        .transpose()
        .map_err(|_| ApiError::Internal)?;
    let mut bundle = wallet
        .scoped_receipt_proof(oxsum_core::EntryId::from_uuid(entry), key.as_ref(), &scope)
        .await?
        .ok_or_else(ApiError::not_found)?;
    // The wholesale catalog binding includes restricted supplier rates. Omit
    // the optional link; never alter signed bytes. Sale binding is retail-only.
    bundle.price = None;
    ok(bundle)
}

async fn require_ledger(state: &AppState, tenant: &str) -> Result<(), ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("ledger_{tenant}.entries"))
        .fetch_one(state.db.pool())
        .await
        .map_err(oxsum_core::WalletError::from)?;
    if exists {
        Ok(())
    } else {
        Err(ApiError::not_found())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecoveryQuery {
    #[serde(default)]
    closed_unknown: bool,
    before: Option<Uuid>,
}
pub(crate) async fn recovery_queue(
    State(state): State<AppState>,
    Extension(token): Extension<crate::auth::PersonalToken>,
    ApiQuery(query): ApiQuery<RecoveryQuery>,
) -> ApiResult<Vec<oxsum_core::usage_recovery::UsageRecoveryQueueRow>> {
    ok(state
        .db
        .business_usage_recovery(&token, query.closed_unknown, query.before)
        .await?)
}
