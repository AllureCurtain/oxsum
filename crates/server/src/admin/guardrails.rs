//! `/api/v1/admin/guardrail-*`: the protection layer's configuration (issue
//! #502) — named policies, and the bindings that attach them to a channel or a
//! key.
//!
//! A policy's `config` is validated at write by the same impls the gateway
//! builds at admission: a keyword set that cannot compile, or a moderation
//! endpoint that is not an absolute http(s) URL, is a 400 here rather than a
//! policy the gateway would only skip — the write refuses what the pipeline
//! could not enforce. A `moderation` policy's `apiKey` is sealed under the
//! deployment's `SecretKey` the way an upstream key is, and rides the policy
//! row as `secret_sealed`; it is never read back through this surface.
//!
//! Deleting a bound policy is allowed: `for_turn` skips a dangling name
//! rather than refuse the turn, the same fail-open the verdicts keep — the
//! alternative (blocking the delete) lets a retired policy block every
//! binding cleanup, which is worse than the unguarded turn it prevents.

use axum::extract::{Path, State};
use oxsum_core::{
    GuardrailBinding, GuardrailPolicy, KeywordConfig, KeywordPolicy, ModerationConfig,
    audit_action, validate_policy_names,
};
use serde::Deserialize;

use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};
use crate::{AppState, admin::record};

/// The body of `PUT …/guardrail-policies/{name}`: the kind and its own config
/// object. `apiKey` is the `moderation` kind's bearer — sealed on write, never
/// echoed; absent leaves a stored secret alone, so re-PUTting a moderation
/// policy's endpoint does not demand its key again.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PolicyReq {
    kind: String,
    config: serde_json::Value,
    api_key: Option<String>,
}

/// Every configured policy, ordered by name. `secretSealed` never leaves the
/// row — the answer carries the rule, never the credential it may hold.
pub(crate) async fn list_policies(
    State(state): State<AppState>,
) -> ApiResult<Vec<GuardrailPolicy>> {
    ok(state.db.list_guardrail_policies().await?)
}

/// Writes or replaces one policy. The name is the path's; the kind picks the
/// config's shape:
///
/// - `keyword` — `{patterns: [{re, action}]}`, compiled before it lands;
/// - `pii` — `{}` (the pattern set is the impl's, not config);
/// - `moderation` — `{endpoint, timeoutMs?}` plus `apiKey` on the first write.
pub(crate) async fn set_policy(
    State(state): State<AppState>,
    Path(name): Path<String>,
    ApiJson(request): ApiJson<PolicyReq>,
) -> ApiResult<GuardrailPolicy> {
    validate_policy_names(std::slice::from_ref(&name))?;
    let secret_sealed = match request.kind.as_str() {
        "keyword" => {
            let config: KeywordConfig = serde_json::from_value(request.config.clone())
                .map_err(|e| ApiError::Validation(format!("keyword config: {e}")))?;
            // Compile here, at write: a pattern the gateway could not build is
            // a 400, not a policy every turn would skip. The name is thrown
            // away — the impl only wants it for its verdicts.
            KeywordPolicy::compile(&name, &config.patterns)?;
            if request.api_key.is_some() {
                return Err(ApiError::Validation(
                    "a keyword policy keeps no apiKey".into(),
                ));
            }
            None
        }
        "pii" => {
            if !request.config.is_object()
                || !request.config.as_object().is_some_and(|o| o.is_empty())
            {
                return Err(ApiError::Validation(
                    "a pii policy's config is the empty object".into(),
                ));
            }
            if request.api_key.is_some() {
                return Err(ApiError::Validation("a pii policy keeps no apiKey".into()));
            }
            None
        }
        "moderation" => {
            let config: ModerationConfig = serde_json::from_value(request.config.clone())
                .map_err(|e| ApiError::Validation(format!("moderation config: {e}")))?;
            // The endpoint is posted the request's texts from inside admission:
            // it must be an absolute http(s) URL, or the callout can only fail.
            let url = reqwest::Url::parse(&config.endpoint).map_err(|_| {
                ApiError::Validation("moderation endpoint must be an absolute URL".into())
            })?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(ApiError::Validation(
                    "moderation endpoint must be http or https".into(),
                ));
            }
            match request.api_key {
                // A present key re-seals and replaces; absent keeps whatever
                // the row already holds — a policy whose endpoint is edited
                // does not demand its bearer again.
                Some(api_key) => {
                    let api_key = api_key.trim();
                    if api_key.is_empty() {
                        return Err(ApiError::Validation("apiKey must not be empty".into()));
                    }
                    // A deployment with no sealing key cannot store a
                    // credential, and saying so beats storing one in the
                    // clear — the same refusal `channels::set` makes.
                    let Some(secret) = state.config.secret() else {
                        tracing::error!(
                            "a moderation policy was configured without OXSUM_SECRET_KEY"
                        );
                        return Err(ApiError::Internal);
                    };
                    Some(secret.seal(api_key)?)
                }
                None => state
                    .db
                    .list_guardrail_policies()
                    .await?
                    .into_iter()
                    .find(|p| p.name == name)
                    .and_then(|p| p.secret_sealed),
            }
        }
        other => {
            return Err(ApiError::Validation(format!(
                "kind must be keyword, pii or moderation, not {other:?}"
            )));
        }
    };
    let policy = state
        .db
        .upsert_guardrail_policy(
            &name,
            &request.kind,
            &request.config,
            secret_sealed.as_deref(),
        )
        .await?;
    record(
        &state,
        audit_action::GUARDRAIL_POLICY_SET,
        Some(name),
        serde_json::json!({"kind": request.kind}),
        None,
    )
    .await?;
    ok(policy)
}

