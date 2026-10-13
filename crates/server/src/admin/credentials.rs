//! `/api/v1/admin/service-credentials`: the metering surface's own
//! credentials, and the user password-reset rescue.

use axum::extract::{Path, State};
use oxsum_core::audit_action;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};
use crate::{AppState, admin::record};

/// The body of `POST …/service-credentials`: which service holds the
/// credential — the name settlement descriptions record as the reporter.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServiceCredentialReq {
    name: Option<String>,
}

/// Every service credential, newest first — metadata only; the secret exists
/// once, at mint.
pub(crate) async fn service_credentials(
    State(state): State<AppState>,
) -> ApiResult<Vec<oxsum_core::ServiceCredential>> {
    ok(state.db.list_service_credentials().await?)
}

/// Mints a service credential for the metering surface, and answers its secret
/// the one time it exists readable.
pub(crate) async fn mint_service_credential(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ServiceCredentialReq>,
) -> ApiResult<oxsum_core::CreatedServiceCredential> {
    let created = state.db.create_service_credential(request.name).await?;
    record(
        &state,
        audit_action::SERVICE_CREDENTIAL_MINT,
        Some(created.credential.id.to_string()),
        serde_json::json!({
            "name": created.credential.name,
            "prefix": created.credential.prefix,
        }),
        None,
    )
    .await?;
    ok(created)
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
    record(
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
    record(
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
