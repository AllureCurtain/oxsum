use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use oxsum_core::{
    Member, NewUser, Organization, Ownership, Principal, Registration, Role, UserOrganization,
    WalletError,
};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::auth::{require_bot_check, send_verification};
use crate::routes::{ApiResult, SessionInfo, double_option_i64, double_option_uuid, ok, person};

pub(crate) async fn session(Extension(principal): Extension<Principal>) -> ApiResult<SessionInfo> {
    match principal {
        Principal::Session(principal) => ok(SessionInfo::from(principal)),
        Principal::Key(_) => Err(ApiError::not_found()),
    }
}

pub(crate) async fn organization(
    Extension(principal): Extension<Principal>,
) -> ApiResult<Organization> {
    ok(principal.organization().clone())
}

/// The body of `PATCH /api/v1/org`: the display unit the organization renders
/// in (issue #257), or null to render credits alone.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateOrgDisplayReq {
    /// The fiat unit — `code`, `ratePerCredit`, `decimals` — or `null` to
    /// clear it. Presentational only: no ledger write and no money moves.
    display_currency: Option<oxsum_core::DisplayCurrency>,
}

/// Sets the organization's display currency (issue #257). A person-only write:
/// the session must sit in the owner or admin role — a member's session and an
/// API key are refused, the same rule membership management applies, because
/// the display is the organization's own choice and a credential that is not a
/// person cannot speak for it.
pub(crate) async fn update_display(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<UpdateOrgDisplayReq>,
) -> ApiResult<Organization> {
    let actor = principal.membership_actor()?;
    // Owner or admin only — the display is the organization's own setting, so
    // the write borrows the membership rules' first check rather than adding a
    // second role vocabulary for it.
    match actor.role {
        Role::Owner | Role::Admin => {}
        Role::Member => {
            return Err(oxsum_core::WalletError::Forbidden(
                "only an owner or an admin may change the organization's display currency".into(),
            )
            .into());
        }
    }
    state
        .db
        .set_display_currency(principal.organization().id, request.display_currency)
        .await?;
    // Read back what stands: the answer is the stored row's rendering, not the
    // request's — a write that parsed differently in storage cannot drift the
    // two apart.
    let mut organization = principal.organization().clone();
    organization.display_currency = state
        .db
        .organization_by_id(principal.organization().id)
        .await?
        .display_currency;
    ok(organization)
}

/// The `GET /api/v1/org/promo-lots` query: whether closed lots ride the list.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PromoLotsQuery {
    /// `true` keeps expired and cancelled lots in the answer; absent lists
    /// only what can still fund a hold.
    include_closed: Option<bool>,
}

/// The organization's promo lots, soonest-expiring first — the order admission
/// draws them in. Any member may read them: a lot is the organization's own
/// granted credit, the same visibility the balance carries. The response is
/// the read model behind the wallet's promo pool — `remainingMinor` is what
/// new holds can still pin, `reservedMinor` what open holds already named.
pub(crate) async fn promo_lots(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    crate::error::ApiQuery(query): crate::error::ApiQuery<PromoLotsQuery>,
) -> ApiResult<Vec<oxsum_core::PromoLot>> {
    ok(state
        .db
        .promo_lots(
            principal.organization().id,
            query.include_closed.unwrap_or(false),
        )
        .await?)
}

/// Who may act on memberships of *several* organizations: a person, never a key.
///
/// Listing, creating and switching between organizations are all a person's actions —
/// an API key belongs to exactly one organization and has no user to hold a second
/// membership, so it is refused the same way membership management refuses it
/// (docs/decisions.md, "membership management is a person's action").
pub(crate) async fn my_organizations(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<UserOrganization>> {
    let principal = person(principal)?;
    ok(state.db.organizations_of(principal.user.id).await?)
}

/// The body of `POST /api/v1/orgs`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateOrgReq {
    name: String,
}

/// Creates a team organization and makes the session's user its owner.
pub(crate) async fn create_organization(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CreateOrgReq>,
) -> ApiResult<Organization> {
    let principal = person(principal)?;
    ok(state
        .db
        .create_team_organization(principal.user.id, &request.name)
        .await?)
}

/// The body of `POST /api/v1/session/organization`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SwitchOrgReq {
    organization_id: Uuid,
}

