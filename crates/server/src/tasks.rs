//! The task relay's route layer (issue #503): `POST /v1/tasks`,
//! `GET /v1/tasks/{taskId}`, `POST /v1/tasks/{taskId}/cancel`.
//!
//! A task is a turn on a minutes timescale: submit freezes an upper bound and
//! relays `{baseUrl}/tasks`, the `poll-tasks` periodic pass asks
//! `{baseUrl}/tasks/{id}` for the still-open rows and settles the hold on the
//! terminal answer, and cancel asks upstream to stop the job and releases the
//! hold either way. The caller's `GET` is a row read — upstream is asked only
//! inside the pass, so a hundred polls cost one upstream question each tick.
//!
//! The surface serves only channels whose `protocol` is `tasks`: a model no
//! `tasks` channel serves is refused the same `model_not_served` a chat
//! request for an unserved model takes.

use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use oxsum_core::{
    ActingKey, NewTask, Organization, Task, TaskKind, TaskOutcome, TaskState, fingerprint,
    hold_description, route_order,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::auth::require_key_gateway;
use crate::gateway::error::{BALANCE_MINOR, FREEZE_MINOR, GatewayError, REQUEST_ID};
use crate::today;

/// The task surface, nested under `/v1` by [`crate::routes`].
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/tasks", post(submit))
        .route("/tasks/{task_id}", get(poll).post(cancel_placeholder))
        .route("/tasks/{task_id}/cancel", post(cancel))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_key_gateway,
        ))
}

/// `POST /v1/tasks` — never reached: a bare POST to the read path is a client
/// mistake, answered 405 by the method router before this runs.
async fn cancel_placeholder() -> Response {
    GatewayError::Invalid {
        message: "use POST /v1/tasks/{taskId}/cancel".to_owned(),
        param: None,
    }
    .into_response()
}

/// `POST /v1/tasks` — submit a task: freeze the declared bound, relay
/// `{baseUrl}/tasks`, record `external_task_id`, answer `{taskId, state}`.
async fn submit(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Extension(key): Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let body: Value = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(error) => {
            return error_response(
                GatewayError::Invalid {
                    message: format!("the request body must be JSON: {error}"),
                    param: None,
                },
                &request_id,
            );
        }
    };
    let Some(map) = body.as_object() else {
        return error_response(
            GatewayError::Invalid {
                message: "the request body must be a JSON object".to_owned(),
                param: None,
            },
            &request_id,
        );
    };
    match submit_inner(&state, &organization, &key, map, &request_id, &headers).await {
        Ok((response, task)) => {
            // The claim completes with the answer it produced: a retry under
            // the same key replays this body rather than submitting twice.
            if let Some(key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
                let _ = state
                    .db
                    .complete_request(organization.id, key, 200, &response_json_body(&task))
                    .await;
            }
            response
        }
        Err(error) => {
            // A refused submit frees the claim so a corrected retry can take
            // it — the refusal carried no state worth replaying.
            if let Some(key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
                let _ = state.db.release_request(organization.id, key).await;
            }
            error_response(error, &request_id)
        }
    }
}

