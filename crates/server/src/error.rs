use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, OptionalFromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use oxsum_core::WalletError;
use serde::de::DeserializeOwned;
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
    /// Nothing to answer with, or nothing the caller's organization has. The message says
    /// which, because the caller was authorized to ask (docs/api.md, membership management).
    NotFound(String),
    Conflict(String),
    InsufficientFunds,
    /// The acting API key's spend limit is exhausted: settled charges plus outstanding
    /// holds attributed to the key would exceed it. A quota refusal, not a balance one.
    KeyLimitExceeded {
        limit_minor: i64,
        committed_minor: i64,
    },
    /// The acting API key's rolling-minute request allowance is used up. The answer
    /// carries `Retry-After` and the `X-RateLimit-*` headers, so a client can back
    /// off without guessing.
    RateLimited {
        limit: i64,
        retry_after_secs: u64,
    },
    /// The acting API key's outstanding-holds cap is reached.
    TooManyHolds {
        limit: i64,
        open: i64,
    },
    /// A feature the deployment did not configure: the wallet works, this surface does not.
    ServiceUnavailable(String),
    Internal,
}

impl ApiError {
    /// A resource the caller named that this organization does not have. One answer for
    /// "never existed" and "not yours", so an id cannot be probed.
    pub(crate) fn not_found() -> Self {
        Self::NotFound("not found".into())
    }
}

impl From<WalletError> for ApiError {
    fn from(e: WalletError) -> Self {
        match e {
            WalletError::InvalidInput(m) => Self::Validation(m),
            WalletError::Unauthenticated => Self::Unauthorized,
            WalletError::InvalidCredentials => Self::InvalidCredentials,
            WalletError::Forbidden(m) => Self::Forbidden(m),
            WalletError::Conflict(m) => Self::Conflict(m),
            WalletError::HoldNotFound(_) => Self::NotFound("not found".into()),
            // A resource the caller named and does not have (an unknown account, a user who
            // is not a member): the message is about their own organization, so it is shown.
            WalletError::NotFound(message) => Self::NotFound(message),
            WalletError::InsufficientFunds => Self::InsufficientFunds,
            WalletError::KeyLimitExceeded {
                limit_minor,
                committed_minor,
            } => Self::KeyLimitExceeded {
                limit_minor,
                committed_minor,
            },
            WalletError::RateLimited {
                limit,
                retry_after_secs,
            } => Self::RateLimited {
                limit,
                retry_after_secs,
            },
            WalletError::TooManyHolds { limit, open } => Self::TooManyHolds { limit, open },
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

/// Every way axum's own [`Json`] extractor can refuse a body is the caller's mistake, so it gets the
/// envelope's 400 rather than axum's plain-text answer: a missing or wrong `content-type` (415), a
/// body that is not JSON (400) and a body that is JSON but not this shape (422) are one status here,
/// which is the one the contract declares (issue #51).
impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        // axum's text names what was wrong with the body — "Expected request with
        // `Content-Type: application/json`", "Failed to parse the request body as JSON: EOF while
        // parsing a value at line 1 column 10" — which is exactly what a caller needs to hear.
        Self::Validation(rejection.body_text())
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
            Self::NotFound(m) => (StatusCode::NOT_FOUND, "NOT_FOUND", m),
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
            Self::RateLimited {
                limit,
                retry_after_secs,
            } => {
                let body = json!({ "error": {
                    "code": "RATE_LIMITED",
                    "message": format!(
                        "this API key is limited to {limit} requests per minute"
                    ),
                    "details": [],
                } });
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [
                        ("retry-after", retry_after_secs.max(1).to_string()),
                        ("x-ratelimit-limit", limit.to_string()),
                        ("x-ratelimit-remaining", "0".to_owned()),
                        ("x-ratelimit-reset", retry_after_secs.max(1).to_string()),
                    ],
                    Json(body),
                )
                    .into_response();
            }
            Self::TooManyHolds { limit, open } => (
                StatusCode::TOO_MANY_REQUESTS,
                "TOO_MANY_HOLDS",
                format!("this API key has {open} holds outstanding, at or over its cap of {limit}"),
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

/// The JSON body of an `/api/v1` request: [`Json`], with axum's rejection replaced by [`ApiError`].
///
/// Use this instead of `Json` for every request body on the envelope's surface, so that a body this
/// deployment cannot read is answered in the format the surface documents throughout (issue #51).
/// Responses keep `Json`; only extraction differs.
///
/// `Option<ApiJson<T>>` is the optional-body form and keeps axum's rule: a request that sends no
/// `content-type` at all is a request with no body, while one that names a type other than JSON is a
/// rejection. That is what makes `POST /api/v1/org/keys` without a body still mint a plain key.
pub(crate) struct ApiJson<T>(pub(crate) T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = <Json<T> as FromRequest<S>>::from_request(request, state).await?;
        Ok(Self(value))
    }
}

impl<T, S> OptionalFromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Option<Self>, Self::Rejection> {
        // axum's own optional extraction decides what "no body" means, and it is only the
        // rejection that changes.
        match <Json<T> as OptionalFromRequest<S>>::from_request(request, state).await {
            Ok(Some(Json(value))) => Ok(Some(Self(value))),
            Ok(None) => Ok(None),
            Err(rejection) => Err(rejection.into()),
        }
    }
}
