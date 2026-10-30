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
    /// The upstream credential, single-key spelling. Stored sealed; only its
    /// last four characters are ever read back. Merged into `apiKeys` when both
    /// arrive — one channel's pool, one field to carry it.
    api_key: Option<String>,
    /// The channel's credential pool (issue #444): the whole ordered set of
    /// upstream keys, sent together because a `set` re-credentials the channel.
    /// Absent leaves the stored pool untouched; an empty array is a 400 — a
    /// channel with no key cannot serve.
    api_keys: Option<Vec<String>>,
    /// The admin billing credential the `upstream-bill-sync` pass reads the
    /// provider's cost API under (issue #283): triple-state like the other
    /// optional constraint fields — absent leaves a stored billing key
    /// untouched, `null` clears it, a value replaces it.
    #[serde(default, deserialize_with = "crate::admin::double_option")]
    bill_key: Option<Option<String>>,
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
    // The pool is one field's worth of keys however it was spelled: `apiKeys`
    // carries the ordered set, `apiKey` is the single-key spelling of it. Both
    // absent leaves a stored pool alone; a present-but-empty set is a 400 —
    // a channel with no key cannot serve.
    let api_keys: Option<Vec<String>> = match (request.api_key, request.api_keys) {
        (single, Some(mut keys)) => {
            if let Some(single) = single {
                keys.push(single);
            }
            if keys.is_empty() {
                return Err(ApiError::Validation(
                    "apiKeys must carry at least one key".into(),
                ));
            }
            Some(keys)
        }
        (Some(single), None) => Some(vec![single]),
        (None, None) => None,
    };
    state
        .db
        .set_channel_pool(
            &request.name,
            &request.base_url,
            api_keys.as_deref(),
            request.protocol.as_deref().unwrap_or(oxsum_core::OPENAI),
            &secret,
        )
        .await?;
    // The billing credential is independent of the connection above: a present
    // `billKey` replaces or clears it, an absent one leaves it untouched —
    // posting a fresh `apiKey` is not a reason to drop the billing key.
    if let Some(bill_key) = request.bill_key {
        state
            .db
            .set_bill_key(&request.name, bill_key.as_deref(), &secret)
            .await?;
    }
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

/// An alias to claim for a channel, as the admin API takes it (issue #286):
/// the public name callers write, and the upstream model the channel relays.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AliasReq {
    alias: String,
    upstream_model: String,
}

/// One channel's aliases, oldest first.
///
/// An unknown channel is not found, as in [`append`]: the request is well
/// formed and names something that is not there.
pub(crate) async fn aliases(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Vec<oxsum_core::ModelAlias>> {
    match state.db.list_aliases(&name).await? {
        Some(aliases) => ok(aliases),
        None => Err(ApiError::not_found()),
    }
}

/// Claims an alias for a channel: callers write `alias`, the channel relays
/// `upstreamModel` (issue #286).
///
/// The one-name-one-route-set rule is enforced in core: a name already claimed
/// as an alias on any channel, or spelled as a direct model on a different
/// channel, is a 409 — two routes can never split a served name's traffic.
pub(crate) async fn set_alias(
    State(state): State<AppState>,
    Path(name): Path<String>,
    ApiJson(request): ApiJson<AliasReq>,
) -> ApiResult<oxsum_core::ModelAlias> {
    let written = state
        .db
        .create_alias(&name, &request.alias, &request.upstream_model)
        .await
        .map_err(|error| match &error {
            // An unknown channel is not found, like `append`'s: the alias is
            // well formed and names something that is not there.
            oxsum_core::WalletError::InvalidInput(message)
                if message.starts_with("no channel named") =>
            {
                ApiError::not_found()
            }
            _ => ApiError::from(error),
        })?;
    record(
        &state,
        audit_action::CHANNEL_ALIAS_SET,
        Some(name),
        serde_json::json!({
            "alias": written.alias,
            "upstreamModel": written.upstream_model,
        }),
        None,
    )
    .await?;
    ok(written)
}

/// Drops one of a channel's aliases.
///
/// A name that is not this channel's — unknown to it, or claimed by another
/// channel — is a 404: the caller asked to remove something this channel does
/// not have. Aliases are routing configuration, not ledger data, so the delete
/// is hard (docs/decisions.md).
pub(crate) async fn delete_alias(
    State(state): State<AppState>,
    Path((name, alias)): Path<(String, String)>,
) -> ApiResult<serde_json::Value> {
    if !state.db.delete_alias(&name, &alias).await? {
        return Err(ApiError::not_found());
    }
    record(
        &state,
        audit_action::CHANNEL_ALIAS_DELETE,
        Some(name),
        serde_json::json!({ "alias": alias }),
        None,
    )
    .await?;
    ok(serde_json::json!({}))
}

/// A credential to add to a channel's pool, as the admin API takes it (issue
/// #444): the upstream key, stored sealed, appended at the pool's end.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CredentialReq {
    api_key: String,
}

