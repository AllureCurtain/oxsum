//! Contract and HTTP authority boundaries for complete customer statement reads.
#[path = "../../../core/tests/support/application_statements.rs"]
mod fixture;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::IntegrationScope;
use serde_json::Value;
use tower::ServiceExt;

async fn get(
    app: &axum::Router,
    path: &str,
    bearer: Option<&str>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().uri(path);
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("oxsum_session={cookie}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}
#[tokio::test]
async fn statement_routes_confine_machine_authority_and_report_complete_or_refused_documents() {
    let Some(w) = fixture::world("application_statements_http").await else {
        return;
    };
    let id = fixture::statement(&w, "issued", 2, 10).await;
    let app = oxsum_server::app(
        w.db.clone(),
        oxsum_server::Config::new(oxsum_server::Signup::Invite, None),
    );
    let root = format!("/api/v1/integration/customers/{}/statements", w.org);
    for path in [root.clone(), format!("{root}/{id}")] {
        assert_eq!(
            get(&app, &path, None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&app, &path, None, Some(&w.owner)).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body) = get(&app, &path, Some(&w.token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.to_string().contains("businessId"));
    }
    let detail = get(&app, &format!("{root}/{id}"), Some(&w.token), None)
        .await
        .1;
    assert_eq!(detail["data"]["lines"].as_array().unwrap().len(), 2);
    assert_eq!(detail["data"]["totalMinor"], 2);
    let (narrow, _) =
        fixture::credential(&w.db, w.app, vec![IntegrationScope::CustomersRead]).await;
    assert_eq!(
        get(&app, &root, Some(&narrow), None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&app, &format!("{root}?limit=51"), Some(&w.token), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(
            &app,
            &format!("{root}?after={}", uuid::Uuid::new_v4()),
            Some(&w.token),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let open = fixture::statement(&w, "open", 1, 1).await;
    assert_eq!(
        get(&app, &format!("{root}/{open}"), Some(&w.token), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let large = fixture::statement(&w, "issued", 1001, 1).await;
    assert_eq!(
        get(&app, &format!("{root}/{large}"), Some(&w.token), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
}