/// The submission's work, split so every early refusal lands in the gateway
/// envelope with the request id on it. The task comes back with the response
/// so the caller can record the idempotency claim's answer.
async fn submit_inner(
    state: &AppState,
    organization: &Organization,
    key: &ActingKey,
    body: &serde_json::Map<String, Value>,
    request_id: &str,
    headers: &HeaderMap,
) -> Result<(Response, Task), GatewayError> {
    let kind = match body.get("kind").and_then(Value::as_str) {
        Some(raw) => TaskKind::parse(raw)?,
        None => {
            return Err(GatewayError::Invalid {
                message: "kind is required: image, video, batch or custom".to_owned(),
                param: Some("kind"),
            });
        }
    };
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| GatewayError::Invalid {
            message: "model is required".to_owned(),
            param: Some("model"),
        })?;
    // The idempotency claim rides the body fingerprint the gateway surfaces
    // use: a retry under the same key and body replays the stored answer.
    let idem_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(key) = &idem_key {
        match state
            .db
            .claim_request(
                organization.id,
                key,
                &fingerprint(&serde_json::to_vec(body).unwrap_or_default()),
                request_id,
            )
            .await
        {
            Ok(oxsum_core::Claim::Fresh { .. }) => {}
            Ok(oxsum_core::Claim::InFlight) => {
                return Err(GatewayError::idempotency_in_flight());
            }
            Ok(oxsum_core::Claim::Mismatch) => {
                return Err(GatewayError::idempotency_mismatch());
            }
            Ok(oxsum_core::Claim::Replay {
                request_id: stored,
                status,
                body,
            }) => {
                let response = replay(
                    status,
                    serde_json::to_vec(&body).unwrap_or_default(),
                    &stored,
                );
                // A replay answers the stored body — the task read is only
                // for the record; a task the replay predates reads nothing.
                let task = state
                    .db
                    .task(organization.id, stored.parse().unwrap_or_default())
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(dummy_task);
                return Ok((response, task));
            }
            Err(error) => return Err(error.into()),
        }
    }
    // The candidates: channels serving this model under the `tasks` protocol,
    // weighted-ordered the way a chat turn's routes are — a refused submission
    // fails over to the next route's channel under the same hold.
    let servings = state
        .db
        .servings_named(model, state.config.secret().ok_or(GatewayError::Internal)?)
        .await?
        .0
        .into_iter()
        .filter(|serving| serving.protocol == oxsum_core::TASKS)
        .collect::<Vec<_>>();
    if servings.is_empty() {
        return Err(GatewayError::model_not_served(model));
    }
    let ordered = route_order(servings, rand::random::<f64>());
    let primary = ordered
        .first()
        .ok_or_else(|| GatewayError::model_not_served(model))?;

    // The freeze: the caller's declared bound when it names one (`maxCostMinor`),
    // else the channel price's flat `cost_per_request` — a task's bound is a
    // fixed figure, not a token estimate, because upstream's units are not yet
    // known at submit.
    let declared = body
        .get("maxCostMinor")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0);
    let freeze = match declared {
        Some(amount) => amount,
        None => primary.price.cost_per_request.unwrap_or(0),
    };
    if freeze <= 0 {
        return Err(GatewayError::Invalid {
            message: "maxCostMinor must be a positive minor amount".to_owned(),
            param: Some("maxCostMinor"),
        });
    }

    let task_id = Uuid::new_v4();
    let hold_key = format!("req-{task_id}:hold");
    let description = hold_description(&task_id.to_string(), model, freeze)?;
    let wallet = state.tenants.get(&organization.tenant_id).await?;

    // The watch row before the hold: a row without a hold heals itself — the
    // sweeper deletes it — while a hold without a row would be invisible to
    // the sweeper if this process died.
    state
        .db
        .note_open_hold(&oxsum_core::OpenHold {
            hold_key: hold_key.clone(),
            tenant_id: organization.tenant_id.clone(),
            request_id: task_id.to_string(),
            model: model.to_owned(),
            channel: primary.channel.clone(),
            price_version: primary.version,
            input_price: primary.price.input_price_per_million,
            output_price: primary.price.output_price_per_million,
            freeze_minor: freeze,
            key_id: Some(key.key_id),
            mandate_id: None,
            end_user: body.get("user").and_then(Value::as_str).map(str::to_owned),
            service_tier: None,
            tags: Default::default(),
            sweep_attempts: 0,
            last_error: None,
            dead_at: None,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "recording the task's open hold failed");
            GatewayError::Internal
        })?;
    if let Err(error) = wallet
        .hold_for_key(
            key,
            Some(model),
            &hold_key,
            &description,
            freeze,
            today(),
            None,
        )
        .await
    {
        if let Err(clear) = state.db.clear_open_hold(&hold_key).await {
            tracing::error!(%clear, "clearing a refused task hold's watch row failed");
        }
        return Err(match error {
            oxsum_core::WalletError::InsufficientFunds => {
                let available = wallet.available().await.ok();
                GatewayError::freeze_refused(freeze, available, None)
            }
            other => other.into(),
        });
    }

    // Relay `POST {baseUrl}/tasks` with the pool's lead credential. A refusal
    // before any task id comes back releases the hold and refuses the submit —
    // the caller's retry submits again, which is correct because upstream
    // never accepted anything.
    let forwarded = json!({
        "kind": kind.as_str(),
        "model": model,
        "input": body.get("input").cloned().unwrap_or(Value::Null),
        "params": body.get("params").cloned().unwrap_or(Value::Null),
    });
    let credential = primary.credentials.first();
    let Some(credential) = credential else {
        release_task_hold(state, &hold_key).await;
        return Err(GatewayError::upstream(
            StatusCode::BAD_GATEWAY,
            "every upstream credential is cooling or disabled".to_owned(),
            None,
        ));
    };
    let upstream = match state
        .http
        .post(format!("{}/tasks", primary.base_url))
        .bearer_auth(&credential.api_key)
        .json(&forwarded)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            release_task_hold(state, &hold_key).await;
            return Err(GatewayError::upstream(
                StatusCode::BAD_GATEWAY,
                format!("upstream is unreachable: {error}"),
                None,
            ));
        }
    };
    if !upstream.status().is_success() {
        let status = upstream.status();
        let text = upstream.text().await.unwrap_or_default();
        release_task_hold(state, &hold_key).await;
        return Err(GatewayError::upstream(
            StatusCode::BAD_GATEWAY,
            format!("upstream answered {status}: {}", preview(&text)),
            serde_json::from_str::<Value>(&text)
                .ok()
                .filter(|body| body.get("error").is_some()),
        ));
    }
    let upstream_body: Value = upstream.json().await.map_err(|error| {
        GatewayError::upstream(
            StatusCode::BAD_GATEWAY,
            format!("upstream's submit answer was not JSON: {error}"),
            None,
        )
    })?;
    let Some(external_task_id) = upstream_body
        .get("id")
        .or_else(|| upstream_body.get("taskId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        release_task_hold(state, &hold_key).await;
        return Err(GatewayError::upstream(
            StatusCode::BAD_GATEWAY,
            "upstream's submit answer carried no task id".to_owned(),
            None,
        ));
    };

    let task = state
        .db
        .insert_task(&NewTask {
            task_id,
            organization_id: organization.id,
            key_id: Some(key.key_id),
            channel_id: primary.channel_id,
            channel: primary.channel.clone(),
            external_task_id,
            kind,
            hold_key: hold_key.clone(),
            freeze_minor: freeze,
            model: model.to_owned(),
        })
        .await?;

    let balance = wallet.available().await.ok();
    Ok((
        task_response(&task, request_id, Some(freeze), balance),
        task,
    ))
}

