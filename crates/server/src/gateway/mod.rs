//! The protocol-native gateway: `POST /v1/chat/completions`, `POST /v1/messages`,
//! `POST /v1/embeddings`, `POST /v1/rerank`, `GET /v1/models` — and, on the
//! versioned path a Gemini client spells, `POST
//! /v1beta/models/{model}:generateContent` and `:streamGenerateContent`.
//!
//! A request is frozen before upstream is contacted and settled after it answers, so the wallet
//! never has to trust the network. The order is the promise: the hold is taken first, upstream
//! second, the settlement third, and each step has one place where it happens. The POST
//! surfaces run that same pipeline in their own wire dialect — OpenAI's, Anthropic's,
//! Gemini's, or the input-only embeddings/rerank shapes — and each serves only the channels
//! that declare its protocol, priced under the surface's own billing mode.

mod chat;
mod embeddings;
pub(crate) mod error;
mod evidence;
mod messages;
mod models;
mod pipeline;
mod relay;
mod request;
mod rerank;
mod responses;

use axum::routing::{get, post};
use axum::{Router, middleware};

pub(crate) use pipeline::{MANDATE_HEADER, client};

use crate::AppState;

/// The `/v1` surface. Nested under `/v1` by [`crate::routes`], which supplies the state.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/models", get(models::models))
        .route("/chat/completions", post(chat::chat))
        .route("/responses", post(responses::responses))
        .route("/messages", post(messages::messages))
        .route("/embeddings", post(embeddings::embeddings))
        .route("/rerank", post(rerank::rerank))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_key_gateway,
        ))
}

/// The Gemini surface (issue #494): `/v1beta/models/{model}:generateContent`
/// and `:streamGenerateContent`, nested under `/v1beta` by [`crate::routes`].
/// axum allows one capture per path segment, and a Gemini route spells the
/// model and the colon-action in one segment, so the wildcard takes both and
/// the handler splits them.
pub fn beta_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/models/{*target}", post(pipeline::gemini_generate))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_key_gateway,
        ))
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::config::{Config, Signup};
    use axum::body::Body;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;
    use oxsum_core::Db;
    use serde_json::Value;
    use tower::ServiceExt;

    /// An app with no channel configured: the wallet is served, the gateway is not.
    fn wallet_only_app() -> Router {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused")
            .expect("the placeholder URL parses");
        crate::app(Db::from_pool(pool), Config::new(Signup::Invite, None))
    }

    #[tokio::test]
    async fn a_request_without_a_key_is_refused_in_openais_shape() {
        // The key is resolved before the database is reached, so this needs neither a connection
        // nor a channel: what it checks is that `/v1` answers in the format an OpenAI SDK parses.
        let response = wallet_only_app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .expect("the test's own request"),
            )
            .await
            .expect("the router answers");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("collects the body")
            .to_bytes();
        let body: Value = serde_json::from_slice(&body).expect("the error body is JSON");
        assert_eq!(body["error"]["type"], "authentication_error");
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }
}
