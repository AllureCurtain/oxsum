//! Share links for end-user usage reports (issue #300): mint, list, revoke.
//!
//! The handlers are thin — the token, its hash, the expiry bounds and the
//! revocation all live in `oxsum_core::Db`; this module owns the wire shape and
//! the one rule a route can see: a missing link is 404, never 403, so an id of
//! another organization is indistinguishable from an unknown one.

use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use oxsum_core::{LinkScope, MintedReportLink, Principal, ReportLink};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};

/// The body every link mint accepts: an optional operator label and an optional
/// expiry (defaults to 30 days, capped at 90 by the domain layer).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CreateLinkBody {
    label: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

/// The end-user link body: the customer the reports belong to.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateEndUserLinkBody {
    end_user: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

/// One link as the organization's API answers it — the token is never echoed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportLinkRes {
    link_id: Uuid,
    scope: LinkScope,
    report_id: Option<Uuid>,
    end_user: String,
    label: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    revoked_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    last_used_at: Option<OffsetDateTime>,
}

impl From<ReportLink> for ReportLinkRes {
    fn from(link: ReportLink) -> Self {
        Self {
            link_id: link.link_id,
            scope: link.scope,
            report_id: link.report_id,
            end_user: link.end_user,
            label: link.label,
            created_at: link.created_at,
            expires_at: link.expires_at,
            revoked_at: link.revoked_at,
            last_used_at: link.last_used_at,
        }
    }
}

/// A freshly minted link: the only response that carries the plaintext token.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreatedReportLinkRes {
    link: ReportLinkRes,
    token: String,
}

impl From<MintedReportLink> for CreatedReportLinkRes {
    fn from(minted: MintedReportLink) -> Self {
        Self {
            link: minted.link.into(),
            token: minted.token,
        }
    }
}

/// The organization's links, newest first.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportLinkListRes {
    rows: Vec<ReportLinkRes>,
}

/// `POST /api/v1/org/usage-reports/{reportId}/links` — a capability link to one
/// minted report. The report must belong to the acting organization.
pub(crate) async fn create_for_report(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(report_id): Path<Uuid>,
    body: Option<ApiJson<CreateLinkBody>>,
) -> ApiResult<CreatedReportLinkRes> {
    let body = body.map_or_else(CreateLinkBody::default, |ApiJson(body)| body);
    let minted = state
        .db
        .create_report_link(
            principal.organization().id,
            report_id,
            body.label.as_deref(),
            body.expires_at,
            principal.user_id(),
        )
        .await?;
    ok(minted.into())
}

/// `POST /api/v1/org/usage-reports/links` — a capability link to one downstream
/// customer's issued reports.
pub(crate) async fn create_for_end_user(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(body): ApiJson<CreateEndUserLinkBody>,
) -> ApiResult<CreatedReportLinkRes> {
    let minted = state
        .db
        .create_end_user_link(
            principal.organization().id,
            &body.end_user,
            body.label.as_deref(),
            body.expires_at,
            principal.user_id(),
        )
        .await?;
    ok(minted.into())
}

/// `GET /api/v1/org/usage-reports/links` — every link the organization minted,
/// newest first. Tokens are never part of the answer.
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<ReportLinkListRes> {
    let rows = state
        .db
        .report_links_of(principal.organization().id)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    ok(ReportLinkListRes { rows })
}

/// `DELETE /api/v1/org/usage-reports/links/{linkId}` — revoke, idempotently. An
/// id of another organization answers 404, the same as an unknown one.
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(link_id): Path<Uuid>,
) -> ApiResult<ReportLinkRes> {
    match state
        .db
        .revoke_report_link(principal.organization().id, link_id)
        .await?
    {
        Some(link) => ok(link.into()),
        None => Err(ApiError::not_found()),
    }
}