/// A credential status change, as the admin API takes it: `active` picks the
/// credential again, `disabled` keeps it on record and never picks it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CredentialStatusReq {
    status: String,
}

/// Adds one credential to a channel's pool (issue #444): one channel carries
/// several upstream keys, so a single account's 429 can fail over onto the
/// channel's next key before the turn leaves for a differently-priced route.
/// The key lands sealed at the pool's end — take the ordering up with a
/// `POST /channels` resend of `apiKeys` when it should lead.
pub(crate) async fn add_credential(
    State(state): State<AppState>,
    Path(name): Path<String>,
    ApiJson(request): ApiJson<CredentialReq>,
) -> ApiResult<oxsum_core::ChannelCredential> {
    let Some(secret) = state.config.secret().cloned() else {
        tracing::error!("a channel credential was added without OXSUM_SECRET_KEY");
        return Err(ApiError::Internal);
    };
    let written = state
        .db
        .add_credential(&name, &request.api_key, &secret)
        .await
        .map_err(|error| match &error {
            // An unknown channel is not found, like `append`'s: the request is
            // well formed and names something that is not there.
            oxsum_core::WalletError::InvalidInput(message)
                if message.starts_with("no channel named") =>
            {
                ApiError::not_found()
            }
            _ => ApiError::from(error),
        })?;
    record(
        &state,
        audit_action::CHANNEL_CREDENTIAL_ADD,
        Some(name),
        serde_json::json!({
            "credentialId": written.credential_id,
            "position": written.position,
        }),
        None,
    )
    .await?;
    ok(written)
}

/// Switches one credential between `active` and `disabled`: the operator's way
/// of taking a key out of the pick without losing its record — a disabled
/// credential keeps its health state and can be re-enabled, which a drop does
/// not survive.
pub(crate) async fn set_credential_status(
    State(state): State<AppState>,
    Path((name, credential_id)): Path<(String, uuid::Uuid)>,
    ApiJson(request): ApiJson<CredentialStatusReq>,
) -> ApiResult<serde_json::Value> {
    if !state
        .db
        .set_credential_status(&name, credential_id, &request.status)
        .await?
    {
        return Err(ApiError::not_found());
    }
    record(
        &state,
        audit_action::CHANNEL_CREDENTIAL_STATUS,
        Some(name),
        serde_json::json!({
            "credentialId": credential_id,
            "status": request.status,
        }),
        None,
    )
    .await?;
    ok(serde_json::json!({}))
}

/// Drops one credential from a channel's pool. A credential that is not this
/// channel's is a 404, like the alias delete. The pool's last credential is a
/// 400 rather than a delete: a channel with no key cannot serve, so the row
/// stays until the operator disables it or retires the channel itself.
pub(crate) async fn drop_credential(
    State(state): State<AppState>,
    Path((name, credential_id)): Path<(String, uuid::Uuid)>,
) -> ApiResult<serde_json::Value> {
    if !state.db.drop_credential(&name, credential_id).await? {
        return Err(ApiError::not_found());
    }
    record(
        &state,
        audit_action::CHANNEL_CREDENTIAL_DROP,
        Some(name),
        serde_json::json!({ "credentialId": credential_id }),
        None,
    )
    .await?;
    ok(serde_json::json!({}))
}
