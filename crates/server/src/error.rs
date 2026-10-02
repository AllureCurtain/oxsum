use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use oxsum_core::WalletError;
use serde_json::json;

/// HTTP-layer errors, formatted per docs/api.md.
#[derive(Debug)]
pub enum ApiError {
    Validation(String),
    Unauthorized,
    /// A login with an unknown email or a wrong password: 401 like any other failed
    /// credential, but with its own message — "missing or invalid API key" would be the
    /// wrong words here.
    InvalidCredentials,
    Forbidden(String),
    NotFound,
    Conflict(String),
    InsufficientFunds,
    /// The acting API key's spend limit is exhausted: settled charges plus outstanding
    /// holds attributed to the key would exceed it. A quota refusal, not a balance one.
    KeyLimitExceeded { limit_minor: i64, committed_minor: i64 },
    /// A feature the deployment did not configure: the wallet works, this surface does not.
    ServiceUnavailable(String),
    Internal,
}

impl From<WalletError> for ApiError {
    fn from(e: WalletError) -> Self {
        match e {
            WalletError::InvalidInput(m) => Self::Validation(m),
            WalletError::Unauthenticated => Self::Unauthorized,
            WalletError::InvalidCredentials => Self::InvalidCredentials,
            WalletError::Forbidden(m) => Self::Forbidden(m),
            WalletError::Conflict(m) => Self::Conflict(m),
            WalletError::HoldNotFound(_) => Self::NotFound,
            WalletError::InsufficientFunds => Self::InsufficientFunds,
            WalletError::KeyLimitExceeded {
                limit_minor,
                committed_minor,
            } => Self::KeyLimitExceeded {
                limit_minor,
                committed_minor,
            },
            // A deployment that cannot open its own channel credentials is broken rather than asked
            // something wrong: the operator gets the detail in the log, the caller gets a 500.
            WalletError::Misconfigured(detail) => {
                tracing::error!(%detail, "this deployment is misconfigured");
                Self::Internal
            }
            // Storage details go to the logs only, never back to the caller.
            WalletError::Storage(err) => {
                tracing::error!(error = %err, "storage failure");
                Self::Internal
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Validation(m) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", m),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                // One message for every way a key can fail: unknown, revoked, expired and
                // missing are indistinguishable to the caller, on purpose.
                "missing or invalid API key".into(),
            ),
            // One message for both ways a login can fail: an unknown email and a wrong
            // password are indistinguishable to the caller, on purpose.
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "invalid email or password".into(),
            ),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, "FORBIDDEN", m),
            Self::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", "not found".into()),
            Self::Conflict(m) => (StatusCode::CONFLICT, "CONFLICT", m),
            Self::InsufficientFunds => (
                StatusCode::PAYMENT_REQUIRED,
                "INSUFFICIENT_FUNDS",
                "insufficient funds".into(),
            ),
            Self::KeyLimitExceeded {
                limit_minor,
                committed_minor,
            } => (
                StatusCode::TOO_MANY_REQUESTS,
                "KEY_LIMIT_EXCEEDED",
                format!(
                    "this API key has committed {committed_minor} of its {limit_minor} minor-unit spend limit"
                ),
            ),
            Self::ServiceUnavailable(m) => {
                (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE", m)
            }
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "internal error".into(),
            ),
        };
        let body = json!({ "error": { "code": code, "message": message, "details": [] } });
        (status, Json(body)).into_response()
    }
}
