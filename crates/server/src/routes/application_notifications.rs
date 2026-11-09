//! Owner endpoint configuration and scoped application delivery metadata.
use crate::AppState;
use crate::error::{ApiJson, ApiQuery};
use crate::routes::{ApiResult, business, ok};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use oxsum_core::{
    ApplicationNotificationDeliveries, ApplicationNotificationDelivery,
    ApplicationNotificationEndpoint, ApplicationNotificationEndpointRequest,
    ApplicationNotificationEndpoints, ApplicationNotificationMutation,
    CreatedApplicationNotificationEndpoint, WalletError,
};
use serde::Deserialize;
use uuid::Uuid;
#[derive(Deserialize)]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app): Path<Uuid>,
    ApiJson(request): ApiJson<ApplicationNotificationEndpointRequest>,
) -> ApiResult<CreatedApplicationNotificationEndpoint> {
    let key = state.config.secret().ok_or_else(|| {
        WalletError::Misconfigured("notification signing requires OXSUM_SECRET_KEY".into())
    })?;
    ok(state
        .db
        .create_application_notification_endpoint(
            &business::personal_token(&headers)?,
            app,
            &request,
            key,
        )
        .await?)
}
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(app): Path<Uuid>,
) -> ApiResult<ApplicationNotificationEndpoints> {
    ok(state
        .db
        .business_application_notification_endpoints(&business::personal_token(&headers)?, app)
        .await?)
}
pub(crate) async fn disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app, id)): Path<(Uuid, Uuid)>,
    ApiJson(request): ApiJson<ApplicationNotificationMutation>,
) -> ApiResult<ApplicationNotificationEndpoint> {
    ok(state
        .db
        .disable_application_notification_endpoint(
            &business::personal_token(&headers)?,
            app,
            id,
            &request,
        )
        .await?)
}
pub(crate) async fn deliveries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app, id)): Path<(Uuid, Uuid)>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<ApplicationNotificationDeliveries> {
    ok(state
        .db
        .business_application_notification_deliveries(
            &business::personal_token(&headers)?,
            app,
            id,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}
pub(crate) async fn retry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((app, id, delivery)): Path<(Uuid, Uuid, Uuid)>,
    ApiJson(request): ApiJson<ApplicationNotificationMutation>,
) -> ApiResult<ApplicationNotificationDelivery> {
    ok(state
        .db
        .retry_application_notification(
            &business::personal_token(&headers)?,
            app,
            id,
            delivery,
            &request,
        )
        .await?)
}
pub(crate) async fn service_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<ApplicationNotificationEndpoints> {
    let actor = business::integration(&state, &headers).await?;
    ok(state.db.application_notification_endpoints(&actor).await?)
}
pub(crate) async fn service_deliveries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<ApplicationNotificationDeliveries> {
    let actor = business::integration(&state, &headers).await?;
    ok(state
        .db
        .application_notification_deliveries(&actor, id, page.after, page.limit.unwrap_or(50))
        .await?)
}
