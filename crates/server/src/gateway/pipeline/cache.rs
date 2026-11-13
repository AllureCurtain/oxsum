//! The semantic-cache key's digest members — the request's own headers, the
//! admission-time key constraints, and the experiment/mandate/route bindings
//! the answer was produced under. Each is a pure `serde_json::Value`
//! constructor the caller's cache-store assembly composes; nothing here talks
//! to a wallet or the network. Two turns share an answer only when every one
//! of these digests matches.

use axum::http::HeaderMap;
use oxsum_core::{ActingKey, Serving};
use serde_json::{Value, json};

use super::anthropic_version;
use crate::gateway::request::Surface;

/// The caller headers an upstream answer depends on, in the protocol's own
/// spelling. Only the Anthropic surface forwards a version (with a default
/// when the caller omitted it) and an optional beta set; every other surface's
/// request is fully described by its body.
pub(super) fn cache_semantic_headers(surface: Surface, headers: &HeaderMap) -> Value {
    match surface {
        Surface::Anthropic => {
            let mut map = serde_json::Map::new();
            map.insert("version".to_owned(), json!(anthropic_version(headers)));
            if let Some(beta) = headers.get("anthropic-beta").and_then(|v| v.to_str().ok()) {
                map.insert("beta".to_owned(), json!(beta));
            }
            Value::Object(map)
        }
        _ => Value::Object(serde_json::Map::new()),
    }
}

/// The key's admission constraints as they stood for this request: authority
/// state the answer was admitted under, not part of the prompt.
pub(super) fn cache_key_constraints(key: &ActingKey) -> Value {
    json!({
        "keyId": key.key_id,
        "spendLimitMinor": key.spend_limit_minor,
        "requestsPerMinute": key.requests_per_minute,
        "tokensPerMinute": key.tokens_per_minute,
        "tokensPerMinuteScopes": key.tokens_per_minute_scopes,
        "requiredPolicies": key.required_policies,
    })
}

/// The experiment binding that selected the served model, if any. Two variants
/// of the same experiment never share an answer even when they resolve to the
/// same upstream name: the settlement and usage row name the variant.
pub(super) fn cache_experiment(binding: &oxsum_core::ExperimentBinding) -> Value {
    json!({
        "experimentId": binding.experiment_id,
        "variant": binding.variant,
    })
}

/// The spend mandate the turn presented, if any: the authority it delegated
/// under, not part of the prompt. Two mandates with different identities never
/// share an answer, so a cache hit cannot reuse a turn admitted under a
/// different delegated authority.
pub(super) fn cache_mandate(mandate: Option<&oxsum_core::PresentedMandate>) -> Value {
    mandate.map_or(Value::Null, |mandate| {
        json!({
            "mandateId": mandate.verified.mandate_id,
            "hash": mandate.verified.hash,
            "models": mandate.verified.models,
            "requiredPolicies": mandate.verified.required_policies,
        })
    })
}

/// Private route configuration enters only the keyed digest. Every admitted
/// route participates, including fallback aliases and their provider targets.
pub(super) fn cache_route(route: &Serving) -> Value {
    json!({
        "channelId": route.channel_id,
        "channel": &route.channel,
        "protocol": &route.protocol,
        "baseUrl": &route.base_url,
        "settings": &route.settings,
        "credentialConfiguration": &route.credential_configuration,
        "version": route.version,
        "weight": route.weight,
        "configurationUpdatedAt":
            route.configuration_updated_at.unix_timestamp_nanos().to_string(),
        "price": &route.price,
    })
}
