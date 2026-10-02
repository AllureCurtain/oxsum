use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use crate::error::ApiError;

/// A single shared Bearer token, fine for the demo stage. Per-tenant keys are a later task; see TODO.md.
#[derive(Clone)]
pub struct ApiToken(Arc<str>);

impl ApiToken {
    pub fn new(token: impl Into<Arc<str>>) -> Self {
        Self(token.into())
    }

    fn matches(&self, presented: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), presented.as_bytes())
    }
}

pub async fn require_token(
    State(token): State<ApiToken>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(t) if token.matches(t) => Ok(next.run(request).await),
        _ => Err(ApiError::Unauthorized),
    }
}

/// Returns false immediately on length mismatch (length is not a secret); otherwise compares byte by byte in constant time.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn compares_bytes() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret!"));
    }
}
