//! The input-only embeddings surface: `POST /v1/embeddings`.

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::HeaderMap;
use axum::response::Response;
use oxsum_core::{ActingKey, Organization};

use crate::AppState;
use crate::gateway::pipeline;
use crate::gateway::request::Surface;

/// One embeddings call: freeze the input bound, relay, settle — the first of
/// the input-only surfaces (issue #170).
pub(super) async fn embeddings(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    pipeline::gateway(
        Surface::Embeddings,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}
