//! Errors on the OpenAI-compatible surface.
//!
//! An OpenAI SDK shows its caller the `error.message` of an error object, so `/v1/*` answers in
//! OpenAI's shape rather than oxsum's envelope: the same information, in the shape the client on
//! the other side already knows how to read. oxsum's own code travels in `error.code`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use oxsum_core::WalletError;
use serde_json::{Value, json};

/// The header every response carries, so a caller can find its bill without reading the body.
pub const REQUEST_ID: &str = "x-oxsum-request-id";

/// An error a gateway request can end in.
#[derive(Debug)]
pub enum GatewayError {
    /// The request itself is wrong: an unknown model, an image, a `max_tokens` of zero.
    Invalid {
        message: String,
        param: Option<&'static str>,
    },
    /// The wallet cannot cover the freeze, or refused the release.
    InsufficientFunds(String),
    /// No usable credential.
    Unauthorized,
    /// The credential is not allowed to do this. Unreachable until roles arrive; mapped rather
    /// than dropped so a future role check cannot silently turn into a 500.
    Forbidden(String),
    /// The request id was already used for a different turn. A caller cannot cause this today —
    /// the id is minted per request — so it maps the wallet's refusal rather than inviting it.
    Conflict(String),
    /// Upstream failed. `body` is upstream's own error object when it sent one.
    Upstream {
        status: StatusCode,
        message: String,
        body: Option<Value>,
    },
    /// Something in oxsum failed. The detail goes to the log, never to the caller.
    Internal,
}

impl GatewayError {
    /// The request names a model this deployment does not serve.
    pub fn model_not_served(model: &str) -> Self {
        Self::Invalid {
            message: format!("the model {model:?} is not served by this deployment"),
            param: Some("model"),
        }
    }

    /// The freeze is more than the wallet holds.
    ///
    /// The message says what it needs, what is there, and the one thing the caller can change; a
    /// payment error that does not state its price is a support ticket.
    pub fn freeze_refused(freeze: i64, available: Option<i64>, asked: Option<i64>) -> Self {
        let ceiling = match asked {
            Some(asked) => format!(", or ask for fewer than {asked} output tokens"),
            None => ", or set max_tokens".to_owned(),
        };
        let held = match available {
            Some(available) => format!("{available} minor units available"),
            None => "the balance could not be read".to_owned(),
        };
        Self::InsufficientFunds(format!(
            "this request freezes {freeze} minor units, and there are {held}: \
             lower max_tokens{ceiling}"
        ))
    }

    /// Upstream answered with an error, or could not be reached.
    pub fn upstream(status: StatusCode, message: String, body: Option<Value>) -> Self {
        Self::Upstream {
            status,
            message,
            body,
        }
    }
}

impl From<WalletError> for GatewayError {
    fn from(error: WalletError) -> Self {
        match error {
            WalletError::InvalidInput(message) => Self::Invalid {
                message,
                param: None,
            },
            WalletError::Unauthenticated => Self::Unauthorized,
            WalletError::Forbidden(message) => Self::Forbidden(message),
            WalletError::Conflict(message) => Self::Conflict(message),
            WalletError::InsufficientFunds => {
                Self::InsufficientFunds("the wallet cannot cover this request".to_owned())
            }
            // The gateway settles the hold it took itself in this turn; a hold it cannot find
            // is an internal inconsistency, not a caller error.
            WalletError::HoldNotFound(key) => {
                tracing::error!(%key, "gateway settled a hold the ledger does not have");
                Self::Internal
            }
            WalletError::Storage(error) => {
                tracing::error!(%error, "storage failure on the gateway path");
                Self::Internal
            }
            // A channel credential that does not open, or a key that is not there: the deployment is
            // broken, and the caller can do nothing about it but is told so honestly.
            WalletError::Misconfigured(detail) => {
                tracing::error!(%detail, "this deployment is misconfigured");
                Self::Internal
            }
        }
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        match self {
            Self::Invalid { message, param } => error_response(
                StatusCode::BAD_REQUEST,
                &message,
                "invalid_request_error",
                "VALIDATION_ERROR",
                param,
            ),
            Self::InsufficientFunds(message) => error_response(
                StatusCode::PAYMENT_REQUIRED,
                &message,
                "insufficient_quota",
                "INSUFFICIENT_FUNDS",
                None,
            ),
            Self::Unauthorized => error_response(
                StatusCode::UNAUTHORIZED,
                // One message for every way a key can fail: unknown, revoked, expired and missing
                // are indistinguishable to the caller, on purpose.
                "missing or invalid API key",
                "authentication_error",
                "UNAUTHORIZED",
                None,
            ),
            Self::Forbidden(message) => error_response(
                StatusCode::FORBIDDEN,
                &message,
                "permission_error",
                "FORBIDDEN",
                None,
            ),
            Self::Conflict(message) => error_response(
                StatusCode::CONFLICT,
                &message,
                "invalid_request_error",
                "CONFLICT",
                None,
            ),
            Self::Upstream {
                status,
                message,
                body,
            } => match body {
                // Upstream already speaks this format, so its error object is passed through rather
                // than paraphrased: the caller sees exactly what upstream said was wrong.
                Some(body) if body.get("error").is_some() => (status, Json(body)).into_response(),
                _ => error_response(status, &message, "api_error", "UPSTREAM_ERROR", None),
            },
            Self::Internal => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
                "api_error",
                "INTERNAL_ERROR",
                None,
            ),
        }
    }
}

/// The OpenAI error object.
fn error_response(
    status: StatusCode,
    message: &str,
    kind: &str,
    code: &str,
    param: Option<&'static str>,
) -> Response {
    let body = json!({
        "error": {
            "message": message,
            "type": kind,
            "param": param,
            "code": code,
        }
    });
    (status, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use http_body_util::BodyExt;

    async fn body_of(error: GatewayError) -> (StatusCode, Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collects the test's own body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).expect("the error body is JSON"),
        )
    }

    #[tokio::test]
    async fn errors_carry_the_openai_shape() {
        let (status, body) = body_of(GatewayError::model_not_served("nope")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
        assert_eq!(body["error"]["param"], "model");
        assert!(body["error"]["message"].as_str().unwrap().contains("nope"));
    }

    #[tokio::test]
    async fn a_refused_freeze_states_the_price_and_the_balance() {
        let (status, body) = body_of(GatewayError::freeze_refused(
            2_000_000,
            Some(500_000),
            Some(4_000),
        ))
        .await;
        assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
        assert_eq!(body["error"]["code"], "INSUFFICIENT_FUNDS");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains("2000000"), "{message}");
        assert!(message.contains("500000"), "{message}");
        assert!(message.contains("max_tokens"), "{message}");
    }

    #[tokio::test]
    async fn an_upstream_error_object_is_passed_through() {
        let upstream = json!({"error": {"message": "model overloaded", "type": "api_error"}});
        let (status, body) = body_of(GatewayError::upstream(
            StatusCode::BAD_GATEWAY,
            "upstream failed".to_owned(),
            Some(upstream.clone()),
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body, upstream);
    }

    #[tokio::test]
    async fn an_upstream_failure_without_a_body_is_generated() {
        let (status, body) = body_of(GatewayError::upstream(
            StatusCode::BAD_GATEWAY,
            "no route".to_owned(),
            None,
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"]["code"], "UPSTREAM_ERROR");
        assert_eq!(body["error"]["message"], "no route");
    }

    #[tokio::test]
    async fn a_storage_failure_says_nothing_about_storage() {
        let (status, body) = body_of(GatewayError::Internal).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "internal error");
    }
}
