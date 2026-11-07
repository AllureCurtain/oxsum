//! `/api/v1/admin/organizations/{organizationId}/groups`: the request groups a
//! key may join (issue #508).
//!
//! A group names the subset of an organization's channels its keys may serve
//! through — the team-level isolation a single org cannot otherwise express.
//! Mutable config; deletion refuses while keys remain assigned.

use axum::extract::{Path, State};
use oxsum_core::audit_action;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::admin::record;
use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};

/// One group as the admin API answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupView {
    id: Uuid,
    name: String,
    channel_ids: Vec<Uuid>,
    channels: Vec<GroupChannelView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupChannelView {
    id: Uuid,
    name: String,
}

impl From<oxsum_core::Group> for GroupView {
    fn from(group: oxsum_core::Group) -> Self {
        let channels = group
            .channels
            .into_iter()
            .map(|channel| GroupChannelView {
                id: channel.id,
                name: channel.name,
            })
            .collect();
        Self {
            id: group.id,
            name: group.name,
            channel_ids: group.channel_ids,
            channels,
        }
    }
}

/// The channel set a `PUT` installs, in one write.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GroupChannels {
    channel_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupsQuery {
    organization_id: Uuid,
}

/// `GET /api/v1/admin/groups?organizationId=` — an organization's groups.
pub(crate) async fn list_groups(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<GroupsQuery>,
) -> ApiResult<Vec<GroupView>> {
    let organization_id = q.organization_id;
    let groups = state.db.groups(organization_id).await?;
    ok(groups.into_iter().map(GroupView::from).collect())
}

/// `PUT /api/v1/admin/organizations/{organizationId}/groups/{name}` — create or
/// replace a group's channel set atomically.
///
/// Every `channelId` must name a real channel of the deployment — a group
/// pointing at a channel that does not exist would scope keys to a serving
/// that can never answer, so the write refuses rather than land it.
pub(crate) async fn set_group(
    State(state): State<AppState>,
    Path((organization_id, name)): Path<(Uuid, String)>,
    ApiJson(request): ApiJson<GroupChannels>,
) -> ApiResult<GroupView> {
    let group = state
        .db
        .put_group(organization_id, &name, &request.channel_ids)
        .await?;
    record(
        &state,
        audit_action::GROUP_SET,
        Some(format!("{organization_id}/{name}")),
        serde_json::json!({ "channelIds": request.channel_ids }),
        None,
    )
    .await?;
    ok(GroupView::from(group))
}

/// `DELETE /api/v1/admin/organizations/{organizationId}/groups/{name}` — drop a
/// group; 404 when the organization has none of that name. The keys it held
/// keep their assignment; an assigned group returns 409.
pub(crate) async fn delete_group(
    State(state): State<AppState>,
    Path((organization_id, name)): Path<(Uuid, String)>,
) -> ApiResult<serde_json::Value> {
    if !state.db.delete_group(organization_id, &name).await? {
        return Err(ApiError::not_found());
    }
    record(
        &state,
        audit_action::GROUP_DELETE,
        Some(format!("{organization_id}/{name}")),
        serde_json::json!({}),
        None,
    )
    .await?;
    ok(serde_json::json!({ "deleted": true }))
}
