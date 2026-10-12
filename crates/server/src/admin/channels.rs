//! `/api/v1/admin/channels`: upstreams the gateway proxies to, and the price
//! history each of their models carries.

use axum::extract::{Path, State};
use oxsum_core::{Channel, ModelPrice, Price, audit_action};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};
use crate::{AppState, admin::record};

/// A channel's connection, as the admin API takes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ChannelReq {
    name: String,
    base_url: String,
    /// The upstream credential. Stored sealed; only its last four characters are ever read back.
    api_key: String,
    /// The upstream protocol the channel speaks; defaults to `openai`, the only
    /// protocol this build knows.
    protocol: Option<String>,
}

/// A price to append for one of a channel's models.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PriceReq {
    model: String,
    input_price_per_million: i64,
    /// Required on `chat`, absent or zero on the input-only modes — the
    /// mode-aware rule lives in `Price::validate` (issue #170).
    output_price_per_million: Option<i64>,
    max_output_tokens: Option<i64>,
    cache_read_price_per_million: Option<i64>,
    cache_write_5m_price_per_million: Option<i64>,
    cache_write_1h_price_per_million: Option<i64>,
    reasoning_price_per_million: Option<i64>,
    cost_per_request: Option<i64>,
    upstream: Option<oxsum_core::UpstreamPrices>,
    mode: Option<oxsum_core::BillingMode>,
    /// The route's relative preference for leading a request; absent means 100
    /// (issue #168). The bound is validated in core.
    weight: Option<i64>,
    #[serde(default)]
    rules: Vec<oxsum_core::PriceRule>,
}

/// The version a price change wrote.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VersionRes {
    model: String,
    version: i64,
}

/// Every channel, with the current version of each of its models.
pub(crate) async fn list(State(state): State<AppState>) -> ApiResult<Vec<Channel>> {
    ok(state.db.channels().await?)
}

/// Creates a channel, or replaces the connection of a channel that already exists.
///
/// Its prices are untouched: a connection moves, a price history stays where it is.
pub(crate) async fn set(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ChannelReq>,
) -> ApiResult<Channel> {
    // A deployment with no sealing key cannot store a credential, and saying so beats storing one in
    // the clear. In practice this cannot happen: `prepare` refuses to start without the key once
    // channels exist, and a channel that was just created needs it too.
    let Some(secret) = state.config.secret().cloned() else {
        tracing::error!("a channel was configured without OXSUM_SECRET_KEY");
        return Err(ApiError::Internal);
    };
    state
        .db
        .set_channel(
            &request.name,
            &request.base_url,
            &request.api_key,
            request.protocol.as_deref().unwrap_or(oxsum_core::OPENAI),
            &secret,
        )
        .await?;
    let channel = state
        .db
        .channels()
        .await?
        .into_iter()
        .find(|channel| channel.name == request.name)
        .ok_or(ApiError::Internal)?;
    record(
        &state,
        audit_action::CHANNEL_SET,
        Some(channel.name.clone()),
        serde_json::json!({
            "baseUrl": channel.base_url,
            "protocol": channel.protocol,
        }),
        None,
    )
    .await?;
    ok(channel)
}

/// Appends a price version for one of a channel's models, and reports the version.
///
/// The previous version stays readable through [`history`], which is what makes a bill that names a
/// version checkable later.
pub(crate) async fn append(
    State(state): State<AppState>,
    Path(name): Path<String>,
    ApiJson(request): ApiJson<PriceReq>,
) -> ApiResult<VersionRes> {
    // An unknown channel is not found, rather than a validation failure: the request is well formed
    // and names something that is not there.
    if state.db.channel_prices(&name).await?.is_none() {
        return Err(ApiError::not_found());
    }
    // A chat price with no output rate declared reads as free output — an
    // absent field must not silently mean zero (issue #170). The input-only
    // modes carry none, so the field is optional on the wire and the mode rule
    // is `Price::validate`'s.
    if request.mode.unwrap_or_default() == oxsum_core::BillingMode::Chat
        && (request.output_price_per_million.is_none() || request.max_output_tokens.is_none())
    {
        return Err(ApiError::Validation(
            "a chat price needs outputPricePerMillion and maxOutputTokens".into(),
        ));
    }
    let price = Price {
        input_price_per_million: request.input_price_per_million,
        output_price_per_million: request.output_price_per_million.unwrap_or(0),
        max_output_tokens: request.max_output_tokens.unwrap_or(0),
        cache_read_price_per_million: request.cache_read_price_per_million,
        cache_write_5m_price_per_million: request.cache_write_5m_price_per_million,
        cache_write_1h_price_per_million: request.cache_write_1h_price_per_million,
        reasoning_price_per_million: request.reasoning_price_per_million,
        cost_per_request: request.cost_per_request,
        upstream: request.upstream,
        mode: request.mode.unwrap_or_default(),
        rules: request.rules,
    };
    let version = state
        .db
        .append_price(&name, &request.model, price, request.weight.unwrap_or(100))
        .await?;
    record(
        &state,
        audit_action::CHANNEL_PRICE_APPEND,
        Some(name),
        serde_json::json!({
            "model": request.model,
            "version": version,
        }),
        None,
    )
    .await?;
    ok(VersionRes {
        model: request.model,
        version,
    })
}

/// Every version of every model of one channel, newest first.
///
/// A price change appends, so this is the price history rather than a list of what is in force.
pub(crate) async fn history(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Vec<ModelPrice>> {
    match state.db.channel_prices(&name).await? {
        Some(prices) => ok(prices),
        None => Err(ApiError::not_found()),
    }
}
