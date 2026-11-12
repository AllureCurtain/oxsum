//! Business provider transport. Authority and all mutation rules live in core.

use std::time::Duration;

use crate::AppState;
use crate::auth::PersonalToken;
use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use axum::extract::{Path, State};
use oxsum_core::{
    BusinessChannelPage, ModelAlias, ModelPrice, ProviderAliasInput, ProviderBillKeyInput,
    ProviderCacheInput, ProviderChange, ProviderConnection, ProviderConnectionCheck,
    ProviderCredentialInput, ProviderFallbackInput, ProviderMutationResult, ProviderPriceInput,
    ProviderRemoveInput, ProviderRequest, ProviderStatusInput,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}
pub(crate) async fn connection_check(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(channel): Path<String>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderRemoveInput>>,
) -> ApiResult<ProviderConnectionCheck> {
    let PersonalToken(token) = token;
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("provider encryption is not configured".into())
    })?;
    let started = state
        .db
        .begin_provider_check(&token, &channel, &request.idempotency_key, &request.reason)
        .await?;
    let Some(endpoint) = started.endpoint else {
        return ok(started.result);
    };
    let result = async {
        let Some(sealed) = endpoint.api_key_sealed.as_deref() else {
            return ("authenticationFailed", None);
        };
        let Ok(key) = secret.open(sealed) else {
            return ("authenticationFailed", None);
        };
        let call = state
            .provider_http
            .request(
                reqwest::Method::HEAD,
                &endpoint.base_url,
                &endpoint.destination_policy,
                &[],
            )
            .await;
        let call = match call {
            Ok(call) => call,
            Err(oxsum_integrations::provider_http::ProviderHttpError::Blocked) => {
                return ("blocked", None);
            }
            Err(_) => return ("unreachable", None),
        };
        let call = match endpoint.protocol.as_str() {
            "anthropic" => call
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            "gemini" => call.header("x-goog-api-key", key),
            _ => call.bearer_auth(key),
        };
        match call.timeout(Duration::from_secs(10)).send().await {
            Ok(response) => {
                let status = response.status();
                let category = if status.is_success() || status.as_u16() == 405 {
                    "reachable"
                } else if status.as_u16() == 401 || status.as_u16() == 403 {
                    "authenticationFailed"
                } else {
                    "httpError"
                };
                (category, Some(status.as_u16()))
            }
            Err(_) => ("unreachable", None),
        }
    }
    .await;
    ok(state
        .db
        .finish_provider_check(started.result.id, result.0, result.1)
        .await?)
}
pub(crate) async fn channels(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<BusinessChannelPage> {
    ok(state
        .db
        .business_channels(&token, page.after, page.limit.unwrap_or(50))
        .await?)
}
pub(crate) async fn prices(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(name): Path<String>,
) -> ApiResult<Vec<ModelPrice>> {
    ok(state.db.business_channel_prices(&token, &name).await?)
}
pub(crate) async fn aliases(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(name): Path<String>,
) -> ApiResult<Vec<ModelAlias>> {
    ok(state.db.business_channel_aliases(&token, &name).await?)
}
pub(crate) async fn fallbacks(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(model): Path<String>,
) -> ApiResult<serde_json::Value> {
    ok(serde_json::json!({"fallbacks":state.db.business_model_fallbacks(&token,&model).await?}))
}
async fn change(
    state: &AppState,
    token: &str,
    key: &str,
    reason: &str,
    change: ProviderChange,
) -> ApiResult<ProviderMutationResult> {
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("provider encryption is not configured".into())
    })?;
    ok(state
        .db
        .change_business_provider(token, key, reason, &change, secret)
        .await?)
}
pub(crate) async fn set_channel(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderConnection>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::Connection {
            config: request.config,
        },
    )
    .await
}
pub(crate) async fn add_credential(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(channel): Path<String>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderCredentialInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::AddCredential {
            channel,
            config: request.config,
        },
    )
    .await
}
pub(crate) async fn bill_key(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(channel): Path<String>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderBillKeyInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::BillKey {
            channel,
            config: request.config,
        },
    )
    .await
}
pub(crate) async fn credential_status(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path((channel, credential)): Path<(String, Uuid)>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderStatusInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::CredentialStatus {
            channel,
            credential,
            config: request.config,
        },
    )
    .await
}
pub(crate) async fn remove_credential(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path((channel, credential)): Path<(String, Uuid)>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderRemoveInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::RemoveCredential {
            channel,
            credential,
        },
    )
    .await
}
pub(crate) async fn cache(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(channel): Path<String>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderCacheInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::Cache {
            channel,
            config: request.config,
        },
    )
    .await
}
pub(crate) async fn append_price(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(channel): Path<String>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderPriceInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::Price {
            channel,
            config: Box::new(request.config),
        },
    )
    .await
}
pub(crate) async fn set_alias(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(channel): Path<String>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderAliasInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::Alias {
            channel,
            config: request.config,
        },
    )
    .await
}
pub(crate) async fn remove_alias(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path((channel, alias)): Path<(String, String)>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderRemoveInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::RemoveAlias { channel, alias },
    )
    .await
}
pub(crate) async fn set_fallbacks(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(model): Path<String>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderFallbackInput>>,
) -> ApiResult<ProviderMutationResult> {
    change(
        &state,
        &token,
        &request.idempotency_key,
        &request.reason,
        ProviderChange::Fallbacks {
            model,
            config: request.config,
        },
    )
    .await
}