/// Deletes a policy by name; an unknown one is 404. Bindings that named it
/// keep the dangling name — `for_turn` skips it rather than refuse the turn.
pub(crate) async fn delete_policy(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<serde_json::Value> {
    if !state.db.delete_guardrail_policy(&name).await? {
        return Err(ApiError::not_found());
    }
    record(
        &state,
        audit_action::GUARDRAIL_POLICY_DELETE,
        Some(name),
        serde_json::json!({}),
        None,
    )
    .await?;
    ok(serde_json::json!({"deleted": true}))
}

/// Every binding, ordered — the channel and key scopes' policy sets.
pub(crate) async fn list_bindings(
    State(state): State<AppState>,
) -> ApiResult<Vec<GuardrailBinding>> {
    ok(state.db.list_guardrail_bindings().await?)
}

/// The body of `PUT …/guardrail-bindings/{scope}/{target}`: the ordered policy
/// names the scope runs. Every name must already exist — a binding that names
/// nothing would silently unguard the turns it claims to protect, so the write
/// refuses before it lands.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BindingReq {
    policies: Vec<String>,
}

/// Replaces one scope's binding. `scope` is `channel` or `key`; `target` is the
/// channel's name or the key's `key_id`. A channel binding applies to every
/// turn the channel leads; a key binding applies to every key — the key-side
/// *requirement* a single key carries lives on its `requiredPolicies` column.
pub(crate) async fn set_binding(
    State(state): State<AppState>,
    Path((scope, target)): Path<(String, String)>,
    ApiJson(request): ApiJson<BindingReq>,
) -> ApiResult<GuardrailBinding> {
    match scope.as_str() {
        "channel" | "key" => {}
        other => {
            return Err(ApiError::Validation(format!(
                "scope must be channel or key, not {other:?}"
            )));
        }
    }
    if target.trim().is_empty() {
        return Err(ApiError::Validation("target must not be empty".into()));
    }
    validate_policy_names(&request.policies)?;
    state
        .db
        .set_guardrail_binding(&scope, &target, &request.policies)
        .await?;
    record(
        &state,
        audit_action::GUARDRAIL_BINDING_SET,
        Some(format!("{scope}/{target}")),
        serde_json::json!({"policies": request.policies}),
        None,
    )
    .await?;
    ok(GuardrailBinding {
        scope,
        target,
        policies: request.policies,
    })
}

/// Removes one scope's binding; an unknown one is 404. The empty binding a
/// `PUT` with `policies: []` writes is the same "runs nothing" — the DELETE
/// exists so the row does not linger as configuration noise.
pub(crate) async fn delete_binding(
    State(state): State<AppState>,
    Path((scope, target)): Path<(String, String)>,
) -> ApiResult<serde_json::Value> {
    if !state.db.delete_guardrail_binding(&scope, &target).await? {
        return Err(ApiError::not_found());
    }
    record(
        &state,
        audit_action::GUARDRAIL_BINDING_DELETE,
        Some(format!("{scope}/{target}")),
        serde_json::json!({}),
        None,
    )
    .await?;
    ok(serde_json::json!({"deleted": true}))
}
