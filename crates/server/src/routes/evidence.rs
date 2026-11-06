//! Customer evidence bundles (issue #616): mint, list, download, share and
//! the public token-resolving download.
//!
//! The handlers are thin — the artifact collection, the manifest fold, the
//! marker append and the frozen `bundle_json` all live in
//! `oxsum_core::Db::mint_period_evidence` / `mint_statement_evidence`, and the
//! share-token lifecycle in `create_evidence_share` /
//! `resolve_evidence_share`. What this module owns is the wire: the
//! descriptor the list answers, the `data` envelope the downloads wrap the
//! stored bytes in, and the 410 a dead token earns.

use axum::extract::{Extension, Path, State};
use axum::http::{HeaderValue, StatusCode, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use oxsum_core::{EvidenceBundleRow, EvidenceShare, Principal, signing_key};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok, parse_day, wallet};

/// The `POST /org/evidence-bundles` body: the ended span the bundle covers.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MintPeriodBody {
    from: String,
    to: String,
}

/// The `POST .../share` body — every field optional; absent means the
/// defaults the core applies.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CreateShareBody {
    label: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<time::OffsetDateTime>,
    max_downloads: Option<i32>,
}

/// A bundle's descriptor — what `GET /org/evidence-bundles` lists and `POST`
/// answers. Carries no artifacts or proofs; `GET .../{bundleId}` serves the
/// stored bytes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleSummaryRes {
    bundle_id: Uuid,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    period_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    period_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    statement_id: Option<Uuid>,
    manifest_digest: String,
    anchor_log_index: i64,
    entry_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
}

