//! HTTP tests for the bills page and its two exports.
//!
//! What the page promises is a list of the organization's settled entries with their
//! content hashes, a cost in credits and a date; what the exports promise is that same
//! list as a file a browser saves, with the same fields in both and the cost as the
//! ledger's integer in minor units. These tests seed
//! settlements through the wallet API — the holds and settlements the page must list, and
//! the top-ups and outstanding holds it must not — then read the page and both downloads.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// An app over a real database with oxsum's tables migrated.
async fn online_app(url: &str) -> (Router, Db) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (
        oxsum_server::app(db.clone(), Config::new(Signup::Open, None)),
        db,
    )
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

/// One response, with the body kept as bytes: the exports are files, not JSON envelopes.
struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("the body is text")
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }
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
    Res {
        status: res.status(),
        headers: res.headers().clone(),
        body: res.into_body().collect().await.unwrap().to_bytes().to_vec(),
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
        res.json()
    );
    let tenant_id = res.json()["data"]["organization"]["tenantId"]
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
    assert_eq!(res.status, StatusCode::OK, "login failed: {}", res.json());
    let cookie = res
        .header("set-cookie")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned();
    (tenant_id, cookie)
}

/// One settled entry, as the API that wrote it named it, plus the receipts of the two
/// entries beside it: the hold it released and the top-up that funded it — neither of
/// which is a settled entry, so neither belongs on the bills page.
struct Seed {
    entry_id: String,
    content_hash: String,
    charged_minor: i64,
    hold_hash: String,
    top_up_hash: String,
}

/// Tops up, holds and settles through the wallet API — the page's own record of a
/// settled entry, without the gateway.
async fn seed(app: &Router, cookie: &str, tag: &str, held: i64, charged: i64) -> Seed {
    let res = call(
        app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": format!("bills-{tag}:topup"), "amountMinor": held + 1_000})),
        Some(cookie),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the top-up lands: {}",
        res.json()
    );
    let top_up_hash = res.json()["data"]["contentHash"]
        .as_str()
        .expect("the top-up receipt carries its content hash")
        .to_owned();

    let hold_key = format!("bills-{tag}:hold");
    let res = call(
        app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": hold_key, "amountMinor": held})),
        Some(cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "the hold lands: {}", res.json());
    let hold_hash = res.json()["data"]["contentHash"]
        .as_str()
        .expect("the hold receipt carries its content hash")
        .to_owned();

    let res = call(
        app,
        "POST",
        "/api/v1/settlements",
        Some(json!({"holdKey": hold_key, "actualMinor": charged})),
        Some(cookie),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the settlement lands: {}",
        res.json()
    );
    Seed {
        entry_id: res.json()["data"]["entryId"]
            .as_str()
            .expect("the settlement receipt names its entry")
            .to_owned(),
        content_hash: res.json()["data"]["contentHash"]
            .as_str()
            .expect("the settlement receipt carries its content hash")
            .to_owned(),
        charged_minor: charged,
        hold_hash,
        top_up_hash,
    }
}

/// The CSV export's header, which is also its field list.
const CSV_HEADER: &str = "bookedOn,entryId,chargedMinor,contentHash";

/// The page lists the organization's settled entries — and only those — with their
/// content hashes, and both exports carry the same rows with the same fields.
#[tokio::test]
async fn the_bills_page_lists_settled_entries_and_the_exports_carry_them() {
    let (app, _db) = app_or_skip!();
    let (_tenant_id, cookie) = logged_in(&app, "bills").await;

    let charged = seed(&app, &cookie, "a", 400, 316).await;
    let free = seed(&app, &cookie, "b", 100, 0).await;
    // The posting date is the server's current UTC date (crates/server/AGENTS.md).
    let today = time::OffsetDateTime::now_utc().date().to_string();

    // The page: both settlements, newest first, and neither the hold nor the top-up.
    let res = call(&app, "GET", "/dashboard/bills", None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "the bills page renders");
    let html = res.text();
    assert!(
        html.contains("Bills"),
        "the page names itself: {}",
        &html[..html.len().min(400)]
    );
    assert!(
        html.contains(&charged.content_hash),
        "the page lists the settled entry's content hash"
    );
    assert!(
        html.contains(&free.content_hash),
        "a settlement that charged nothing is still a settled entry"
    );
    assert!(
        html.contains(&charged.entry_id),
        "the page names the entry the proof endpoint takes"
    );
    // The charge column is money a reader sees, so it is the dashboard's credits
    // formatting, not the ledger's integer: 316 minor units is 0.000316 credits. The
    // integer is what the exports carry (asserted below).
    assert!(
        html.contains(">0.000316<"),
        "the cost is formatted as credits, like every other amount in the dashboard"
    );
    assert!(
        !html.contains(">316<"),
        "the page never shows the bare integer"
    );
    assert!(
        !html.contains(&charged.hold_hash),
        "a hold is not a settled entry"
    );
    assert!(
        !html.contains(&charged.top_up_hash),
        "a top-up is not a settled entry"
    );
    assert!(
        html.contains("/dashboard/bills/export.csv")
            && html.contains("/dashboard/bills/export.json"),
        "the page offers both exports"
    );

    // The CSV: a file, with the field list as its header and one row per settled entry.
    let res = call(
        &app,
        "GET",
        "/dashboard/bills/export.csv",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "the CSV export answers");
    assert_eq!(res.header("content-type"), "text/csv; charset=utf-8");
    assert_eq!(
        res.header("content-disposition"),
        "attachment; filename=\"oxsum-bills.csv\"",
        "a header a browser saves rather than renders"
    );
    let body = res.text();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        lines[0], CSV_HEADER,
        "the header is the export's field list"
    );
    assert_eq!(
        lines.len(),
        3,
        "the header and exactly the two settled entries: {body}"
    );
    assert_eq!(
        lines[1],
        format!(
            "{today},{},{},{}",
            free.entry_id, free.charged_minor, free.content_hash
        ),
        "newest first, the charge as the ledger's integer in minor units"
    );
    assert_eq!(
        lines[2],
        format!(
            "{today},{},{},{}",
            charged.entry_id, charged.charged_minor, charged.content_hash
        )
    );
    assert!(
        !body.contains('"'),
        "no field here needs quoting, so none is quoted: {body}"
    );

    // The JSON: the same rows, the same fields, money still an integer.
    let res = call(
        &app,
        "GET",
        "/dashboard/bills/export.json",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "the JSON export answers");
    assert_eq!(
        res.header("content-type"),
        "application/json; charset=utf-8"
    );
    assert_eq!(
        res.header("content-disposition"),
        "attachment; filename=\"oxsum-bills.json\""
    );
    let payload = res.json();
    let rows = payload.as_array().expect("the export is an array of rows");
    assert_eq!(rows.len(), 2, "the same two rows as the CSV: {payload}");
    for row in rows {
        let mut keys: Vec<&str> = row
            .as_object()
            .expect("a row is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["bookedOn", "chargedMinor", "contentHash", "entryId"],
            "the field names are fixed: {row}"
        );
        assert!(
            row["chargedMinor"].is_i64(),
            "money is an integer, never a string or a formatted amount: {row}"
        );
    }
    assert_eq!(rows[0]["contentHash"], free.content_hash.as_str());
    assert_eq!(rows[0]["entryId"], free.entry_id.as_str());
    assert_eq!(rows[0]["chargedMinor"], 0);
    assert_eq!(rows[0]["bookedOn"], today.as_str());
    assert_eq!(rows[1]["contentHash"], charged.content_hash.as_str());
    assert_eq!(rows[1]["chargedMinor"], 316);

    // And the two exports are the same records: each JSON row is the CSV line beside it.
    assert_eq!(rows.len(), lines.len() - 1);
    for (row, line) in rows.iter().zip(lines.iter().skip(1)) {
        assert_eq!(
            *line,
            format!(
                "{},{},{},{}",
                row["bookedOn"].as_str().unwrap(),
                row["entryId"].as_str().unwrap(),
                row["chargedMinor"].as_i64().unwrap(),
                row["contentHash"].as_str().unwrap()
            ),
            "the CSV row and the JSON row carry the same fields"
        );
    }
}