/// Switches the acting organization: the session row is updated, so the choice
/// carries every request the cookie makes from now on.
pub(crate) async fn switch_organization(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<SwitchOrgReq>,
) -> ApiResult<SessionInfo> {
    let mut principal = person(principal)?;
    let (organization, role) = state
        .db
        .switch_organization(
            principal.session.id,
            principal.user.id,
            request.organization_id,
        )
        .await?;
    principal.organization = organization;
    principal.role = role;
    ok(SessionInfo::from(principal))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeleteAccountReq {
    password: String,
}

/// Deletes the session user's account (issue #202): pseudonymization, one
/// transaction — the identity fields are tombstoned, the sessions and minted
/// keys die, the memberships go, and the attribution their keys wrote is
/// nulled. The ledger is untouched, so the account's bills still verify.
///
/// Session-only: a key is not a person. The session that asks dies in the same
/// commit, so the answer is the last thing the credential ever receives.
pub(crate) async fn delete_account(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    ApiJson(request): ApiJson<DeleteAccountReq>,
) -> ApiResult<oxsum_core::AccountDeletion> {
    let token = super::business::personal_token(&headers)?;
    let user = state.db.session_user(&token).await?;
    let deletion = state.db.delete_account(user.id, &request.password).await?;
    ok(deletion)
}

/// The body of `POST /api/v1/invitations/redeem`: the link's token plus the account
/// to create.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RedeemReq {
    token: String,
    email: String,
    password: String,
    /// The Turnstile widget's answer — required when the deployment configures
    /// the anti-bot check, ignored when it does not (issue #154).
    turnstile_token: Option<String>,
}

/// A token is present on first creation only; retries cannot resend it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreatedInvitationRes {
    #[serde(flatten)]
    invitation: oxsum_core::CreatedInvitation,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<&'static str>,
}
pub(crate) async fn create_invitation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::TeamInvitationRequest>,
) -> ApiResult<CreatedInvitationRes> {
    let token = super::business::personal_token(&headers)?;
    let email = super::recipient_email(request.email.as_deref())?;
    if email.is_some() && state.config.mailer().is_none() {
        return Err(
            WalletError::InvalidInput("this deployment has no mailer configured".into()).into(),
        );
    }
    let minted = state.db.create_invitation(&token, &request).await?;
    let delivery = match (email, state.config.mailer(), minted.token.as_deref()) {
        (Some(to), Some(mailer), Some(secret)) => {
            let organization = state.db.organization_by_id(minted.organization_id).await?;
            match mailer.send_invitation(to, secret, &organization.name).await {
                Ok(()) => Some("sent"),
                Err(error) => {
                    tracing::warn!(%error, "the invitation mail was not sent");
                    Some("failed")
                }
            }
        }
        _ => None,
    };
    ok(CreatedInvitationRes {
        invitation: minted,
        delivery,
    })
}
pub(crate) async fn accept_invitation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::InvitationAcceptRequest>,
) -> ApiResult<oxsum_core::InvitationAcceptance> {
    ok(state
        .db
        .accept_invitation(&super::business::personal_token(&headers)?, &request.token)
        .await?)
}
pub(crate) async fn revoke_invitation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    ApiJson(request): ApiJson<oxsum_core::InvitationRevokeRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .revoke_invitation(
            &super::business::personal_token(&headers)?,
            id,
            false,
            &request,
        )
        .await?)
}

/// Registers through an invitation link. Open in every signup mode — an `invite`
/// deployment's registrations all come through here.
pub(crate) async fn redeem_invitation(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RedeemReq>,
) -> ApiResult<Registration> {
    // Before touching the invitation: a forged token costs the check a
    // verifier call, which the verifier rate-limits far better than our
    // redemption table would take it.
    require_bot_check(&state, request.turnstile_token.as_deref()).await?;
    let mut registration = state
        .db
        .redeem_invitation(
            &request.token,
            NewUser {
                email: request.email,
                password: request.password,
                organization_name: None,
            },
        )
        .await?;
    registration.verification_sent = send_verification(&state, registration.user.id).await;
    ok(registration)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateMemberReq {
    /// The role half of the change; absent leaves it.
    role: Option<AssignableRole>,
    /// The member budget half: absent leaves it, an explicit null clears it
    /// (issue #162).
    #[serde(default, deserialize_with = "double_option_i64")]
    budget_limit_minor: Option<Option<i64>>,
    /// The team half (issue #440): absent leaves the assignment, an explicit
    /// null unassigns, a team id reassigns the member — the team's own
    /// budget then gates the member's *next* hold.
    #[serde(default, deserialize_with = "double_option_uuid")]
    team_id: Option<Option<Uuid>>,
}

/// The roles a membership may be changed to.
///
/// `owner` is deliberately not a value: ownership is a single seat moved by
/// `POST /api/v1/org/ownership`, so no request body reaches this surface able to promote
/// anyone — the refusal is in the contract rather than in a branch (crates/server/openapi.yaml).
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum AssignableRole {
    Admin,
    Member,
}

impl From<AssignableRole> for Role {
    fn from(role: AssignableRole) -> Self {
        match role {
            AssignableRole::Admin => Self::Admin,
            AssignableRole::Member => Self::Member,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransferOwnershipReq {
    user_id: Uuid,
}

/// Adds an existing account to the organization as a member.
///
/// The acting role is checked here and in the domain layer (crates/core/src/orgs.rs): a
/// membership write needs a session in the owner or admin role, so an API key is refused by
/// `membership_actor` before the request reaches a rule.
/// The organization's members, as the dashboard's members page lists them.
/// Reading is any member's right — the membership writes on the same paths are
/// the owner/admin-gated ones.
pub(crate) async fn list_members(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<MembersRes> {
    ok(MembersRes {
        members: state.db.members(principal.organization().id).await?,
    })
}

/// `GET /api/v1/org/members` answers `{members: [...]}`.
#[derive(serde::Serialize)]
pub(crate) struct MembersRes {
    members: Vec<Member>,
}

pub(crate) async fn add_member() -> ApiResult<Member> {
    Err(WalletError::Forbidden(
        "direct membership addition is retired; invite the person and require acceptance".into(),
    )
    .into())
}

pub(crate) async fn remove_member(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Member> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .remove_member(principal.organization().id, actor, user_id)
        .await?)
}

/// Changes a member's role and/or spend budget in one PATCH.
///
/// `budgetLimitMinor` is a double option: absent leaves the cap alone, an
/// explicit null clears it, a figure sets it. Both halves absent is the
/// domain layer's validation error (issue #162).
pub(crate) async fn update_member(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
    ApiJson(r): ApiJson<UpdateMemberReq>,
) -> ApiResult<Member> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .update_member(
            principal.organization().id,
            actor,
            user_id,
            r.role.map(Role::from),
            r.budget_limit_minor,
            r.team_id,
        )
        .await?)
}

pub(crate) async fn transfer_ownership(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<TransferOwnershipReq>,
) -> ApiResult<Ownership> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .transfer_ownership(principal.organization().id, actor, r.user_id)
        .await?)
}

