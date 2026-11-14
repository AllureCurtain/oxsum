//! The `/metrics` scrape: gated on the operator token, and its counters move with traffic.
//!
//! Each `app()` carries its own registry (crates/server/src/metrics.rs), so a test that drove
//! a request can assert the exact count it caused rather than a shared process total.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// The operator token the apps in these tests are configured with.
const OPERATOR_TOKEN: &str = "operator-token-metrics-0123";

/// A pool that connects to nothing: auth refuses before the database is reached.
fn unused_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL")
}

/// An app over a real database, with the operator token configured.
async fn online_app(url: &str) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    (
        crate::support::app(
            db,
            Config::new(Signup::Open, None)
                .with_admin_token(OPERATOR_TOKEN)
                .with_secret(secret),
        ),
        pool,
    )
}

macro_rules! app_or_skip {
    () => {
        match oxsum_testkit::suite_url!() {
            Some(u) => online_app(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// One JSON call, the way the other suites make it.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(key) = key {
        request = crate::support::credential(request, key);
    }
    let request = request
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("the test's own request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A scrape of `/metrics`: status, and the exposition document as text.
async fn scrape(app: &Router, token: Option<&str>) -> (StatusCode, String) {
    let mut request = Request::builder().method("GET").uri("/metrics");
    if let Some(token) = token {
        request = crate::support::credential(request, token);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).expect("the request"))
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The exposition line whose name and labels all appear — Prometheus renders one sample per
/// line, so a line carrying every fragment is the series the assertion wants.
fn sample<'a>(document: &'a str, name: &str, labels: &[&str]) -> Option<&'a str> {
    document.lines().find(|line| {
        line.starts_with(name)
            && labels.iter().all(|label| line.contains(label))
            && !line.starts_with("#")
    })
}

/// The value a sample line reports (everything after the last space).
fn value(line: &str) -> f64 {
    line.rsplit(' ')
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .expect("a sample ends in a number")
}

/// Registers an organization and answers its first key's secret.
async fn register(app: &Router) -> (String, String) {
    let email = format!(
        "metrics_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    (
        body["data"]["apiKey"]["secret"]
            .as_str()
            .expect("registration returns a key")
            .to_owned(),
        crate::support::login(app, &email, "correct horse battery").await,
    )
}

/// Funds an organization through the manual rail, so its holds have something to freeze.
async fn top_up(app: &Router, key: &str, minor: i64) {
    let (status, body) = call(
        app,
        "POST",
        "/__test/funding",
        Some(json!({
            "idempotencyKey": format!("metrics-topup-{}", uuid::Uuid::new_v4().simple()),
            "amountMinor": minor,
        })),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the top-up failed: {body}");
}

#[tokio::test]
async fn the_scrape_answers_to_the_operator_token() {
    let app = crate::support::app(
        Db::from_pool(unused_pool()),
        Config::new(Signup::Open, None).with_admin_token(OPERATOR_TOKEN),
    );
    // No credential and a wrong credential both refuse, like every admin route.
    assert_eq!(scrape(&app, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        scrape(&app, Some("not-the-token")).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = scrape(&app, Some(OPERATOR_TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The exposition document explains itself: HELP lines and the oxsum prefix.
    assert!(body.contains("# HELP oxsum_http_requests_total"), "{body}");
    assert!(body.contains("# TYPE oxsum_http_requests_total"), "{body}");
}

#[tokio::test]
async fn a_scrape_counts_requests_and_reports_the_pool() {
    let (app, pool) = app_or_skip!();
    let model = crate::support::flat_model(
        &Db::from_pool(pool.clone()),
        &oxsum_core::SecretKey::from_bytes([7; 32]),
        1,
    )
    .await;
    let (key, _session) = register(&app).await;
    top_up(&app, &key, 100).await;

    // The health probe and a real gateway turn count under their route patterns.
    let (status, _) = call(&app, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, hold) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        Some(crate::support::chat_body(&model)),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hold}");

    let (status, body) = scrape(&app, Some(OPERATOR_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let health = sample(
        &body,
        "oxsum_http_requests_total",
        &["route=\"/healthz\"", "status=\"200\""],
    )
    .expect("the health request was counted");
    assert_eq!(value(health), 1.0, "{health}");
    let holds = sample(
        &body,
        "oxsum_http_requests_total",
        &["route=\"/v1/chat/completions\"", "status=\"200\""],
    )
    .expect("the gateway request was counted under its route pattern");
    assert_eq!(value(holds), 1.0, "{holds}");
    // The scrape-time gauges are there. Their values are the shared database's, so the
    // assertion is on presence — the gateway suite's scrape pins what they say.
    assert!(
        sample(&body, "oxsum_open_holds", &[]).is_some(),
        "the open-holds gauge is reported:\n{body}"
    );
    assert!(
        sample(&body, "oxsum_dead_holds", &[]).is_some(),
        "the dead-holds gauge is reported:\n{body}"
    );
    let pool = sample(&body, "oxsum_db_pool_connections", &["state=\"open\""])
        .expect("the pool gauge is reported");
    assert!(value(pool) >= 1.0, "{pool}");
}

/// The Solana rail's operator surface (issue #1145): a `solana-deposits`
/// pass refreshes the exception-queue and paid-but-uncredited gauges, and
/// the scrape carries the rail's series. A `confirmed` deposit — the chain
/// validated it, the ledger credit never landed — is exactly the alert's
/// subject.
#[tokio::test]
async fn the_solana_pass_refreshes_the_rails_operator_gauges() {
    let Some(u) = oxsum_testkit::suite_url!() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let (app, _events, metrics) = oxsum_server::app_with_billing(
        db.clone(),
        Config::new(Signup::Open, None)
            .with_admin_token(OPERATOR_TOKEN)
            .with_secret(oxsum_core::SecretKey::from_bytes([7; 32])),
    );
    let (_key, _session) = register(&app).await;

    // One organization, three deposits across the statuses an operator
    // works: a `confirmed` one (paid, uncredited — the alert), an `expired`
    // one, and a `pending` one the poll walks.
    let organization_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT organization_id FROM oxsum.organizations ORDER BY created_at LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("the registered organization exists");
    for (i, status) in ["confirmed", "expired", "pending"].iter().enumerate() {
        sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, status, meta) \
             VALUES ($1, $2, 'solana', $3, 1000, $4, '{}')",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(organization_id)
        .bind(format!("ref-{i}"))
        .bind(status)
        .execute(&pool)
        .await
        .expect("the deposit row inserts");
    }

    // Drive the pass the way the worker does: a due `solana-deposits` row
    // and a context carrying the app's own registry plus a stub chain.
    struct NoChain;
    impl std::fmt::Debug for NoChain {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("NoChain")
        }
    }
    impl oxsum_core::ChainLookup for NoChain {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Vec<String>, oxsum_core::LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a oxsum_core::ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            Option<oxsum_core::ValidatedTransfer>,
                            oxsum_core::LookupError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(None) })
        }
    }
    let verifier = oxsum_core::SolanaRailVerifier::new(
        std::sync::Arc::new(NoChain),
        oxsum_core::SolanaTerms::new(
            "payTo".into(),
            vec![oxsum_core::AssetTerms {
                key: "sol".into(),
                spl_token_mint: None,
                asset_decimals: 9,
                rate_micro: 200_000_000,
            }],
            "sol".into(),
            600,
            1,
            i64::MAX,
            0,
        ),
    );
    sqlx::query(
        "INSERT INTO oxsum.jobs (kind, run_at) \
         VALUES ('solana-deposits', now() - interval '1 second') \
         ON CONFLICT DO NOTHING",
    )
    .execute(&pool)
    .await
    .expect("the solana-deposits run is due");
    let ctx = oxsum_server::jobs::JobContext {
        hold_timeout: std::time::Duration::from_secs(60),
        metrics: metrics.clone(),
        webhook: None,
        retention: oxsum_core::Retention::default(),
        upstream_price_sync: false,
        witness: None,
        bill_provider: None,
        solana: Some(std::sync::Arc::new(verifier)),
    };
    oxsum_server::jobs::run_due(&db, &ctx).await;

    let (status, body) = scrape(&app, Some(OPERATOR_TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The confirmed deposit is the paid-but-uncredited alert's subject. The
    // exact sample line, not `sample`'s prefix match — `…_oldest_seconds`
    // shares the prefix and would shadow the count.
    let paid = body
        .lines()
        .find(|line| line.starts_with("solana_deposits_paid_uncredited "))
        .expect("the paid-uncredited gauge is reported");
    assert_eq!(value(paid), 1.0, "{paid}");
    let oldest = sample(&body, "solana_deposits_paid_uncredited_oldest_seconds", &[])
        .expect("the oldest-age gauge is reported");
    assert!(value(oldest) >= 0.0, "{oldest}");
    // The exception queue carries the real statuses the pass saw.
    let confirmed = sample(
        &body,
        "solana_exception_queue_depth",
        &["status=\"confirmed\""],
    )
    .expect("the confirmed depth is reported");
    assert_eq!(value(confirmed), 1.0, "{confirmed}");
    let expired = sample(
        &body,
        "solana_exception_queue_depth",
        &["status=\"expired\""],
    )
    .expect("the expired depth is reported");
    assert_eq!(value(expired), 1.0, "{expired}");
    // A status no row sits in still has a series, at zero.
    let flagged = sample(
        &body,
        "solana_exception_queue_depth",
        &["status=\"flagged\""],
    )
    .expect("the flagged depth is reported at zero");
    assert_eq!(value(flagged), 0.0, "{flagged}");
    // The pre-created screening series carries its HELP line on the first
    // scrape — a >0 alert has a metric to read before any hit lands. (The
    // RPC-failover series are describe-only until a real call emits a sample:
    // HELP lines only render for series with samples.)
    assert!(
        body.contains("# HELP solana_screening_hits_total"),
        "{body}"
    );
}

#[tokio::test]
async fn rate_limit_rejections_are_counted_by_surface() {
    let (app, pool) = app_or_skip!();
    let model = crate::support::flat_model(
        &Db::from_pool(pool.clone()),
        &oxsum_core::SecretKey::from_bytes([7; 32]),
        1,
    )
    .await;
    let (key, session) = register(&app).await;
    top_up(&app, &key, 100).await;

    // A key capped at one request a minute: the second gateway turn is refused.
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "metered", "requestsPerMinute": 1})),
        Some(&session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let metered = created["data"]["secret"]
        .as_str()
        .expect("the key's secret");
    for n in 1..=2 {
        let (status, _) = call(
            &app,
            "POST",
            "/v1/chat/completions",
            Some(crate::support::chat_body(&model)),
            Some(metered),
        )
        .await;
        if n == 1 {
            assert_eq!(status, StatusCode::OK);
        } else {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        }
    }

    let (_, body) = scrape(&app, Some(OPERATOR_TOKEN)).await;
    let rejected = sample(
        &body,
        "oxsum_rate_limit_rejections_total",
        &["surface=\"gateway\""],
    )
    .expect("the rejection was counted");
    assert_eq!(value(rejected), 1.0, "{rejected}");
}