/// Both exports need the session the page needs: without it they answer a refusal, not
/// another organization's records.
#[tokio::test]
async fn the_exports_need_a_session() {
    let (app, _db) = app_or_skip!();
    for path in [
        "/dashboard/bills/export.csv",
        "/dashboard/bills/export.json",
    ] {
        let res = call(&app, "GET", path, None, None).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            res.header("content-disposition"),
            "",
            "{path} answers no file"
        );
        assert!(
            res.text().len() < 100,
            "{path} answers a sentence, not records"
        );
    }
}

/// The bills page and its exports are the acting organization's records only, whatever
/// else the shared database holds.
#[tokio::test]
async fn one_organization_never_sees_anothers_bills() {
    let (app, _db) = app_or_skip!();
    let (_tenant_a, cookie_a) = logged_in(&app, "bills-a").await;
    let (_tenant_b, cookie_b) = logged_in(&app, "bills-b").await;
    let a = seed(&app, &cookie_a, "iso-a", 400, 111).await;
    let b = seed(&app, &cookie_b, "iso-b", 400, 222).await;

    let csv_a = call(
        &app,
        "GET",
        "/dashboard/bills/export.csv",
        None,
        Some(&cookie_a),
    )
    .await
    .text();
    assert!(csv_a.contains(&a.content_hash), "its own entry is there");
    assert!(
        !csv_a.contains(&b.content_hash),
        "another organization's settled entry must not appear"
    );

    let json_b = call(
        &app,
        "GET",
        "/dashboard/bills/export.json",
        None,
        Some(&cookie_b),
    )
    .await
    .text();
    assert!(json_b.contains(&b.content_hash), "its own entry is there");
    assert!(
        !json_b.contains(&a.content_hash),
        "another organization's settled entry must not appear"
    );

    // The page itself renders for any session — the guard runs in the browser — but an
    // unauthenticated render never carries records.
    let page = call(&app, "GET", "/dashboard/bills", None, None).await;
    assert_eq!(page.status, StatusCode::OK, "the shell renders");
    assert!(
        !page.text().contains(&a.content_hash) && !page.text().contains(&b.content_hash),
        "no organization's records without a session"
    );
}