/// `GET /v1/tasks/{taskId}` — the caller's poll is a row read, never a second
/// upstream call: upstream is asked only inside the `poll-tasks` pass, so the
/// answer is whatever the last pass (or the submit, or the cancel) left.
async fn poll(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Extension(_key): Extension<ActingKey>,
    Path(task_id): Path<Uuid>,
) -> Response {
    match state.db.task(organization.id, task_id).await {
        Ok(Some(task)) => task_response(&task, &task_id.to_string(), None, None),
        Ok(None) => GatewayError::Invalid {
            message: format!("no task {task_id}"),
            param: Some("taskId"),
        }
        .into_response(),
        Err(error) => {
            tracing::error!(%error, %task_id, "reading a task failed");
            GatewayError::Internal.into_response()
        }
    }
}

/// `POST /v1/tasks/{taskId}/cancel` — best-effort: ask upstream to stop the
/// job, then release the hold at zero whether upstream accepted or not — the
/// billing side ends because the caller said the work is no longer wanted.
async fn cancel(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Extension(_key): Extension<ActingKey>,
    Path(task_id): Path<Uuid>,
) -> Response {
    let task = match state.db.task(organization.id, task_id).await {
        Ok(Some(task)) => task,
        Ok(None) => {
            return GatewayError::Invalid {
                message: format!("no task {task_id}"),
                param: Some("taskId"),
            }
            .into_response();
        }
        Err(error) => {
            tracing::error!(%error, %task_id, "reading a task to cancel failed");
            return GatewayError::Internal.into_response();
        }
    };
    // A terminal task is already settled: cancel answers it back unchanged.
    if task.state.is_terminal() {
        return task_response(&task, &task_id.to_string(), None, None);
    }
    // Ask upstream to stop the job — best-effort, because a refused cancel
    // still ends the billing side. The poll pass will not see the task again.
    let secret = match state.config.secret() {
        Some(secret) => secret,
        None => return GatewayError::Internal.into_response(),
    };
    let api_key = match task_credentials(&state, &task, secret).await {
        Ok(key) => key,
        Err(error) => {
            tracing::warn!(%error, %task_id, "the task's credential would not open; cancelling without upstream");
            None
        }
    };
    if let Some(api_key) = api_key {
        let _ = state
            .http
            .post(format!(
                "{}/tasks/{}/cancel",
                task_base(&state, &task).await,
                task.external_task_id
            ))
            .bearer_auth(api_key)
            .send()
            .await;
    }
    // Release the hold at zero and walk the row to `cancelled` — the settle
    // is a `released` settlement, distinct from `swept`.
    let outcome = TaskOutcome {
        state: TaskState::Cancelled,
        usage: None,
        result_json: None,
        error_json: Some(json!({ "message": "cancelled by the caller" })),
        usage_json: None,
    };
    match oxsum_core::settle_task(
        &state.db,
        &state.tenants,
        &task,
        organization.id,
        &outcome,
        today(),
    )
    .await
    {
        Ok(_) | Err(oxsum_core::WalletError::Conflict(_)) => {}
        Err(error) => {
            tracing::error!(%error, %task_id, "the task cancel's settle failed");
            return GatewayError::Internal.into_response();
        }
    }
    // Read the row back: the settle updated it terminal.
    match state.db.task(organization.id, task_id).await {
        Ok(Some(task)) => task_response(&task, &task_id.to_string(), None, None),
        _ => GatewayError::Internal.into_response(),
    }
}

