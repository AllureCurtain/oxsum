//! The OpenAI Responses surface: `POST /v1/responses`.

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::response::Response;
use oxsum_core::{ActingKey, Organization};

use crate::AppState;
use crate::gateway::pipeline;
use crate::gateway::request::Surface;

/// One Responses turn: freeze, relay, settle — OpenAI's successor wire (issue
/// #495). The same `openai` channels serve it; upstream is called at
/// `responses` rather than `chat/completions`.
pub(super) async fn responses(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    pipeline::gateway(
        Surface::Responses,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}
