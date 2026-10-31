use axum::extract::{Extension, Path, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use oxsum_core::{
    ApiKey, CreatedApiKey, CreatedMandate, Entitlement, EntitlementMandate, Mandate,
    MandateVerdict, Principal, RpmVerdict, TpmVerdict, WalletError,
};

use crate::AppState;
use crate::error::{ApiError, ApiJson, ApiQuery};
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

/// The body of `POST /api/v1/org/keys/{keyId}/mandates` (issue #262): the bounds
/// the delegate may spend inside — always narrowing the key's own.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateMandateReq {
    /// Total committed spend this mandate may carry, in minor units.
    cap_minor: i64,
    /// The models the delegate may call — a subset of the key's allowlist;
    /// absent means all the key serves.
    #[serde(default)]
    models: Option<Vec<String>>,
    /// RFC 3339; absent means valid from now.
    #[serde(default, with = "time::serde::rfc3339::option")]
    not_before: Option<OffsetDateTime>,
    /// RFC 3339; required — mandates are bounded and never revoked.
    #[serde(with = "time::serde::rfc3339")]
    not_after: OffsetDateTime,
}

/// Mint a signed spend mandate on a key of the organization (issue #262). The
/// key holder or an owner/admin session asks for one; the answer carries the
/// signed blob exactly once, for the delegate to present in `X-Oxsum-Mandate`.
/// A mandate may only narrow the key: a cap over its `spendLimitMinor` or a
/// model outside its `modelAllowlist` is a 400.
pub(crate) async fn create_mandate(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key_id): Path<Uuid>,
    ApiJson(r): ApiJson<CreateMandateReq>,
) -> ApiResult<CreatedMandate> {
    let secret = state
        .config
        .secret()
        .ok_or_else(|| ApiError::ServiceUnavailable("mandate signing is not configured".into()))?;
    match state
        .db
        .create_mandate(
            principal.organization().id,
            key_id,
            principal.key_scope(),
            r.cap_minor,
            r.models,
            r.not_before,
            r.not_after,
            secret,
        )
        .await?
    {
        Some(created) => ok(created),
        None => Err(ApiError::not_found()),
    }
}

/// The mandates a key has minted, newest first — metadata only, never the
/// signed blob (the delegate already carries it). Scoped like the key list:
/// a member sees only mandates on keys they created.
pub(crate) async fn list_mandates(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key_id): Path<Uuid>,
) -> ApiResult<Vec<Mandate>> {
    ok(state
        .db
        .list_mandates(principal.organization().id, key_id, principal.key_scope())
        .await?)
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

/// The query of `GET /api/v1/org/keys/{keyId}/entitlement` (issue #501): the
/// request the answer should be shaped for — the model it would call, the
/// charge it would take, and the `metadata` tags it would carry (repeated
/// `tag=name:value`, resolving which tag-scoped token buckets the request
/// would land in).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntitlementQuery {
    /// The gateway model the request would call; absent skips the allowlist question.
    #[serde(default)]
    model: Option<String>,
    /// A hypothetical charge in minor units — the freeze a real request would take.
    #[serde(default)]
    estimate: Option<i64>,
    /// The request's `metadata` tag values, `name:value` — resolves which
    /// tag-scoped TPM buckets the turn would accrue into.
    #[serde(default)]
    tag: Vec<String>,
}

