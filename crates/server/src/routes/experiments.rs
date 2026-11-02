//! `/api/v1/org/experiments`: the A/B variant sets a request may opt into
//! (issue #509). A member reads the list and the results; the writes — create
//! or replace the variant set, end the experiment — are owner/admin actions
//! the membership role gates, the same rule `teams` keeps. An experiment is
//! mutable config (a binding the operator edits), not ledger, so `PUT` writes
//! the whole set rather than appending.

use axum::extract::{Extension, Path, State};
use serde::Deserialize;

use oxsum_core::{Experiment, Principal, Role, Variant, VariantResult};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};

/// The body of `PUT /org/experiments/{name}`: the whole variant set,
/// `[{model, weight}]` — replaced atomically.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PutExperimentReq {
    variants: Vec<Variant>,
}

/// The organization role a write needs — the same gate every org-structure
/// write keeps: an experiment is config, not ledger, but it steers which model
/// a request pays for, so the write borrows the membership rules' first check
/// rather than adding a second role vocabulary for it (org.rs, `update_display`).
/// The check is a person's action: an API key names no role and is refused by
/// `membership_actor` before the rule runs.
fn writer(principal: &Principal) -> Result<(), ApiError> {
    match principal.membership_actor()?.role {
        Role::Owner | Role::Admin => Ok(()),
        Role::Member => Err(ApiError::Forbidden(
            "only an owner or an admin may change an experiment".into(),
        )),
    }
}

/// `GET /org/experiments` — every experiment of the organization, open and
/// ended, oldest first.
pub(crate) async fn list_experiments(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<Experiment>> {
    ok(state
        .db
        .list_experiments(principal.organization().id)
        .await?)
}

/// `PUT /org/experiments/{name}` — create or replace one experiment's variant
/// set. The served-model check runs in core: a variant naming an unserved
/// model is refused before it lands.
pub(crate) async fn put_experiment(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    ApiJson(r): ApiJson<PutExperimentReq>,
) -> ApiResult<Experiment> {
    writer(&principal)?;
    ok(state
        .db
        .put_experiment(principal.organization().id, &name, &r.variants)
        .await?)
}

/// `POST /org/experiments/{name}/end` — stop serving the experiment: a later
/// request under it resolves the plain model. 404 when no such experiment or
/// it already ended.
pub(crate) async fn end_experiment(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> ApiResult<()> {
    writer(&principal)?;
    if !state
        .db
        .end_experiment(principal.organization().id, &name)
        .await?
    {
        return Err(ApiError::NotFound("experiment not found".into()));
    }
    ok(())
}

/// `GET /org/experiments/{id}/results` — the usage the experiment served,
/// grouped by variant: turns, token and charge sums, errored count. Answers an
/// ended experiment too — its recorded spend still groups.
pub(crate) async fn experiment_results(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Vec<VariantResult>> {
    let org = principal.organization().id;
    // The id must belong to this org — a foreign id sees no rows, but the
    // honest answer is 404, not an empty set that reads as "served nothing".
    if state.db.experiment_by_id(org, id).await?.is_none() {
        return Err(ApiError::NotFound("experiment not found".into()));
    }
    ok(state.db.experiment_results(org, id).await?)
}
