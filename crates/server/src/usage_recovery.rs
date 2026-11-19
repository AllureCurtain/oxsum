//! Bounded read-only provider lookup; no inference request is ever retried here.
use oxsum_core::usage_recovery::ObservedTextUsage;
use oxsum_core::{Db, SecretKey, Tenants, WalletError};
use oxsum_integrations::provider_http::ProviderHttp;
use std::time::Duration;
use uuid::Uuid;

/// Walk at most sixteen pending operations per regular hold-sweep pass.
pub async fn recover_due(
    db: &Db,
    tenants: &Tenants,
    secret: Option<&SecretKey>,
) -> Result<(), WalletError> {
    for operation in db.due_usage_recovery(16).await? {
        if let Err(error) = recover_one(db, tenants, operation, secret).await {
            tracing::warn!(%error,%operation,"usage recovery remains pending");
        }
    }
    Ok(())
}
/// Immediate closure or durable deferral. The gateway passes no key to avoid blocking a response on GET.
pub async fn recover_one(
    db: &Db,
    tenants: &Tenants,
    operation: Uuid,
    secret: Option<&SecretKey>,
) -> Result<Option<i64>, WalletError> {
    let Some(claim) = db.claim_usage_recovery(operation).await? else {
        return Ok(None);
    };
    let mut unavailable = !claim.needs_lookup;
    let mut report = None;
    if claim.needs_lookup
        && let Some(secret) = secret
    {
        match db.recovery_lookup(&claim, secret).await? {
            None => unavailable = true,
            Some(context) => {
                let result =
                    lookup(&ProviderHttp::default(), &context, Duration::from_secs(10)).await;
                match result {
                    Some(Ok(observed)) => report = Some(observed),
                    Some(Err(())) => unavailable = true,
                    None => {}
                }
            }
        }
    }
    db.finish_usage_recovery(tenants, &claim, report.as_ref(), unavailable)
        .await
}

async fn lookup(
    http: &ProviderHttp,
    context: &oxsum_core::usage_recovery::RecoveryLookup,
    timeout: Duration,
) -> Option<Result<ObservedTextUsage, ()>> {
    tokio::time::timeout(timeout, async {
        let request = http
            .request(
                reqwest::Method::GET,
                &context.base_url,
                &context.policy,
                &["chat", "completions", &context.completion_id],
            )
            .await
            .ok()?;
        let mut response = request
            .bearer_auth(&context.api_key)
            .timeout(timeout)
            .send()
            .await
            .ok()?;
        if matches!(response.status().as_u16(), 400 | 401 | 403 | 404 | 410) {
            return Some(Err(()));
        }
        if !response.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Some(Err(()));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let observed = ObservedTextUsage::parse(&value);
        if observed.completion_id.as_deref() != Some(&context.completion_id) {
            return Some(Err(()));
        }
        Some(Ok(observed))
    })
    .await
    .ok()
    .flatten()
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::routing::any;
    use serde_json::json;
    #[tokio::test]
    async fn stored_completion_get_is_correlated_bounded_and_never_posts() {
        let app=Router::new().fallback(any(|method:Method,uri:Uri,headers:HeaderMap|async move {
            assert_eq!(method,Method::GET);assert_eq!(headers["authorization"],"Bearer fixture-key");let id=uri.path().rsplit('/').next().unwrap();
            match id {
                "chatcmpl-unauthorized"=>StatusCode::UNAUTHORIZED.into_response(),
                "chatcmpl-transient"=>StatusCode::TOO_MANY_REQUESTS.into_response(),
                "chatcmpl-large"=>Response::new(Body::from("x".repeat(4*1024*1024+1))),
                "chatcmpl-slow"=>{tokio::time::sleep(Duration::from_millis(300)).await;StatusCode::OK.into_response()},
                _=>axum::Json(json!({"id":if id=="chatcmpl-wrong"{"chatcmpl-other"}else{id},"object":"chat.completion","choices":[{"index":0,"message":{"content":"hidden response body"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}})).into_response()
            }
        }));
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", socket.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(socket, app).await.unwrap();
        });
        let mut context = oxsum_core::usage_recovery::RecoveryLookup {
            base_url: base,
            policy: oxsum_core::ProviderDestinationPolicy {
                network: oxsum_core::ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            },
            api_key: "fixture-key".into(),
            completion_id: "chatcmpl-ok".into(),
        };
        let http = ProviderHttp::default();
        let report = lookup(&http, &context, Duration::from_secs(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.input, Some(3));
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("hidden response")
        );
        for id in ["chatcmpl-unauthorized", "chatcmpl-wrong", "chatcmpl-large"] {
            context.completion_id = id.into();
            assert!(
                lookup(&http, &context, Duration::from_secs(1))
                    .await
                    .unwrap()
                    .is_err()
            );
        }
        context.completion_id = "chatcmpl-transient".into();
        assert!(
            lookup(&http, &context, Duration::from_secs(1))
                .await
                .is_none()
        );
        context.completion_id = "chatcmpl-slow".into();
        assert!(
            lookup(&http, &context, Duration::from_millis(30))
                .await
                .is_none()
        );
        task.abort();
    }
}
