//! Monthly statement routes enforce scoped personal authority and stable document bytes.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
async fn get(
    app: &Router,
    path: &str,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().uri(path);
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    if let Some(bearer) = bearer {
        req = req.header("authorization", format!("Bearer {bearer}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    (
        status,
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap(),
    )
}
#[tokio::test]
async fn monthly_document_endpoints_are_bounded_scoped_and_read_only() {
    let Some(url) = oxsum_testkit::suite_db_url("monthly_statements_http") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "monthly HTTP fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let personal = db
        .login(&customer.user.email, "monthly HTTP fixture")
        .await
        .unwrap()
        .into_session()
        .token;
    let statement = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.statements(statement_id,organization_id,period,status,payment_status,total_minor,credit_drawn_minor,paid_minor,payment_terms_days,due_date,entry_count) VALUES($1,$2,'2026-09','finalized','pending',1000,700,200,14,'2026-10-01',1)").bind(statement).bind(customer.organization.id).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.statement_lines(statement_id,line_no,channel,model,turns,amount_minor) VALUES($1,1,'local','monthly',1,1000)").bind(statement).execute(db.pool()).await.unwrap();
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let path = format!(
        "/api/v1/business/customers/{}/monthly-statements",
        customer.organization.id
    );
    let detail = format!("{path}/{statement}");
    for path in [&path, &detail] {
        for (cookie, bearer, status) in [
            (None, None, StatusCode::UNAUTHORIZED),
            (Some(personal.as_str()), None, StatusCode::FORBIDDEN),
            (Some(owner.as_str()), Some(""), StatusCode::FORBIDDEN),
            (
                Some(owner.as_str()),
                Some("oxs-int-fake"),
                StatusCode::FORBIDDEN,
            ),
            (
                Some(owner.as_str()),
                Some(customer.api_key.secret.as_str()),
                StatusCode::FORBIDDEN,
            ),
        ] {
            assert_eq!(get(&app, path, cookie, bearer).await.0, status);
        }
    }
    let (status, page) = get(
        &app,
        &format!("{path}?limit=1&period=2026-09"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["data"]["statements"][0]["id"], json!(statement));
    assert!(page["data"]["nextCursor"].is_null());
    let (status, body) = get(&app, &detail, Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["statement"]["dueDate"], "2026-10-01");
    assert_eq!(body["data"]["statement"]["paymentStatus"], "overdue");
    assert_eq!(body["data"]["statement"]["outstandingMinor"], 500);
    assert_eq!(body["data"]["lines"][0]["amountMinor"], 1000);
    for query in [
        "limit=0",
        "limit=101",
        "after=no-uuid",
        "period=2999-01",
        "extra=oops",
    ] {
        assert_eq!(
            get(&app, &format!("{path}?{query}"), Some(&owner), None)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let foreign = format!(
        "/api/v1/business/customers/{}/monthly-statements/{statement}",
        Uuid::new_v4()
    );
    assert_eq!(
        get(&app, &foreign, Some(&owner), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(
            &app,
            &format!("{path}/{}", Uuid::new_v4()),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let stored: String =
        sqlx::query_scalar("SELECT payment_status FROM oxsum.statements WHERE statement_id=$1")
            .bind(statement)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored, "pending");
    let exists: bool = sqlx::query_scalar("SELECT to_regnamespace($1) IS NOT NULL")
        .bind(format!("ledger_{}", customer.organization.tenant_id))
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!exists);
}
