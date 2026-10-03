//! HTTP tests for the dashboard: the Leptos pages are mounted into the same binary as
//! the API, and `/ws/billing` pushes billing progress to a logged-in browser in real time.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use oxsum_core::{Db, OpenHold};
use oxsum_server::{BillingEvent, Config, Signup};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// The wasm-bindgen glue and the wasm module, relative to the site root: the names
/// `cargo leptos build` writes into `<site-root>/pkg`, taken from `output-name` in
/// `[[workspace.metadata.leptos]]`.
///
/// wasm-bindgen emits `oxsum_bg.wasm` and cargo-leptos 0.3.11 renames it to
/// `oxsum.wasm` ("for backward compatibility with leptos' `HydrationScripts`"), which is
/// the name the shell's markup must ask for (issue #65).
const GLUE: &str = "pkg/oxsum.js";
const WASM: &str = "pkg/oxsum.wasm";

/// An app over a real database with oxsum's tables migrated, plus the broadcast sender
/// the gateway publishes billing events to.
async fn online_app(url: &str) -> (Router, Db, broadcast::Sender<BillingEvent>) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None);
    let (app, billing) = oxsum_server::app_with_billing(db.clone(), config);
    (app, db, billing)
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => online_app(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
    set_cookie: Option<String>,
}

/// One plain HTTP call against the app.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let set_cookie = headers
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        headers,
        body,
        set_cookie,
    }
}

/// Registers a fresh organization; returns the tenant id and the login cookie.
async fn logged_in(app: &Router, name: &str) -> (String, String) {
    let email = format!(
        "{name}_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let res = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "registration failed: {}",
        res.body
    );
    let tenant_id = res.body["data"]["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();
    let res = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "login failed: {}", res.body);
    let cookie = res
        .set_cookie
        .as_deref()
        .expect("a cookie was set")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned();
    (tenant_id, cookie)
}

/// The overview payload, the way the page's own code asks for it: the
/// `/_pages/get_dashboard` server function, with the session cookie, answered as
/// `DashboardData` JSON.
///
/// Leptos suffixes a server function's URL with a hash of the crate and module it was
/// declared in, so the path is read from the same registry the router registers its
/// server-function routes from rather than hard-coded — that hash carries the absolute
/// path of the checkout.
async fn dashboard(app: &Router, cookie: &str) -> Value {
    let (path, method) = leptos::server_fn::axum::server_fn_paths()
        .find(|(path, _)| path.starts_with("/_pages/get_dashboard"))
        .expect("the dashboard's overview server function is registered");
    // No arguments to send: the function takes none, and the codec is the URL-encoded
    // body, so the body is empty and the cookie is the credential.
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", format!("oxsum_session={cookie}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("the dashboard answers JSON")
}

