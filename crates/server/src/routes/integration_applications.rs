//! Owner application setup and separately scoped service customer lifecycle.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiJson, ApiQuery};
use crate::routes::{ApiResult, business, ok};
use oxsum_core::{
    ApplicationCustomer, ApplicationCustomerArchiveRequest, IntegrationApplication,
    IntegrationApplicationPage, IntegrationApplicationRequest, ProvisionCustomerRequest,
};

#[derive(Deserialize)]
pub(crate) struct PageQuery {
    after: Option<Uuid>,
    limit: Option<i64>,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> ApiResult<IntegrationApplicationPage> {
    ok(state
        .db
        .integration_applications(
            &business::personal_token(&headers)?,
            query.after,
            query.limit.unwrap_or(50),
        )
        .await?)
}

pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<IntegrationApplicationRequest>,
) -> ApiResult<IntegrationApplication> {
    ok(state
        .db
        .create_integration_application(&business::personal_token(&headers)?, &request)
        .await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CustomerQuery {
    external_id: String,
}

pub(crate) async fn customer(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<CustomerQuery>,
) -> ApiResult<ApplicationCustomer> {
    let service = business::integration(&state, &headers).await?;
    ok(state
        .db
        .application_customer(&service, &query.external_id)
        .await?)
}

pub(crate) async fn provision(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ProvisionCustomerRequest>,
) -> ApiResult<ApplicationCustomer> {
    let service = business::integration(&state, &headers).await?;
    ok(state
        .db
        .provision_application_customer(&service, &request)
        .await?)
}

pub(crate) async fn archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    ApiJson(request): ApiJson<ApplicationCustomerArchiveRequest>,
) -> ApiResult<ApplicationCustomer> {
    let service = business::integration(&state, &headers).await?;
    ok(state
        .db
        .archive_application_customer(&service, id, &request)
        .await?)
}
