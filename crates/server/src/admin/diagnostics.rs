//! Sanitized deployment diagnostics; configuration presence and local counts only.

use std::time::Duration;

use axum::extract::State;
use oxsum_core::{Db, DiagnosticCounts, DiagnosticSnapshot};
use serde::Serialize;

use crate::AppState;
use crate::routes::{ApiResult, ok};

/// Fixed configuration flags; no configured value leaves this endpoint.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Configuration {
    smtp_configured: bool,
    public_url_configured: bool,
    credential_seal_configured: bool,
    head_signing_configured: bool,
    stripe_configured: bool,
    airwallex_configured: bool,
    hyperswitch_configured: bool,
    x402_configured: bool,
}

/// A deployment diagnostic response whose fields are safe to retain for support.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Diagnostics {
    version: &'static str,
    git_sha: &'static str,
    schema_version: &'static str,
    database_state: &'static str,
    applied_schema_version: Option<String>,
    configuration: Configuration,
    counts: Option<DiagnosticCounts>,
}

/// Read one bounded local snapshot under the admin router's operator-only guard.
pub(crate) async fn read(State(state): State<AppState>) -> ApiResult<Diagnostics> {
    let snapshot =
        tokio::time::timeout(Duration::from_secs(6), state.db.diagnostic_snapshot()).await;
    let (database_state, applied_schema_version, counts) = match snapshot {
        Ok(Ok(DiagnosticSnapshot::Ready(counts))) => (
            "ready",
            Some(Db::latest_schema_version().to_owned()),
            Some(counts),
        ),
        Ok(Ok(DiagnosticSnapshot::Unmigrated)) => ("unmigrated", None, None),
        Ok(Ok(DiagnosticSnapshot::SchemaMismatch(version))) => {
            // The migration table is storage input, not permission to echo
            // arbitrary text into a support report.
            let version = version.filter(|value| {
                value.len() <= 120
                    && value.len() > 5
                    && value.as_bytes()[..4].iter().all(u8::is_ascii_digit)
                    && value.as_bytes()[4] == b'_'
                    && value.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            });
            ("schemaMismatch", version, None)
        }
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "operator diagnostic snapshot unavailable");
            ("unavailable", None, None)
        }
        Err(_) => {
            tracing::warn!("operator diagnostic snapshot timed out");
            ("unavailable", None, None)
        }
    };
    ok(Diagnostics {
        version: env!("CARGO_PKG_VERSION"),
        git_sha: env!("OXSUM_GIT_SHA"),
        schema_version: Db::latest_schema_version(),
        database_state,
        applied_schema_version,
        configuration: Configuration {
            smtp_configured: state.config.mailer().is_some(),
            public_url_configured: state.config.public_url().is_some(),
            credential_seal_configured: state.config.secret().is_some(),
            head_signing_configured: state.config.head_signing_seed().is_some(),
            stripe_configured: state.config.stripe().is_some(),
            airwallex_configured: state.config.airwallex().is_some(),
            hyperswitch_configured: state.config.hyperswitch().is_some(),
            x402_configured: state.config.x402().is_some(),
        },
        counts,
    })
}