impl From<EvidenceBundleRow> for BundleSummaryRes {
    fn from(row: EvidenceBundleRow) -> Self {
        Self {
            bundle_id: row.bundle_id,
            kind: row.kind.as_str(),
            period_from: row.period_from.map(|d| d.to_string()),
            period_to: row.period_to.map(|d| d.to_string()),
            statement_id: row.statement_id,
            manifest_digest: row.manifest_digest,
            anchor_log_index: row.anchor_log_index,
            entry_count: row.entry_count,
            created_at: row.created_at,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleListRes {
    rows: Vec<BundleSummaryRes>,
}

/// A share token's descriptor — the list answer, never the token.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShareRes {
    share_id: Uuid,
    bundle_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: time::OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_downloads: Option<i32>,
    download_count: i32,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    revoked_at: Option<time::OffsetDateTime>,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    last_download_at: Option<time::OffsetDateTime>,
}

impl From<EvidenceShare> for ShareRes {
    fn from(share: EvidenceShare) -> Self {
        Self {
            share_id: share.share_id,
            bundle_id: share.bundle_id,
            label: share.label,
            created_at: share.created_at,
            expires_at: share.expires_at,
            max_downloads: share.max_downloads,
            download_count: share.download_count,
            revoked_at: share.revoked_at,
            last_download_at: share.last_download_at,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShareListRes {
    rows: Vec<ShareRes>,
}

/// The minted share's one-time answer: the descriptor plus the plaintext
/// token, which exists only here.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreatedShareRes {
    share: ShareRes,
    token: String,
}

/// The revoke answer: how many live shares the call stamped.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RevokedRes {
    revoked: u64,
}

/// The signing key a bundle's head is signed under, derived the way
/// `log::proof` derives it — `None` leaves the bundle's signature fields
/// absent rather than failing a deployment that signs no heads.
fn signing(state: &AppState) -> Result<Option<oxsum_core::HeadSigningKey>, ApiError> {
    state
        .config
        .head_signing_seed()
        .map(signing_key)
        .transpose()
        .map_err(|_| ApiError::Internal)
}

/// `POST /api/v1/org/evidence-bundles` — mint the period bundle for
/// `[from, to]`, or replay the standing one.
pub(crate) async fn mint_period(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(body): ApiJson<MintPeriodBody>,
) -> ApiResult<BundleSummaryRes> {
    let from = parse_day(&body.from)?;
    let to = parse_day(&body.to)?;
    let organization = principal.organization();
    let w = wallet(&state.tenants, organization).await?;
    let key = signing(&state)?;
    let row = state
        .db
        .mint_period_evidence(
            &w,
            organization.id,
            &organization.tenant_id,
            from,
            to,
            state.config.evidence_upstream_cost(),
            key.as_ref(),
        )
        .await?;
    ok(row.into())
}

/// `GET /api/v1/org/evidence-bundles` — every minted bundle's descriptor,
/// newest first.
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<BundleListRes> {
    let rows = state
        .db
        .evidence_bundles_of(principal.organization().id)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    ok(BundleListRes { rows })
}

/// Reads the stored bundle bytes for `bundle_id` after confirming the
/// organization owns it — the shared tail of the owner download, the
/// statement mint replay and the share resolution. The stored `bundle_json`
/// is the `data` object verbatim; it is served as parsed JSON inside the
/// envelope, never re-parsed for its fields.
async fn owned_bundle_json(
    state: &AppState,
    organization_id: Uuid,
    bundle_id: Uuid,
) -> Result<serde_json::Value, ApiError> {
    if state
        .db
        .evidence_bundle(organization_id, bundle_id)
        .await?
        .is_none()
    {
        // A bundle of another organization answers 404, the same as an
        // unknown one — whether it exists elsewhere is not this org's
        // business.
        return Err(ApiError::not_found());
    }
    let bytes = state
        .db
        .evidence_bundle_json(bundle_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::Internal)
}

/// `GET /api/v1/org/evidence-bundles/{bundleId}` — the frozen bundle bytes.
pub(crate) async fn download(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(bundle_id): Path<Uuid>,
) -> ApiResult<serde_json::Value> {
    let bundle = owned_bundle_json(&state, principal.organization().id, bundle_id).await?;
    ok(bundle)
}

/// `GET /api/v1/org/statements/{statementId}/evidence` — the statement-scoped
/// bundle: mint it on first ask, then serve the standing bytes.
pub(crate) async fn statement_evidence(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(statement_id): Path<Uuid>,
) -> ApiResult<serde_json::Value> {
    let organization = principal.organization();
    let w = wallet(&state.tenants, organization).await?;
    let key = signing(&state)?;
    let row = state
        .db
        .mint_statement_evidence(
            &w,
            organization.id,
            &organization.tenant_id,
            statement_id,
            state.config.evidence_upstream_cost(),
            key.as_ref(),
        )
        .await?;
    let bundle = owned_bundle_json(&state, organization.id, row.bundle_id).await?;
    ok(bundle)
}

/// `POST /api/v1/org/evidence-bundles/{bundleId}/share` — mint a share token.
pub(crate) async fn create_share(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(bundle_id): Path<Uuid>,
    ApiJson(body): ApiJson<CreateShareBody>,
) -> ApiResult<CreatedShareRes> {
    let minted = state
        .db
        .create_evidence_share(
            principal.organization().id,
            bundle_id,
            body.label.as_deref(),
            body.expires_at,
            body.max_downloads,
            principal.user_id(),
        )
        .await?;
    ok(CreatedShareRes {
        share: minted.share.into(),
        token: minted.token,
    })
}

/// `GET /api/v1/org/evidence-bundles/{bundleId}/share` — the bundle's shares,
/// newest first.
pub(crate) async fn list_shares(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(bundle_id): Path<Uuid>,
) -> ApiResult<ShareListRes> {
    // Confirm the bundle is the org's own before listing its shares: an id
    // guessed from another tenant must not even reveal whether shares exist.
    if state
        .db
        .evidence_bundle(principal.organization().id, bundle_id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found());
    }
    let rows = state
        .db
        .evidence_shares_of(principal.organization().id, bundle_id)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    ok(ShareListRes { rows })
}

/// `DELETE /api/v1/org/evidence-bundles/{bundleId}/share` — revoke every live
/// share on the bundle.
pub(crate) async fn revoke_shares(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(bundle_id): Path<Uuid>,
) -> ApiResult<RevokedRes> {
    if state
        .db
        .evidence_bundle(principal.organization().id, bundle_id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found());
    }
    let revoked = state
        .db
        .revoke_evidence_shares(principal.organization().id, bundle_id)
        .await?;
    ok(RevokedRes { revoked })
}

/// `GET /api/v1/evidence/{token}` — the public, session-free download. The
/// token is the credential: `resolve_evidence_share` spends one download of
/// its budget atomically, and an expired, revoked, exhausted or unknown token
/// all earn the same 410 so a guessed token leaks nothing about which shares
/// exist. The stored bytes are served verbatim — the same `data` object the
/// owner download wraps — so the recipient verifies byte-for-byte what the
/// organization committed.
pub(crate) async fn public_download(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Response {
    match serve_shared(&state, &token).await {
        Ok(bytes) => {
            // The stored `bundle_json` is the `data` object verbatim; the API's
            // envelope wraps it. Splicing the bytes inside `{"data":…}` keeps
            // them byte-identical — the recipient verifies byte-for-byte what
            // the organization committed.
            let mut body = Vec::with_capacity(bytes.len() + 9);
            body.extend_from_slice(b"{\"data\":");
            body.extend_from_slice(&bytes);
            body.push(b'}');
            let mut response = body.into_response();
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            response
        }
        Err(response) => *response,
    }
}

/// Resolves the token and reads the bundle bytes, or builds the refusal. A
/// dead token is the caller-facing 410; a storage error is logged detail and
/// a 500 — never a 410, which would wrongly tell the caller its token died.
/// The refusal is boxed: a `Response` is large, and the error path is rare.
async fn serve_shared(state: &AppState, token: &str) -> Result<Vec<u8>, Box<Response>> {
    let resolved = state
        .db
        .resolve_evidence_share(token)
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "evidence share resolution failed");
            Box::new(internal())
        })?;
    let Some(resolved) = resolved else {
        return Err(Box::new(gone()));
    };
    state
        .db
        .evidence_bundle_json(resolved.bundle_id)
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "evidence bundle read failed");
            Box::new(internal())
        })?
        .ok_or_else(|| Box::new(gone()))
}

/// The single answer a dead token earns — expired, revoked, out of downloads
/// and never-existed are indistinguishable, so the body names none of them.
fn gone() -> Response {
    (
        StatusCode::GONE,
        axum::Json(serde_json::json!({
            "error": {
                "code": "NOT_FOUND",
                "message": "this evidence link has expired or been revoked",
                "details": [],
            }
        })),
    )
        .into_response()
}

/// A storage failure's refusal — the log kept the detail.
fn internal() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({
            "error": {
                "code": "INTERNAL_ERROR",
                "message": "internal error",
                "details": [],
            }
        })),
    )
        .into_response()
}
