//! Unauthenticated operational endpoints: liveness, readiness and the
//! deployment's own version identity.
//!
//! None of these sit behind a credential: a load balancer's health check and an
//! operator's "what is running" question arrive before any account exists, and
//! they carry nothing an attacker could not learn from the login page.
//! `security: []` in openapi.yaml.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::AppState;

/// What `GET /api/v1/version` reports: the package version the binary was built
/// as, the commit it was built from (`unknown` when the source was a tarball)
/// and the newest schema version its migration runner carries.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Version {
    version: &'static str,
    git_sha: &'static str,
    schema_version: &'static str,
}

/// `GET /api/v1/version` — unauthenticated. A deployment's own name-tag: the
/// values say nothing about its tenants, and the install/upgrade runbook reads
/// them to confirm a pull or rollback landed the intended build.
pub(crate) async fn version() -> Response {
    axum::Json(serde_json::json!({
        "data": Version {
            version: env!("CARGO_PKG_VERSION"),
            git_sha: env!("OXSUM_GIT_SHA"),
            schema_version: oxsum_core::Db::latest_schema_version(),
        }
    }))
    .into_response()
}

/// `GET /readyz` — readiness, not liveness.
///
/// Answers 200 only when the database answers *and* its migration ledger has
/// run to the schema this binary serves. A freshly-created database answers
/// 503 until `oxsum-migrate` has run, which is what keeps the compose
/// `healthcheck` honest: an up-but-unmigrated deployment is up, not ready.
/// 200 carries the applied schema version; 503 carries a short reason.
pub(crate) async fn readyz(State(state): State<AppState>) -> Response {
    match state.db.applied_schema_version().await {
        Ok(Some(applied)) if applied == oxsum_core::Db::latest_schema_version() => {
            (StatusCode::OK, format!("ready ({applied})")).into_response()
        }
        Ok(applied) => {
            let at = applied.as_deref().unwrap_or("unmigrated");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "schema at {at}, this binary serves {}",
                    oxsum_core::Db::latest_schema_version()
                ),
            )
                .into_response()
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "database unreachable").into_response(),
    }
}
