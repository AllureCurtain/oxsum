//! The model catalog surface: `GET /v1/models`, and the named-serving lookup
//! admission shares with the pipeline.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use oxsum_core::{ActingKey, BillingMode, Organization, Serving};
use serde_json::json;

use crate::AppState;
use crate::gateway::error::GatewayError;

/// Lists the models this deployment can serve, in OpenAI's shape.
///
/// Only models with a price appear: a model the gateway cannot price cannot be frozen, so it is not
/// served at all. An `event`-mode price is not a model at all — it is the billable code a service
/// reports on `/api/v1/metering`; it appears only for the separate generic task adapter.
/// Listing a task model does not certify a particular media provider. `created` is when
/// the version in force was written — the closest thing a price row has to a creation time — and
/// `owned_by` is the channel that serves it. A model served by several channels lists once, under
/// the route that leads it (issue #168).
///
/// A business running sale schedules narrows the list further: a channel-priced
/// model whose sale terms do not cover this customer yet — the window has not
/// opened, or no schedule reaches it — would bill wholesale, so it is left out
/// rather than listed and then failing the call (issue #1052). A deployment
/// with no sale schedule at all is wholesale-only and lists every priced model.
pub(super) async fn models(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Extension(key): Extension<ActingKey>,
) -> Result<Response, GatewayError> {
    let access = state
        .db
        .serving_access(organization.id, Some(key.key_id))
        .await?;
    let catalog = state.db.catalog_for_access(&access).await?;
    let mut listed = BTreeMap::new();
    // The core catalog orders equal public names by weight, after permission
    // filtering. No denied route can provide this row's channel identity.
    for route in &catalog {
        if route.price.mode != BillingMode::Event || route.protocol == oxsum_core::TASKS {
            listed.entry(route.model.clone()).or_insert_with(|| {
                json!({
                    "id": route.model,
                    "object": "model",
                    "created": route.created_at.unix_timestamp(),
                    "owned_by": route.channel,
                })
            });
        }
    }
    // Sale coverage narrows the list when schedules exist (issue #1052): a
    // model whose terms are scheduled for later is not billable under the
    // caller's deal yet, so listing it reads as served when it is not.
    let names: Vec<String> = listed.keys().cloned().collect();
    if let Some(covered) = state
        .db
        .sale_priced_models(
            access.business_id,
            organization.id,
            &names,
            crate::today_at(),
        )
        .await?
    {
        listed.retain(|model, _| covered.contains(model));
    }
    Ok(
        Json(json!({"object":"list","data":listed.into_values().collect::<Vec<_>>()}))
            .into_response(),
    )
}

/// The routes a request for this model may take, plus the upstream name an
/// alias resolved to (issue #286) — `None` when the asked name is served
/// directly or when nothing serves it.
///
/// A deployment with no sealing key serves no channel at all: `app` refuses to start when channels
/// exist without one, so this is the "the wallet is running, the gateway is not configured" case.
/// A credential that will not open is [`oxsum_core::WalletError::Misconfigured`], which becomes a
/// 500: the operator's mistake, not the caller's.
pub(super) async fn servings_named(
    state: &AppState,
    model: &str,
    access: &oxsum_core::ServingAccess,
) -> Result<(Vec<Serving>, Option<String>), GatewayError> {
    let Some(secret) = state.config.secret() else {
        return Ok((Vec::new(), None));
    };
    Ok(state
        .db
        .servings_named_for_access(model, secret, access)
        .await?)
}
