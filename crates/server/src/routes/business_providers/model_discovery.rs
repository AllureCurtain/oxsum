//! Fetch one bounded page of candidate model IDs from the pinned channel.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Extension;
use axum::extract::{Path, State};
use oxsum_core::{
    ProviderModelDiscovery, ProviderRemoveInput, ProviderRequest, SecretKey, TaskProviderEndpoint,
};
use oxsum_integrations::provider_http::{ProviderHttp, ProviderHttpError};
use reqwest::{Method, StatusCode};
use serde_json::Value;

use crate::AppState;
use crate::auth::PersonalToken;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};

const DEADLINE: Duration = Duration::from_secs(10);
const MAX_BODY: usize = 1024 * 1024;
const MAX_MODELS: usize = 500;

struct Outcome {
    state: &'static str,
    status: Option<u16>,
    models: Vec<String>,
    partial: bool,
}

impl Outcome {
    fn failure(state: &'static str, status: Option<u16>) -> Self {
        Self {
            state,
            status,
            models: Vec::new(),
            partial: false,
        }
    }
}

pub(crate) async fn discover_models(
    State(state): State<AppState>,
    Extension(PersonalToken(token)): Extension<PersonalToken>,
    Path(channel): Path<String>,
    ApiJson(request): ApiJson<ProviderRequest<ProviderRemoveInput>>,
) -> ApiResult<ProviderModelDiscovery> {
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("provider encryption is not configured".into())
    })?;
    let started = state
        .db
        .begin_provider_model_discovery(&token, &channel, &request.idempotency_key, &request.reason)
        .await?;
    let Some(endpoint) = started.endpoint else {
        return ok(started.result);
    };
    let outcome = tokio::time::timeout(DEADLINE, fetch(&state.provider_http, &endpoint, secret))
        .await
        .unwrap_or_else(|_| Outcome::failure("unreachable", None));
    ok(state
        .db
        .finish_provider_model_discovery(
            started.result.id,
            outcome.state,
            outcome.status,
            &outcome.models,
            outcome.partial,
        )
        .await?)
}

async fn fetch(
    http: &ProviderHttp,
    endpoint: &TaskProviderEndpoint,
    secret: &SecretKey,
) -> Outcome {
    let protocol = endpoint.protocol.as_str();
    if !matches!(protocol, "openai" | "anthropic" | "gemini") {
        return Outcome::failure("unsupported", None);
    }
    let Some(key) = endpoint
        .api_key_sealed
        .as_deref()
        .and_then(|sealed| secret.open(sealed).ok())
    else {
        return Outcome::failure("authenticationFailed", None);
    };
    let call = match http
        .request(
            Method::GET,
            &endpoint.base_url,
            &endpoint.destination_policy,
            &["models"],
        )
        .await
    {
        Ok(call) => call,
        Err(ProviderHttpError::Blocked) => return Outcome::failure("blocked", None),
        Err(ProviderHttpError::Unreachable) => return Outcome::failure("unreachable", None),
    };
    let Ok(mut response) = super::provider_auth(call, protocol, &key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
    else {
        return Outcome::failure("unreachable", None);
    };
    let status = Some(response.status().as_u16());
    match response.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Outcome::failure("authenticationFailed", status);
        }
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED => {
            return Outcome::failure("unsupported", status);
        }
        code if !code.is_success() => return Outcome::failure("httpError", status),
        _ => {}
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= MAX_BODY => {
                bytes.extend_from_slice(&chunk);
            }
            Ok(Some(_)) => return Outcome::failure("invalidCatalog", status),
            Ok(None) => break,
            Err(_) => return Outcome::failure("unreachable", status),
        }
    }
    let Some((models, partial)) = parse_catalog(&bytes, protocol == "gemini", &key) else {
        return Outcome::failure("invalidCatalog", status);
    };
    Outcome {
        state: "listed",
        status,
        models,
        partial,
    }
}

fn parse_catalog(body: &[u8], gemini: bool, key: &str) -> Option<(Vec<String>, bool)> {
    let catalog: Value = serde_json::from_slice(body).ok()?;
    let entries = catalog[if gemini { "models" } else { "data" }].as_array()?;
    let mut partial = if gemini {
        catalog["nextPageToken"]
            .as_str()
            .is_some_and(|token| !token.is_empty())
    } else {
        catalog["has_more"].as_bool() == Some(true)
    };
    let mut models = BTreeSet::new();
    for entry in entries {
        let id = if gemini {
            entry["name"]
                .as_str()
                .and_then(|name| name.strip_prefix("models/"))
        } else {
            entry["id"].as_str()
        };
        match id {
            // Do not normalize IDs or retain a reflected serving credential.
            Some(id)
                if !id.is_empty()
                    && id.len() <= 200
                    && id.trim() == id
                    && !id.chars().any(char::is_control)
                    && !id.contains(key) =>
            {
                models.insert(id);
            }
            _ => partial = true,
        }
    }
    if models.is_empty() && !entries.is_empty() {
        return None;
    }
    partial |= models.len() > MAX_MODELS;
    Some((
        models
            .into_iter()
            .take(MAX_MODELS)
            .map(str::to_owned)
            .collect(),
        partial,
    ))
}
