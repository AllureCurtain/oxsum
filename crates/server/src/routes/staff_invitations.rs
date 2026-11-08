//! Business staff invitation history and explicitly requested SMTP submission.
use axum::extract::State;
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiJson, ApiQuery};
use crate::routes::{ApiResult, business, ok};

#[derive(Deserialize)]
pub(crate) struct Query {
    after: Option<Uuid>,
    limit: Option<i64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    #[serde(flatten)]
    invitations: oxsum_core::StaffInvitationPage,
    mail_configured: bool,
}
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<Query>,
) -> ApiResult<Page> {
    ok(Page {
        invitations: state
            .db
            .staff_invitations(
                &business::personal_token(&headers)?,
                query.after,
                query.limit.unwrap_or(25),
            )
            .await?,
        mail_configured: state.config.mailer().is_some(),
    })
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Created {
    #[serde(flatten)]
    invitation: oxsum_core::CreatedInvitation,
    mail_state: oxsum_core::InvitationDeliveryState,
}
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::StaffInvitationRequest>,
) -> ApiResult<Created> {
    let token = business::personal_token(&headers)?;
    let invitation = state.db.create_staff_invitation(&token, &request).await?;
    if request.send_email
        && let Some(secret) = invitation.token.as_deref()
    {
        let target = state
            .db
            .begin_staff_invitation_mail(
                &token,
                invitation.id,
                secret,
                state.config.mailer().is_some(),
            )
            .await?;
        if let (Some(target), Some(mailer)) = (target, state.config.mailer()) {
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                mailer.send_invitation(&target.email, secret, "the team"),
            )
            .await
            {
                Ok(Ok(())) => {
                    state
                        .db
                        .complete_staff_invitation_mail(target.invitation_id)
                        .await?;
                }
                _ => {
                    tracing::warn!(invitation_id=%invitation.id,"staff invitation SMTP outcome is unknown; replay will not resend");
                }
            }
        }
    }
    let mail_state = state
        .db
        .staff_invitation_mail_state(&token, invitation.id, request.send_email)
        .await?;
    ok(Created {
        invitation,
        mail_state,
    })
}
pub(crate) async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    ApiJson(request): ApiJson<oxsum_core::InvitationRevokeRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .revoke_invitation(
            &business::personal_token(&headers)?,
            id,
            oxsum_core::InvitationScope::Staff,
            &request,
        )
        .await?)
}
