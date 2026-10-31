//! `/api/v1/admin/model-fallbacks`: the ordered chain of models a request may
//! serve under when its own model's every channel fails (issue #500).
//!
//! A chain is model → model, channel-agnostic: the fallback is a named model a
//! caller could have asked for, never a channel — the gateway resolves it
//! through the same `servings_named` its own name produces, so its own channels
//! and aliases apply. It is mutable config, not ledger: a `PUT` replaces the
//! whole chain atomically, a `DELETE` drops it, and bills already written still
//! name the model that answered.

use axum::extract::{Path, State};
use oxsum_core::audit_action;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};
use crate::{AppState, admin::record};

/// The chain as the admin API takes and returns it: `fallbacks` in try order.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FallbackChain {
    fallbacks: Vec<String>,
}

/// One model's fallback chain, in try order — empty for a model with none.
pub(crate) async fn get_fallbacks(
    State(state): State<AppState>,
    Path(model): Path<String>,
) -> ApiResult<FallbackChain> {
    let fallbacks = state.db.fallbacks_for(&model).await?;
    ok(FallbackChain { fallbacks })
}

/// Replaces one model's whole fallback chain atomically.
///
/// Every name must resolve to a served model or a registered alias — a chain
/// naming an unserved model would dead-end a request that already failed once,
/// so the write refuses a 400 rather than landing a route that cannot serve.
/// The model itself inside its own chain is refused the same way: the loop it
/// would build serves nobody. The model the chain hangs off must be served too.
/// An empty `fallbacks` clears the chain, the same write `DELETE` performs.
pub(crate) async fn set_fallbacks(
    State(state): State<AppState>,
    Path(model): Path<String>,
    ApiJson(request): ApiJson<FallbackChain>,
) -> ApiResult<FallbackChain> {
    state.db.set_fallbacks(&model, &request.fallbacks).await?;
    record(
        &state,
        audit_action::MODEL_FALLBACK_SET,
        Some(model.clone()),
        serde_json::json!({ "fallbacks": request.fallbacks }),
        None,
    )
    .await?;
    ok(FallbackChain {
        fallbacks: request.fallbacks,
    })
}

/// Drops one model's fallback chain; 404 when the model had none.
///
/// The chain is routing configuration, not ledger data, so the delete is hard:
/// a settled bill still names the model that answered.
pub(crate) async fn delete_fallbacks(
    State(state): State<AppState>,
    Path(model): Path<String>,
) -> ApiResult<serde_json::Value> {
    if !state.db.clear_fallbacks(&model).await? {
        return Err(ApiError::not_found());
    }
    record(
        &state,
        audit_action::MODEL_FALLBACK_DELETE,
        Some(model),
        serde_json::json!({}),
        None,
    )
    .await?;
    ok(serde_json::json!({ "cleared": true }))
}