/// The task's channel's upstream base — read at use time so a poll or cancel
/// never carries a stale directory entry the row was written with.
async fn task_base(state: &AppState, task: &Task) -> String {
    state
        .db
        .channel_base(task.channel_id)
        .await
        .unwrap_or_default()
}

/// One healthy credential for the task's channel, opened under the deployment
/// secret — the poll and the cancel share it.
async fn task_credentials(
    state: &AppState,
    task: &Task,
    secret: &oxsum_core::SecretKey,
) -> Result<Option<String>, oxsum_core::WalletError> {
    state.db.task_channel_key(task.channel_id, secret).await
}

/// Releases a task's hold when the submission itself fails before upstream
/// accepted anything: settle at zero with kind `released`, and clear the
/// watch row so the sweeper never sees it.
async fn release_task_hold(state: &AppState, hold_key: &str) {
    // The sweeper owns a hold nobody settles; a submit that never reached
    // upstream leaves no usage record, so the release is a plain ledger
    // settle at zero plus the watch row's delete.
    let tenant_id = match state.db.tenant_of_hold(hold_key).await {
        Ok(Some(tenant_id)) => tenant_id,
        _ => {
            let _ = state.db.clear_open_hold(hold_key).await;
            return;
        }
    };
    let Ok(wallet) = state.tenants.get(&tenant_id).await else {
        let _ = state.db.clear_open_hold(hold_key).await;
        return;
    };
    let description = "task submit failed before upstream accepted".to_owned();
    let _ = wallet.settle(hold_key, &description, 0, today()).await;
    let _ = state.db.clear_open_hold(hold_key).await;
}

