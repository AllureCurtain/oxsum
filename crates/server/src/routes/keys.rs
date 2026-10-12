use axum::extract::{Extension, Path, State};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use oxsum_core::{ApiKey, CreatedApiKey, Principal, WalletError};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateKeyReq {
    name: Option<String>,
    /// RFC 3339; absent means a key that never expires.
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    /// Minor units; absent or null means unlimited.
    #[serde(default)]
    spend_limit_minor: Option<i64>,
    /// "daily" | "weekly" | "monthly"; absent keeps the limit cumulative.
    #[serde(default)]
    budget_duration: Option<String>,
    /// The gateway models this key may call; absent or null allows all of them.
    #[serde(default)]
    model_allowlist: Option<Vec<String>>,
    /// Requests inside a rolling minute; absent or null is uncapped.
    #[serde(default)]
    requests_per_minute: Option<i32>,
    /// Holds outstanding at once; absent or null is uncapped.
    #[serde(default)]
    max_concurrent_holds: Option<i32>,
    /// Settled tokens inside a rolling minute; absent or null is uncapped.
    #[serde(default)]
    tokens_per_minute: Option<i32>,
    /// Tag-scoped slices of the token allowance; absent or null scopes nothing.
    #[serde(default)]
    tokens_per_minute_scopes: Option<oxsum_core::TpmScopes>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateKeyConstraintsReq {
    /// The new limit in minor units; null clears it back to unlimited.
    #[serde(default)]
    spend_limit_minor: Option<i64>,
    /// The window the limit applies to; null clears it back to cumulative.
    #[serde(default)]
    budget_duration: Option<String>,
    /// The models the key may call; null clears the list back to all.
    #[serde(default)]
    model_allowlist: Option<Vec<String>>,
    /// The per-minute request allowance; null clears it back to uncapped.
    #[serde(default)]
    requests_per_minute: Option<i32>,
    /// The outstanding-holds cap; null clears it back to uncapped.
    #[serde(default)]
    max_concurrent_holds: Option<i32>,
    /// The per-minute token allowance; null clears it back to uncapped.
    #[serde(default)]
    tokens_per_minute: Option<i32>,
    /// The tag-scoped token allowances; null scopes nothing.
    #[serde(default)]
    tokens_per_minute_scopes: Option<oxsum_core::TpmScopes>,
}

/// Builds the constraint set both key endpoints carry, parsing the duration's name.
fn key_constraints(r: &UpdateKeyConstraintsReq) -> Result<oxsum_core::KeyConstraints, WalletError> {
    Ok(oxsum_core::KeyConstraints {
        spend_limit_minor: r.spend_limit_minor,
        budget_duration: r
            .budget_duration
            .as_deref()
            .map(oxsum_core::BudgetDuration::parse)
            .transpose()?,
        model_allowlist: r.model_allowlist.clone(),
        requests_per_minute: r.requests_per_minute,
        max_concurrent_holds: r.max_concurrent_holds,
        tokens_per_minute: r.tokens_per_minute,
        tokens_per_minute_scopes: r.tokens_per_minute_scopes.clone(),
    })
}
pub(crate) async fn create_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    body: Option<ApiJson<CreateKeyReq>>,
) -> ApiResult<CreatedApiKey> {
    let (name, expires_at, constraints) = body
        .map_or(
            Ok((None, None, oxsum_core::KeyConstraints::default())),
            |ApiJson(r)| {
                key_constraints(&UpdateKeyConstraintsReq {
                    spend_limit_minor: r.spend_limit_minor,
                    budget_duration: r.budget_duration,
                    model_allowlist: r.model_allowlist,
                    requests_per_minute: r.requests_per_minute,
                    max_concurrent_holds: r.max_concurrent_holds,
                    tokens_per_minute: r.tokens_per_minute,
                    tokens_per_minute_scopes: r.tokens_per_minute_scopes,
                })
                .map(|c| (r.name, r.expires_at, c))
            },
        )
        .map_err(ApiError::from)?;
    // A key minted through a session records who minted it, for product.md's per-member
    // rules; a key minted with an API key records no creator, because no person acts there.
    ok(state
        .db
        .create_key(
            principal.organization().id,
            name,
            expires_at,
            principal.user_id(),
            constraints,
        )
        .await?)
}

pub(crate) async fn list_keys(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<ApiKey>> {
    ok(state
        .db
        .list_keys(principal.organization().id, principal.key_scope())
        .await?)
}

pub(crate) async fn revoke_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key_id): Path<Uuid>,
) -> ApiResult<ApiKey> {
    // A key id of another organization — or, for a member, a key they did not create — is
    // not found, not forbidden: an id should not be probeable for existence.
    match state
        .db
        .revoke_key(principal.organization().id, key_id, principal.key_scope())
        .await?
    {
        Some(key) => ok(key),
        None => Err(ApiError::not_found()),
    }
}

pub(crate) async fn patch_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key_id): Path<Uuid>,
    ApiJson(r): ApiJson<UpdateKeyConstraintsReq>,
) -> ApiResult<ApiKey> {
    // The scope rules are the revoke's: a member may change only the keys they created,
    // and any other key id answers 404 with the key untouched, so ids cannot be probed.
    match state
        .db
        .update_key_constraints(
            principal.organization().id,
            key_id,
            principal.key_scope(),
            key_constraints(&r)?,
        )
        .await?
    {
        Some(key) => ok(key),
        None => Err(ApiError::not_found()),
    }
}