/// `GET /api/v1/org/keys/{keyId}/entitlement`: may this key's request be
/// admitted now — the same checks admission runs, read-only (issue #501).
///
/// The ledger-side verdicts come from `Wallet::entitlement_read`; the request
/// window is peeked at through the server's limiter (consuming nothing) and
/// the settled-token window through `Db::tpm_window*`, and
/// `Entitlement::compose` picks the first refusal in the documented order. A
/// presented `X-Oxsum-Mandate` adds the delegation's own verdict — a blob that
/// cannot verify is the `mandate_invalid` answer, not an error: diagnosing a
/// refused delegate is half of what the endpoint is for.
pub(crate) async fn key_entitlement(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key_id): Path<Uuid>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<EntitlementQuery>,
) -> ApiResult<Entitlement> {
    let estimate = query.estimate;
    if estimate.is_some_and(|e| e <= 0) {
        return Err(ApiError::from(WalletError::InvalidInput(
            "estimate must be a positive amount in minor units".into(),
        )));
    }
    let mut tags = std::collections::BTreeMap::new();
    for pair in &query.tag {
        let Some((name, value)) = pair.split_once(':') else {
            return Err(ApiError::from(WalletError::InvalidInput(
                "a tag parameter is `name:value`".into(),
            )));
        };
        tags.insert(name.to_owned(), value.to_owned());
    }
    // A presented mandate is verified the way the hold paths verify it — the
    // signature checks against the asked-about key's own public half. Any way
    // the blob can fail is the `mandate_invalid` verdict, not an HTTP error:
    // the caller is asking *why* a delegate cannot call, and a blob that does
    // not verify *is* the answer.
    let mandate = match headers
        .get(crate::gateway::MANDATE_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some(blob) => {
            let verifying = match state.config.secret() {
                Some(secret) => state.db.mandate_verifying_key(&key_id, secret).await?,
                None => None,
            };
            match verifying.and_then(|v| oxsum_core::parse_presented(blob, &v)) {
                Some(presented) => Some(MandateVerdictOrMandate::Verified(EntitlementMandate {
                    mandate_id: presented.verified.mandate_id,
                    key_id: presented.verified.key_id,
                    not_before_ts: presented.not_before_ts(),
                    not_after_ts: presented.not_after_ts(),
                    cap_minor: presented.verified.cap_minor,
                    models: presented.verified.models.clone(),
                })),
                None => Some(MandateVerdictOrMandate::Invalid),
            }
        }
        None => None,
    };
    let w = crate::routes::wallet(&state.tenants, principal.organization()).await?;
    let (mandate_param, mandate_presented_invalid) = match &mandate {
        Some(MandateVerdictOrMandate::Verified(m)) => (Some(m), false),
        Some(MandateVerdictOrMandate::Invalid) => (None, true),
        None => (None, false),
    };
    let read = w
        .entitlement_read(
            &principal.organization().id,
            &key_id,
            principal.key_scope(),
            query.model.as_deref(),
            estimate,
            &tags,
            mandate_param,
        )
        .await?;
    let Some(mut read) = read else {
        // A key id outside the caller's scope — another organization's, or a
        // member's colleague's — is not found, never forbidden: ids cannot be probed.
        return Err(ApiError::not_found());
    };
    if mandate_presented_invalid {
        read.mandate = Some(MandateVerdict::Invalid);
    }
    // The request window is peeked, not admitted: `peek` reports the allowance
    // `admit` would answer without recording the request.
    let rpm = read.requests_per_minute.and_then(|limit| {
        (limit > 0).then(|| {
            match state
                .rate_limiter
                .peek(key_id, limit as u32, std::time::Instant::now())
            {
                Ok(allowance) => RpmVerdict {
                    limit: allowance.limit,
                    remaining: allowance.remaining,
                    allowed: true,
                    retry_after_secs: None,
                },
                Err(limited) => RpmVerdict {
                    limit: limited.limit,
                    remaining: 0,
                    allowed: false,
                    retry_after_secs: Some(limited.retry_after.as_secs()),
                },
            }
        })
    });
    // The token window, when the key caps it: settled tokens inside the
    // trailing minute against the cap — `settled < limit` says the window has
    // room at all, `remainingTpm` how much. The estimate is a money figure, not
    // tokens, so it is not judged here; each tag-scoped bucket the request's
    // tags land in is checked the same way, and the tightest is the answer's.
    let mut tpm: Option<TpmVerdict> = None;
    if let Some(limit) = read.tokens_per_minute
        && limit > 0
    {
        let (settled, retry_after_secs) = state.db.tpm_window(key_id).await?;
        tpm = Some(TpmVerdict {
            settled,
            limit: i64::from(limit),
            allowed: settled < i64::from(limit),
            retry_after_secs,
        });
    }
    for (scope_name, cap) in &read.tpm_scopes {
        let (settled, retry_after_secs) = state.db.tpm_window_scoped(key_id, scope_name).await?;
        let verdict = TpmVerdict {
            settled,
            limit: *cap,
            allowed: settled < *cap,
            retry_after_secs,
        };
        // The tightest bucket is the answer's figure — a refusal always is;
        // between passing buckets the least headroom names what the caller
        // would hit first.
        let replace = match &tpm {
            None => true,
            Some(current) => match (current.allowed, verdict.allowed) {
                (true, false) => true,
                (true, true) => {
                    (verdict.limit - verdict.settled) < (current.limit - current.settled)
                }
                (false, _) => false,
            },
        };
        if replace {
            tpm = Some(verdict);
        }
    }
    ok(Entitlement::compose(read, rpm, tpm))
}

/// What a presented `X-Oxsum-Mandate` resolved to for the entitlement read:
/// the verified fields to judge, or the mark that the blob cannot verify —
/// which the answer reports as `mandate_invalid` rather than an error.
enum MandateVerdictOrMandate {
    /// `parse_presented` verified the blob: the fields to judge.
    Verified(EntitlementMandate),
    /// The blob cannot verify — no seed, bad signature, foreign shape.
    Invalid,
}