/// The dashboard socket refuses a request with no session: a probe learns nothing.
#[tokio::test]
async fn billing_socket_refuses_a_missing_session() {
    let (app, _db, _billing) = app_or_skip!();
    let res = call(&app, "GET", "/ws/billing", None, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

/// The dashboard socket refuses a session it does not know, the same way.
#[tokio::test]
async fn billing_socket_refuses_an_unknown_session() {
    let (app, _db, _billing) = app_or_skip!();
    let res = call(
        &app,
        "GET",
        "/ws/billing",
        None,
        Some("oxsess-not-a-session"),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

/// The pages are mounted into the same binary as the API: the login page renders.
#[tokio::test]
async fn the_login_page_is_served() {
    let (app, _db, _billing) = app_or_skip!();
    let res = call(&app, "GET", "/login", None, None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let content_type = res
        .headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/html"),
        "the login page is HTML, got {content_type}"
    );
}

/// The dashboard shell renders server-side; the session guard itself runs in the browser.
#[tokio::test]
async fn the_dashboard_shell_is_served() {
    let (app, _db, _billing) = app_or_skip!();
    let res = call(&app, "GET", "/dashboard", None, None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

/// An organization that has moved no money owes nothing and holds nothing: the frozen
/// total and this month's spend are 0, not an error. The ledger is created on this first
/// read and has no postings at all.
#[tokio::test]
async fn a_new_organization_has_nothing_frozen_and_no_spend() {
    let (app, _db, _billing) = app_or_skip!();
    let (_tenant_id, cookie) = logged_in(&app, "untouched").await;
    let data = dashboard(&app, &cookie).await;
    assert_eq!(data["availableMinor"], 0, "{data}");
    assert_eq!(data["frozenMinor"], 0, "{data}");
    assert_eq!(data["monthSpendMinor"], 0, "{data}");
}

/// The overview's two figures, pinned as integers in minor units: the frozen total is what
/// the outstanding hold reserved, and this month's spend is what the settled charge
/// booked. The top-up is neither — it credits the wallet — and the hold that was settled
/// no longer counts as frozen.
#[tokio::test]
async fn the_dashboard_shows_the_frozen_total_and_this_months_spend() {
    let (app, _db, _billing) = app_or_skip!();
    let (_tenant_id, cookie) = logged_in(&app, "totals").await;

    // Fund the wallet: 5 credits in. Every `/api/v1` endpoint takes the session cookie as
    // well as an API key, so the moves below need no second credential.
    let res = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "t1", "amountMinor": 5_000_000})),
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // One hold left outstanding: 1 credit frozen.
    let res = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": "h1", "amountMinor": 1_000_000})),
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // A second hold, settled for less than it froze: 0.5 credit charged.
    let res = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": "h2", "amountMinor": 2_000_000})),
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(
        &app,
        "POST",
        "/api/v1/settlements",
        Some(json!({"holdKey": "h2", "actualMinor": 500_000})),
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    let data = dashboard(&app, &cookie).await;
    // 5 credits topped up, 1 still frozen, 0.5 charged: 5 - 1 - 0.5 = 3.5 available.
    assert_eq!(data["availableMinor"], 3_500_000, "{data}");
    assert_eq!(data["frozenMinor"], 1_000_000, "{data}");
    assert_eq!(data["monthSpendMinor"], 500_000, "{data}");
}

/// The static bundle is served from the site root: `cargo leptos build` writes the
/// WASM/CSS to `<site-root>/pkg`, and the shell references `/pkg/*` — without the pkg
/// route the pages render but never hydrate. `web::mount` wires this route with the
/// same two `leptos_axum` helpers, so this pins the serving behavior they provide.
#[tokio::test]
async fn the_pkg_bundle_is_served_from_the_site_root() {
    let site = std::env::temp_dir().join(format!("oxsum-pkg-{}", std::process::id()));
    let pkg = site.join("pkg");
    std::fs::create_dir_all(&pkg).expect("creates the fake site dir");
    std::fs::write(pkg.join("probe.txt"), "bundle-bytes").expect("writes the fake bundle");

    let options = leptos::config::LeptosOptions::builder()
        .output_name("oxsum")
        .site_root(site.to_str().expect("the temp dir is UTF-8"))
        .build();
    let app = Router::new().route_service(
        &leptos_axum::site_pkg_dir_service_route_path(&options),
        leptos_axum::site_pkg_dir_service(&options),
    );
    let req = Request::builder()
        .uri("/pkg/probe.txt")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    std::fs::remove_dir_all(&site).ok();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"bundle-bytes");
}

/// One GET whose body is kept as bytes: the server-rendered pages are HTML, and
/// `/pkg/*` is not text at all, so `call`'s JSON body does not fit.
async fn get_raw(app: &Router, path: &str) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("the request is valid");
    let res = app.clone().oneshot(req).await.expect("the app answers");
    let status = res.status();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("the body is readable")
        .to_bytes()
        .to_vec();
    (status, bytes)
}

/// The workspace root: this crate sits at `<root>/crates/server`.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/server sits under the workspace root")
        .to_path_buf()
}

/// The shell's markup asks for the wasm module the built site holds, under the name
/// `cargo leptos build` writes it.
///
/// leptos decides that name when *leptos* is compiled: `HydrationScripts` appends `_bg`
/// unless `LEPTOS_OUTPUT_NAME` is set (`leptos-0.8.21/src/hydration/mod.rs`), and only
/// `cargo leptos build`/`serve` used to set it — for the server half as well as the wasm
/// half. A server built by plain `cargo run -p oxsum-server`, the command
/// `docs/development.md` documents, therefore asked for `/pkg/oxsum_bg.wasm`, while the
/// site holds `pkg/oxsum.wasm`: the import 404'd and every page stayed exactly as the
/// server had rendered it (issue #65). `.cargo/config.toml` sets `LEPTOS_OUTPUT_NAME` for
/// every cargo invocation in the workspace, and this test binary is such a build — which
/// is what makes this assertion cover the command a developer actually runs.
#[tokio::test]
async fn the_shell_asks_for_the_wasm_file_the_built_site_holds() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("the placeholder URL parses");
    let app = oxsum_server::app(Db::from_pool(pool), Config::new(Signup::Open, None));

    let (status, body) = get_raw(&app, "/login").await;
    assert_eq!(status, StatusCode::OK, "GET /login answered {status}");
    let html = String::from_utf8_lossy(&body).into_owned();

    // The `_bg` suffix is leptos's compile-time fallback: the file it names is not in the
    // site, so this is the assertion that fails when the name drifts back (issue #65).
    assert!(
        !html.contains("_bg.wasm"),
        "GET /login asks for a `_bg` wasm module: leptos appends that suffix when \
         LEPTOS_OUTPUT_NAME is unset while it is compiled, and it is compiled into this \
         server. The built site holds {WASM} instead, so the file the markup names \
         answers 404 and the page never hydrates (issue #65)"
    );
    for asset in [GLUE, WASM] {
        assert!(
            html.contains(asset),
            "GET /login never references {asset}, so the browser has nothing to load"
        );
    }

    // With a site on disk — `cargo leptos build` writes it to the workspace's `target/site`,
    // which is the site root the server resolves (`site-root` in the workspace Cargo.toml) —
    // the markup names a file the pkg route really serves: the browser's entire contract,
    // in one more request.
    if workspace_root().join("target/site").join(WASM).exists() {
        let (status, body) = get_raw(&app, &format!("/{WASM}")).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "/{WASM} is in the built site, but the pkg route answered {status}"
        );
        assert!(!body.is_empty(), "/{WASM} answered 200 with an empty body");
    }
}

