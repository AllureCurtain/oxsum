//! The Anthropic messages surface: `POST /v1/messages`.

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::response::Response;
use oxsum_core::{ActingKey, Organization};

use crate::AppState;
use crate::gateway::pipeline;
use crate::gateway::request::Surface;

/// One messages turn: freeze, relay, settle — Anthropic's surface.
pub(super) async fn messages(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    pipeline::gateway(
        Surface::Anthropic,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}
