//! Customer invitation history and explicitly requested SMTP submission.
use axum::extract::State;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::auth::PersonalToken;
use crate::error::{ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    after: Option<Uuid>,
    limit: Option<i64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    #[serde(flatten)]
    invitations: oxsum_core::CustomerInvitationPage,
    mail_configured: bool,
}
pub(crate) async fn list(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiQuery(query): ApiQuery<Query>,
) -> ApiResult<Page> {
    ok(Page {
        invitations: state
            .db
            .customer_invitations(&token, query.after, query.limit.unwrap_or(25))
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
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiJson(request): ApiJson<oxsum_core::CustomerInvitationRequest>,
) -> ApiResult<Created> {
    let PersonalToken(token) = token;
    let invitation = state
        .db
        .create_customer_invitation(&token, &request)
        .await?;
    if request.send_email
        && let Some(secret) = invitation.token.as_deref()
    {
        let target = state
            .db
            .begin_customer_invitation_mail(
                &token,
                invitation.id,
                secret,
                state.config.mailer().is_some(),
            )
            .await?;
        if let (Some(target), Some(mailer)) = (target, state.config.mailer()) {
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                mailer.send_invitation(&target.email, secret, &target.name),
            )
            .await
            {
                Ok(Ok(())) => {
                    state.db.complete_customer_invitation_mail(&target).await?;
                }
                _ => {
                    tracing::warn!(invitation_id=%invitation.id,"customer invitation SMTP outcome is unknown; replay will not resend");
                }
            }
        }
    }
    let mail_state = state
        .db
        .customer_invitation_mail_state(&token, invitation.id)
        .await?;
    ok(Created {
        invitation,
        mail_state,
    })
}
