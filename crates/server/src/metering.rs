//! The `/api/v1/metering` routes: the deployer's services report billable
//! events here (issue #172). Handlers deserialize and answer; the business
//! rules — price resolution, hold/settle/release against the wallet, the watch
//! row — live in `oxsum_core`'s metering module.
//!
//! The credential is a service credential and nothing else: `require_service`
//! refuses API keys and never reads the session cookie, so an organization can
//! never report its own usage.

use axum::extract::{Extension, State};
use axum::routing::post;
use axum::{Router, middleware};
use oxsum_core::{ActingService, MeteredEvent, MeteredHold, MeteredOutcome, UsageRecord};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::require_service;
use crate::error::ApiJson;
use crate::routes::{ApiResult, ok};
use crate::{AppState, today};

/// The metering surface: `holds` freezes an event's bound, `settle` and
/// `release` close it, `settlements` bills a completed event in one call.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/holds", post(hold))
        .route("/settle", post(settle))
        .route("/release", post(release))
        .route("/settlements", post(settle_once))
        .layer(middleware::from_fn_with_state(state, require_service))
}

/// A `holds` call: which organization's wallet freezes, under which channel's
/// `event`-mode price for `billable_code`, bounded by the declared usage.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HoldReq {
    idempotency_key: String,
    organization_id: Uuid,
    channel: String,
    billable_code: String,
    event_id: Option<String>,
    usage: UsageRecord,
}

/// A `settle` call: the outstanding hold, and the usage the event actually
/// consumed. The settlement's idempotency key derives from the hold's, so the
/// request carries none.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettleReq {
    organization_id: Uuid,
    hold_key: String,
    event_id: Option<String>,
    usage: UsageRecord,
}

/// A `release` call: the hold the event will never settle against.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseReq {
    organization_id: Uuid,
    hold_key: String,
}

/// A one-shot `settlements` call — `holds`'s request shape for an event that
/// already ran.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettleOnceReq {
    idempotency_key: String,
    organization_id: Uuid,
    channel: String,
    billable_code: String,
    event_id: Option<String>,
    usage: UsageRecord,
}

/// Freezes the declared usage as the event's bound and answers the hold key.
async fn hold(
    State(state): State<AppState>,
    Extension(service): Extension<ActingService>,
    ApiJson(request): ApiJson<HoldReq>,
) -> ApiResult<MeteredHold> {
    let HoldReq {
        idempotency_key,
        organization_id,
        channel,
        billable_code,
        event_id,
        usage,
    } = request;
    ok(state
        .db
        .metering_hold(
            &state.tenants,
            &service,
            organization_id,
            &idempotency_key,
            &MeteredEvent {
                channel,
                billable_code,
                event_id,
                usage,
            },
            today(),
        )
        .await?)
}

/// Settles an outstanding hold at the declared usage, capped by its freeze.
async fn settle(
    State(state): State<AppState>,
    Extension(service): Extension<ActingService>,
    ApiJson(request): ApiJson<SettleReq>,
) -> ApiResult<MeteredOutcome> {
    ok(state
        .db
        .metering_settle(
            &state.tenants,
            &service,
            request.organization_id,
            &request.hold_key,
            request.event_id,
            &request.usage,
            today(),
        )
        .await?)
}

/// Releases an outstanding hold at zero — the event is not billing.
async fn release(
    State(state): State<AppState>,
    Extension(service): Extension<ActingService>,
    ApiJson(request): ApiJson<ReleaseReq>,
) -> ApiResult<MeteredOutcome> {
    ok(state
        .db
        .metering_release(
            &state.tenants,
            &service,
            request.organization_id,
            &request.hold_key,
            today(),
        )
        .await?)
}

/// Bills a completed event in one call: hold and settle under keys derived
/// from the one idempotency key.
async fn settle_once(
    State(state): State<AppState>,
    Extension(service): Extension<ActingService>,
    ApiJson(request): ApiJson<SettleOnceReq>,
) -> ApiResult<MeteredOutcome> {
    let SettleOnceReq {
        idempotency_key,
        organization_id,
        channel,
        billable_code,
        event_id,
        usage,
    } = request;
    ok(state
        .db
        .metering_settle_once(
            &state.tenants,
            &service,
            organization_id,
            &idempotency_key,
            &MeteredEvent {
                channel,
                billable_code,
                event_id,
                usage,
            },
            today(),
        )
        .await?)
}
