//! The OpenAI chat-completions surface: `POST /v1/chat/completions`.

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::response::Response;
use oxsum_core::{ActingKey, Organization};

use crate::AppState;
use crate::gateway::pipeline;
use crate::gateway::request::Surface;

/// One chat completion: freeze, relay, settle — OpenAI's surface.
pub(super) async fn chat(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    pipeline::gateway(
        Surface::OpenAi,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}