// ── Teams (issue #440): the budget subgroup layer between members and the
// organization credit line. Management follows the membership rules — an
// owner or an admin, never an API key — so the handlers borrow
// `membership_actor` the same way the member writes do.

/// `GET /api/v1/org/teams` answers `{teams: [...]}`.
#[derive(serde::Serialize)]
pub(crate) struct TeamsRes {
    teams: Vec<oxsum_core::Team>,
}

/// The teams the organization carries, as the organization surface lists
/// them. Reading is any member's right — the writes beside it are the
/// owner/admin-gated ones.
pub(crate) async fn list_teams(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<TeamsRes> {
    ok(TeamsRes {
        teams: state.db.teams(principal.organization().id).await?,
    })
}

/// The body of `POST /api/v1/org/teams`: the subgroup's name and its
/// committed-spend cap — `null` or absent for an uncapped team.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateTeamReq {
    name: String,
    budget_limit_minor: Option<i64>,
}

pub(crate) async fn create_team(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<CreateTeamReq>,
) -> ApiResult<oxsum_core::Team> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .create_team(
            principal.organization().id,
            actor,
            &r.name,
            r.budget_limit_minor,
        )
        .await?)
}

/// The body of `PATCH /api/v1/org/teams/{teamId}`: either half of the team's
/// shape, both optional — `budgetLimitMinor` is a double option where `null`
/// clears the cap.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateTeamReq {
    name: Option<String>,
    #[serde(default, deserialize_with = "double_option_i64")]
    budget_limit_minor: Option<Option<i64>>,
}

pub(crate) async fn update_team(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(team_id): Path<Uuid>,
    ApiJson(r): ApiJson<UpdateTeamReq>,
) -> ApiResult<oxsum_core::Team> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .update_team(
            principal.organization().id,
            actor,
            team_id,
            r.name.as_deref(),
            r.budget_limit_minor,
        )
        .await?)
}

/// Dissolves a team: the members it grouped keep their seats with `team_id`
/// nulled — the member layer stays, the team cap simply stops applying.
pub(crate) async fn delete_team(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(team_id): Path<Uuid>,
) -> ApiResult<oxsum_core::Team> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .delete_team(principal.organization().id, actor, team_id)
        .await?)
}

/// The organization's active subscription, as `GET /api/v1/org/subscription`
/// answers it — the plan, the period window, what the period has consumed and
/// whether an exhausted quota may fall through to the wallet. `None` for an
/// organization on pay-as-you-go alone.
pub(crate) async fn subscription(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Option<oxsum_core::OrgSubscription>> {
    ok(state
        .db
        .active_subscription(principal.organization().id)
        .await?)
}

/// The subscription's period history — the audit a bill's `subscription` field
/// points back at, newest first.
pub(crate) async fn subscription_periods(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<oxsum_core::SubscriptionPeriod>> {
    let Some(subscription) = state
        .db
        .active_subscription(principal.organization().id)
        .await?
    else {
        return ok(Vec::new());
    };
    ok(state
        .db
        .subscription_periods(subscription.subscription_id)
        .await?)
}

pub(crate) async fn preview_invitation(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<oxsum_core::InvitationAcceptRequest>,
) -> ApiResult<oxsum_core::InvitationPreview> {
    ok(state.db.invitation_preview(&request.token).await?)
}
