use axum::extract::{Extension, State};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use oxsum_core::{CreatedApiKey, DeviceRequest, Principal};

use crate::AppState;
use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok, person_only};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceCodeRes {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: i64,
}

/// Mints a device request. Public: a tool asking to be authorized holds no
/// credential yet — that is what the grant produces.
pub(crate) async fn device_code(State(state): State<AppState>) -> ApiResult<DeviceCodeRes> {
    let grant = state.db.mint_device_request().await?;
    let expires_in = (grant.expires_at - OffsetDateTime::now_utc()).whole_seconds();
    ok(DeviceCodeRes {
        device_code: grant.device_code,
        user_code: grant.user_code,
        verification_uri: verification_uri(&state),
        expires_in,
        interval: oxsum_core::POLL_INTERVAL.whole_seconds(),
    })
}

/// The page a user code lands on. `OXSUM_PUBLIC_URL` when the deployment names
/// one, else the relative path — the browser resolves either. The approval
/// page is the React app's `/app/device` since the Topcoat pages retired
/// (issue #256); pointing straight there skips the `/device` → `/app/device`
/// redirect.
fn verification_uri(state: &AppState) -> String {
    match state.config.public_url() {
        Some(base) => format!("{}/app/device", base.trim_end_matches('/')),
        None => "/app/device".to_owned(),
    }
}

/// The `code` query parameter of `GET /api/v1/device/request`.
#[derive(Deserialize)]
pub(crate) struct DeviceRequestQuery {
    code: String,
}

/// The pending request the typed code names, for the approval page's confirm
/// step. Session only — an API key is not a person and can approve nothing.
pub(crate) async fn device_request(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(query): ApiQuery<DeviceRequestQuery>,
) -> ApiResult<DeviceRequest> {
    person_only(&principal)?;
    ok(state.db.device_request(&query.code).await?)
}

/// The body of `POST /api/v1/device/authorize`: the user code plus the verdict.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeviceAuthorizeReq {
    user_code: String,
    approve: bool,
}

/// The signed-in user's verdict on a device request. Approving records the
/// session's user and current organization — the key mints there when the tool
/// polls, so an approver can only ever grant into an organization they act in.
pub(crate) async fn device_authorize(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<DeviceAuthorizeReq>,
) -> ApiResult<DeviceRequest> {
    let user_id = person_only(&principal)?;
    ok(state
        .db
        .decide_device_request(
            &request.user_code,
            request.approve,
            user_id,
            principal.organization().id,
        )
        .await?)
}

/// The body of `POST /api/v1/device/token`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeviceTokenReq {
    device_code: String,
}

/// What a poll answers. `apiKey` rides along on `approved` only — and only on
/// the first such poll, which is the one that minted it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceTokenRes {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<CreatedApiKey>,
}

/// The tool's poll. Public: the device code is the credential.
pub(crate) async fn device_token(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<DeviceTokenReq>,
) -> ApiResult<DeviceTokenRes> {
    use oxsum_core::{DevicePoll, PollError};
    let poll = state
        .db
        .poll_device(&request.device_code)
        .await
        .map_err(|e| match e {
            PollError::NotFound => ApiError::not_found(),
            PollError::TooFast { retry_after_secs } => ApiError::PollTooFast { retry_after_secs },
            PollError::Wallet(w) => ApiError::from(w),
        })?;
    let res = match poll {
        DevicePoll::Pending => DeviceTokenRes {
            status: "pending",
            api_key: None,
        },
        DevicePoll::Denied => DeviceTokenRes {
            status: "denied",
            api_key: None,
        },
        DevicePoll::Consumed => DeviceTokenRes {
            status: "consumed",
            api_key: None,
        },
        DevicePoll::Delivered(key) => DeviceTokenRes {
            status: "approved",
            api_key: Some(*key),
        },
    };
    ok(res)
}
