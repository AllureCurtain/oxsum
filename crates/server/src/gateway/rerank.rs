//! The input-only rerank surface: `POST /v1/rerank`.

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::response::Response;
use oxsum_core::{ActingKey, Organization};

use crate::AppState;
use crate::gateway::pipeline;
use crate::gateway::request::Surface;

/// One rerank call: freeze the input bound, relay, settle — the second
/// input-only surface, in the Jina/Cohere shape (issue #170).
pub(super) async fn rerank(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    pipeline::gateway(
        Surface::Rerank,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}