/// The task row as the wire spells it — camelCase, `taskId` first.
fn task_response(
    task: &Task,
    request_id: &str,
    freeze: Option<i64>,
    balance: Option<i64>,
) -> Response {
    let mut response = Json(json!({
        "taskId": task.task_id,
        "state": task.state.as_str(),
        "kind": task.kind.as_str(),
        "model": task.model,
        "channel": task.channel,
        "submittedAt": task.submitted_at,
        "completedAt": task.completed_at,
        "result": task.result_json,
        "error": task.error_json,
        "usage": task.usage_json,
        "chargedMinor": task.charged_minor,
    }))
    .into_response();
    let headers = response.headers_mut();
    if let Ok(value) = axum::http::HeaderValue::from_str(request_id) {
        headers.insert(REQUEST_ID, value);
    }
    if let Some(freeze) = freeze
        && let Ok(value) = axum::http::HeaderValue::from_str(&freeze.to_string())
    {
        headers.insert(FREEZE_MINOR, value);
    }
    if let Some(balance) = balance
        && let Ok(value) = axum::http::HeaderValue::from_str(&balance.to_string())
    {
        headers.insert(BALANCE_MINOR, value);
    }
    response
}

/// The gateway error envelope with the request id on it.
fn error_response(error: GatewayError, request_id: &str) -> Response {
    let mut response = error.into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID, value);
    }
    response
}

/// A replayed idempotent answer — the stored body under the stored status,
/// marked `Idempotent-Replayed`.
fn replay(status: i32, body: Vec<u8>, request_id: &str) -> Response {
    let mut response = Response::new(axum::body::Body::from(body));
    *response.status_mut() =
        StatusCode::from_u16(u16::try_from(status).unwrap_or(200)).unwrap_or(StatusCode::OK);
    let headers = response.headers_mut();
    headers.insert(
        "idempotent-replayed",
        axum::http::HeaderValue::from_static("true"),
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(request_id) {
        headers.insert(REQUEST_ID, value);
    }
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response
}

/// The first 200 characters of an upstream error body — what a 502 message
/// quotes, bounded so a runaway error page cannot grow the answer.
fn preview(text: &str) -> String {
    text.chars().take(200).collect()
}

/// A never-reached task for a replayed claim — the response is the stored
/// body, so the `Task` the tuple carries is a placeholder the caller never
/// reads. `Default` does not exist on `Task`, so the fill is explicit.
fn dummy_task() -> Task {
    Task {
        task_id: Uuid::nil(),
        organization_id: Uuid::nil(),
        key_id: None,
        channel_id: Uuid::nil(),
        channel: String::new(),
        external_task_id: String::new(),
        kind: TaskKind::Custom,
        state: TaskState::Submitted,
        hold_key: String::new(),
        freeze_minor: 0,
        charged_minor: None,
        model: String::new(),
        result_json: None,
        error_json: None,
        usage_json: None,
        submitted_at: time::OffsetDateTime::UNIX_EPOCH,
        completed_at: None,
    }
}

/// The JSON body an `Ok` submit response carries — reconstructed from the
/// task the response already serialized, so the idempotency record stores the
/// same shape the client saw.
fn response_json_body(task: &Task) -> Value {
    json!({
        "taskId": task.task_id,
        "state": task.state.as_str(),
        "kind": task.kind.as_str(),
        "model": task.model,
        "channel": task.channel,
        "submittedAt": task.submitted_at,
        "chargedMinor": task.charged_minor,
    })
}
