//! Legacy service credential metadata/revocation and user password-reset rescue.

use axum::extract::{Path, State};
use oxsum_core::audit_action;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};

/// Every legacy service credential, newest first; disabled metadata only.
pub(crate) async fn service_credentials(
    State(state): State<AppState>,
) -> ApiResult<Vec<oxsum_core::ServiceCredential>> {
    ok(state.db.list_service_credentials().await?)
}

/// Global service issuance is retired; rejection precedes body parsing.
pub(crate) async fn mint_service_credential() -> Result<(), ApiError> {
    Err(ApiError::Forbidden(
        "use owner-issued scoped integration credentials".into(),
    ))
}

/// Revokes a service credential: it authenticates nothing from here on; what it
/// already wrote stands.
pub(crate) async fn revoke_service_credential(
    State(state): State<AppState>,
    Path(credential_id): Path<Uuid>,
) -> ApiResult<oxsum_core::ServiceCredential> {
    let credential = state
        .db
        .revoke_service_credential(credential_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    crate::admin::record(
        &state,
        audit_action::SERVICE_CREDENTIAL_REVOKE,
        Some(credential_id.to_string()),
        serde_json::json!({ "name": credential.name }),
        None,
    )
    .await?;
    ok(credential)
}

/// The body of `POST …/password-reset`: the replacement password. The same
/// length rule signup applies runs in core.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PasswordResetReq {
    new_password: String,
}

/// The reset as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PasswordResetRes {
    user_id: Uuid,
    /// Live sessions the reset revoked alongside the password change.
    sessions_revoked: u64,
}

/// Sets a new password for the named user and revokes every session they hold —
/// the only recovery path v1 has, since no email is sent.
pub(crate) async fn reset_password(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    ApiJson(request): ApiJson<PasswordResetReq>,
) -> ApiResult<PasswordResetRes> {
    let sessions_revoked = state
        .db
        .reset_password(user_id, &request.new_password)
        .await?;
    crate::admin::record(
        &state,
        audit_action::USER_PASSWORD_RESET,
        Some(user_id.to_string()),
        serde_json::json!({ "sessionsRevoked": sessions_revoked }),
        None,
    )
    .await?;
    ok(PasswordResetRes {
        user_id,
        sessions_revoked,
    })
}
