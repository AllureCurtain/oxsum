use axum::extract::{Extension, Path, State};
use serde::Deserialize;
use uuid::Uuid;

use oxsum_core::{
    CreatedInvitation, Member, NewUser, Organization, Ownership, Principal, Registration, Role,
    UserOrganization,
};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::auth::{require_bot_check, send_verification};
use crate::routes::{ApiResult, SessionInfo, double_option_i64, ok, person};

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
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<DeleteAccountReq>,
) -> ApiResult<oxsum_core::AccountDeletion> {
    let principal = person(principal)?;
    let deletion = state
        .db
        .delete_account(principal.user.id, &request.password)
        .await?;
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

/// Mints an invitation link into the session's organization: the token is answered
/// once, here. Owner or admin only — the same rule every membership write follows.
pub(crate) async fn create_invitation(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<CreatedInvitation> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .create_invitation(actor, principal.organization().id)
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

/// What `POST /api/v1/auth/verify/request` answers: whether a verification mail
/// went out, or that the address was verified already.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddMemberReq {
    email: String,
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

pub(crate) async fn add_member(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<AddMemberReq>,
) -> ApiResult<Member> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .add_member(principal.organization().id, actor, &r.email)
        .await?)
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
