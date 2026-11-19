//! Business request correction preview and apply; financial rules live in core.
use crate::AppState;
use crate::error::{ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use axum::extract::{Extension, Path, State};
use oxsum_core::{RequestCorrection, RequestCorrectionInput, RequestCorrectionPreview};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreviewQuery {
    source_entry_id: Uuid,
}

pub(crate) async fn preview(
    State(state): State<AppState>,
    Extension(token): Extension<crate::auth::PersonalToken>,
    Path(organization): Path<Uuid>,
    ApiQuery(query): ApiQuery<PreviewQuery>,
) -> ApiResult<RequestCorrectionPreview> {
    ok(state
        .db
        .preview_request_correction(&token, organization, query.source_entry_id)
        .await?)
}
pub(crate) async fn apply(
    State(state): State<AppState>,
    Extension(token): Extension<crate::auth::PersonalToken>,
    Path(organization): Path<Uuid>,
    ApiJson(request): ApiJson<RequestCorrectionInput>,
) -> ApiResult<RequestCorrection> {
    ok(state
        .db
        .apply_request_correction(&token, organization, &request)
        .await?)
}
