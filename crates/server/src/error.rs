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
    Forbidden(String),
    NotFound,
    Conflict(String),
    InsufficientFunds,
    Internal,
}

impl From<WalletError> for ApiError {
    fn from(e: WalletError) -> Self {
        match e {
            WalletError::InvalidInput(m) => Self::Validation(m),
            WalletError::Unauthenticated => Self::Unauthorized,
            WalletError::Forbidden(m) => Self::Forbidden(m),
            WalletError::Conflict(m) => Self::Conflict(m),
            WalletError::HoldNotFound(_) => Self::NotFound,
            WalletError::InsufficientFunds => Self::InsufficientFunds,
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
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, "FORBIDDEN", m),
            Self::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", "not found".into()),
            Self::Conflict(m) => (StatusCode::CONFLICT, "CONFLICT", m),
            Self::InsufficientFunds => (
                StatusCode::PAYMENT_REQUIRED,
                "INSUFFICIENT_FUNDS",
                "insufficient funds".into(),
            ),
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