/// A logged-in socket gets a snapshot of the in-flight holds first, then its
/// organization's live events — and never another organization's.
#[tokio::test]
async fn billing_socket_sends_a_snapshot_then_live_events() {
    let (app, db, billing) = app_or_skip!();
    let (tenant_id, cookie) = logged_in(&app, "socket").await;

    // One hold in flight before the socket connects: the snapshot must carry it. The key
    // is unique per run: the tests share one database.
    let hold_key = format!("req-snapshot-{}:hold", uuid::Uuid::new_v4().simple());
    db.note_open_hold(&OpenHold {
        hold_key: hold_key.clone(),
        tenant_id: tenant_id.clone(),
        request_id: "snapshot-1".to_owned(),
        model: "mock-model".to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        input_price: 1,
        output_price: 2,
        freeze_minor: 1000,
    })
    .await
    .expect("the hold is watched");

    // The socket needs a real TCP upgrade, so the app serves on a loopback port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a loopback port");
    let addr = listener.local_addr().expect("the port is known");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serves the app");
    });
    let request = Request::builder()
        .uri(format!("ws://{addr}/ws/billing"))
        .header("host", addr.to_string())
        .header("cookie", format!("oxsum_session={cookie}"))
        // A hand-built upgrade: the headers a browser's WebSocket sends.
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header(
            "sec-websocket-key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header("sec-websocket-version", "13")
        .body(())
        .expect("the upgrade request");
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("the socket upgrades");

    // The first message is the snapshot: the hold that was already in flight.
    let snapshot: Value = next_json(&mut ws).await;
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["holds"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["holds"][0]["holdKey"], hold_key);
    assert_eq!(snapshot["holds"][0]["freezeMinor"], 1000);

    // A live event for this organization is forwarded.
    billing
        .send(BillingEvent::TurnProgress {
            tenant_id: tenant_id.clone(),
            request_id: "snapshot-1".to_owned(),
            output_chars: 2048,
        })
        .expect("the broadcast has a receiver");
    let event: Value = next_json(&mut ws).await;
    assert_eq!(event["type"], "turnProgress");
    assert_eq!(event["tenantId"], tenant_id);
    assert_eq!(event["requestId"], "snapshot-1");
    assert_eq!(event["outputChars"], 2048);

    // Another organization's event never reaches this socket.
    billing
        .send(BillingEvent::TurnStarted {
            tenant_id: "someone-else".to_owned(),
            request_id: "other-1".to_owned(),
            model: "mock-model".to_owned(),
            channel: "mock".to_owned(),
            freeze_minor: 500,
        })
        .expect("the broadcast has a receiver");
    let nothing = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        nothing.is_err(),
        "another organization's event must not arrive"
    );
}

/// The next text message on the socket, parsed as JSON.
async fn next_json(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let message = ws
        .next()
        .await
        .expect("the socket stays open")
        .expect("the message arrives");
    let Message::Text(text) = message else {
        panic!("the dashboard only sends text messages");
    };
    serde_json::from_str(&text).expect("the message is JSON")
}
