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

use oxsum_core::{LinkScope, MintedReportLink, Principal, ReportLink, WalletError};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};

/// The body every link mint accepts: an optional operator label, an optional
/// expiry (defaults to 30 days, capped at 90 by the domain layer) and an
/// optional address to mail the link to (issue #315).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CreateLinkBody {
    label: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    email: Option<String>,
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
    #[serde(default)]
    email: Option<String>,
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

/// A freshly minted link: the only response that carries the plaintext token,
/// and the only place `delivery` says whether the mail went out.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreatedReportLinkRes {
    link: ReportLinkRes,
    token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<&'static str>,
}

/// The one address rule applied before minting: a single address the mailer can
/// parse. A malformed one is a caller mistake, not a reason to mint a link the
/// operator believes was mailed.
fn email_of(email: Option<&str>) -> Result<Option<&str>, ApiError> {
    let Some(email) = email.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let valid = email.len() <= 254
        && email.matches('@').count() == 1
        && !email.starts_with('@')
        && !email.ends_with('@')
        && !email.chars().any(char::is_whitespace);
    if valid {
        Ok(Some(email))
    } else {
        Err(WalletError::InvalidInput("email must be a single address".into()).into())
    }
}

/// Sends the mail and reduces the outcome to the wire's `delivery`. A send
/// failure never turns the mint into an error: the link exists, and its token is
/// in the answer for the operator to pass on by hand.
async fn deliver(
    state: &AppState,
    email: Option<&str>,
    minted: &MintedReportLink,
    customer: &str,
    detail: &str,
) -> Option<&'static str> {
    let email = email?;
    let mailer = state.config.mailer()?;
    match mailer
        .send_bill_link(email, &minted.token, customer, detail)
        .await
    {
        Ok(()) => Some("sent"),
        Err(error) => {
            tracing::warn!(%error, "the bill link mail was not sent");
            Some("failed")
        }
    }
}

/// Minor units as credits with six decimals — the page's spelling.
fn credits(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let absolute = minor.unsigned_abs();
    format!("{sign}{}.{:06}", absolute / 1_000_000, absolute % 1_000_000)
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
    let email = email_of(body.email.as_deref())?;
    if email.is_some() && state.config.mailer().is_none() {
        return Err(
            WalletError::InvalidInput("this deployment has no mailer configured".into()).into(),
        );
    }
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
    let delivery = match email {
        None => None,
        Some(_) => {
            let report = state
                .db
                .usage_report(report_id)
                .await?
                .ok_or_else(ApiError::not_found)?;
            let detail = format!(
                "Period: {} to {}\nTotal: {} credits",
                report.from_date,
                report.to_date,
                credits(report.charged_total_minor)
            );
            deliver(&state, email, &minted, &report.end_user, &detail).await
        }
    };
    ok(CreatedReportLinkRes {
        link: minted.link.into(),
        token: minted.token,
        delivery,
    })
}

/// `POST /api/v1/org/usage-reports/links` — a capability link to one downstream
/// customer's issued reports.
pub(crate) async fn create_for_end_user(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(body): ApiJson<CreateEndUserLinkBody>,
) -> ApiResult<CreatedReportLinkRes> {
    let email = email_of(body.email.as_deref())?;
    if email.is_some() && state.config.mailer().is_none() {
        return Err(
            WalletError::InvalidInput("this deployment has no mailer configured".into()).into(),
        );
    }
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
    let delivery = deliver(
        &state,
        email,
        &minted,
        &minted.link.end_user,
        "This link lists every bill the operator has issued to you.",
    )
    .await;
    ok(CreatedReportLinkRes {
        link: minted.link.into(),
        token: minted.token,
        delivery,
    })
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
