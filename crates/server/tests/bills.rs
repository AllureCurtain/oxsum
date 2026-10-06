//! HTTP tests for the bills page and its two exports (issue #90).
//!
//! What the page promises is the organization's transactions — top-ups, adjustments
//! and settled requests — newest first, each with its content hash, a signed amount
//! in credits and a per-row verify link; what the exports promise is that same list
//! as a file a browser saves, with the same fields in both and the amount as the
//! ledger's signed integer in minor units. A member reads only what their own keys
//! paid plus the key-less organization history, and the proof-bundle read the
//! verify links use answers only inside the session's scope. These tests seed
//! through the wallet API and the gateway's own hold/settle pair, then read the
//! page, the page's server functions and both downloads.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    ActingKey, BillLine, Db, Settlement, SettlementKind, Tenants, UsageRecord, hold_description,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

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

/// One server function of the pages, called the way its own code calls it: the
/// URL-encoded body of the function's arguments, with the session cookie.
///
/// The path is read from the same registry the router registers its server-function
/// routes from, because leptos suffixes it with a hash of the crate and module it was
/// declared in.
async fn page_call(
    app: &Router,
    name: &str,
    body: &str,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let (path, method) = leptos::server_fn::axum::server_fn_paths()
        .find(|(path, _)| path.starts_with(&format!("/_pages/{name}")))
        .unwrap_or_else(|| panic!("the {name} server function is registered"));
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// A registered organization: the tenant its ledger lives in, the session that reads
/// the page, the organization and user ids and the key signup minted.
struct Account {
    user_id: Uuid,
    organization_id: Uuid,
    tenant_id: String,
    email: String,
    cookie: String,
    key_id: Uuid,
}

/// Registers a fresh organization and logs in.
async fn account(app: &Router, name: &str) -> Account {
    let email = format!(
        "{name}_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
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
    let data = &res.json()["data"];
    let (user_id, organization_id, tenant_id, key_id) = (
        data["user"]["id"].as_str().unwrap().parse().unwrap(),
        data["organization"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
        data["organization"]["tenantId"]
            .as_str()
            .unwrap()
            .to_owned(),
        data["apiKey"]["id"].as_str().unwrap().parse().unwrap(),
    );
    let cookie = login(app, &email).await;
    Account {
        user_id,
        organization_id,
        tenant_id,
        email,
        cookie,
        key_id,
    }
}

/// Logs in and returns the session cookie.
///
/// A session is created for the user's *oldest* membership, so for a seeded member
/// this has to run after [`membership`] to act as the seeded organization.
async fn login(app: &Router, email: &str) -> String {
    let res = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "login failed: {}", res.json());
    res.header("set-cookie")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned()
}

/// Gives `account` a `member` membership in `organization_id`, an hour before the
/// personal membership signup created, so the account's next login acts as this
/// organization — the members suite's seeding, for the page's scope tests.
async fn membership(pool: &PgPool, organization_id: Uuid, user_id: Uuid) {
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role, created_at) \
         VALUES ($1, $2, 'member', now() - interval '1 hour')",
    )
    .bind(organization_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("seeds a membership");
}

/// Registers a member of `owner`'s organization: a fresh account seeded as a member
/// an hour back, logged in again so its session acts as the organization rather than
/// the personal one signup gave it.
async fn member_of(app: &Router, db: &Db, owner: &Account, name: &str) -> (String, Account) {
    let mut member = account(app, name).await;
    membership(db.pool(), owner.organization_id, member.user_id).await;
    member.cookie = login(app, &member.email).await;
    (member.cookie.clone(), member)
}

/// Mints one more key for the session's organization; a session mint records who
/// minted it, which is what scopes a member's rows to their own keys.
async fn mint_key(app: &Router, cookie: &str, name: &str) -> Uuid {
    let res = call(
        app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": name})),
        Some(cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "minting failed: {}", res.json());
    res.json()["data"]["id"]
        .as_str()
        .expect("the key has an id")
        .parse()
        .expect("the key id is a uuid")
}

/// The server's date: the ledger records a booking date and accepts no client-supplied one.
fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}

/// One settled request as the API that wrote it named it, plus the receipts of the
/// two entries beside it: the top-up that funded it — a transaction the page lists —
/// and the hold it released, which is not.
struct Seed {
    entry_id: String,
    content_hash: String,
    charged_minor: i64,
    top_up_entry_id: String,
    top_up_hash: String,
    top_up_minor: i64,
    hold_hash: String,
}

/// Tops the organization up through its own session; answers the entry id and the
/// content hash of the transaction the page lists.
async fn top_up(app: &Router, cookie: &str, tag: &str, amount_minor: i64) -> (String, String) {
    let res = call(
        app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": format!("bills-{tag}:topup-{amount_minor}"), "amountMinor": amount_minor})),
        Some(cookie),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the top-up lands: {}",
        res.json()
    );
    (
        res.json()["data"]["entryId"].as_str().unwrap().to_owned(),
        res.json()["data"]["contentHash"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

/// Tops up, holds and settles through the wallet API — one top-up transaction and one
/// settled transaction for the page, with an outstanding hold between them the page
/// must not list.
async fn seed(app: &Router, cookie: &str, tag: &str, held: i64, charged: i64) -> Seed {
    let (top_up_entry_id, top_up_hash) = top_up(app, cookie, tag, held + 1_000).await;

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
        top_up_entry_id,
        top_up_hash,
        top_up_minor: held + 1_000,
        hold_hash,
    }
}

/// One settled gateway request, written the way the gateway writes it: the hold under
/// the key that pays, then the settlement whose record carries the request id, the
/// model, the token counts and the charge — the requests suite's seed, so a bill's
/// detail reads the same record the requests page reads.
async fn seed_request(
    tenants: &Tenants,
    tenant_id: &str,
    key_id: Uuid,
    request: &str,
    status: SettlementKind,
    charged: i64,
    freeze: i64,
) {
    let wallet = tenants
        .get(tenant_id)
        .await
        .expect("the organization's wallet opens");
    let hold_key = format!("req-{request}:hold");
    wallet
        .hold_for_key(
            &ActingKey {
                key_id,
                spend_limit_minor: None,
            },
            None,
            &hold_key,
            &hold_description(request, "mock-a", freeze).expect("the hold record serializes"),
            freeze,
            today(),
        )
        .await
        .expect("the hold is taken");
    let usage = UsageRecord::tokens(116, 100).expect("the seed counts");
    let lines = [
        BillLine {
            item: "input".to_owned(),
            units: usage.input_tokens,
            price_per_m: 1_000,
        },
        BillLine {
            item: "output".to_owned(),
            units: usage.output_tokens,
            price_per_m: 2_000,
        },
    ];
    let record = Settlement {
        request,
        channel: "mock",
        model: "mock-a",
        price_version: 1,
        kind: status,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        charged,
        freeze,
    };
    wallet
        .settle(
            &hold_key,
            &record
                .description()
                .expect("the settlement record serializes"),
            charged,
            today(),
        )
        .await
        .expect("the settlement lands");
}

/// The bills page's payload, the way the page's own code asks for it: the
/// `/_pages/get_bills` server function with the session cookie.
async fn bills(app: &Router, cookie: &str) -> Vec<Value> {
    let (status, body) = page_call(app, "get_bills", "", Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "the bills read answers: {body}");
    body["rows"]
        .as_array()
        .expect("the payload is a page of rows")
        .clone()
}

/// The proof bundle one verify link asks for: `/_pages/get_entry_bundle`.
async fn entry_bundle(app: &Router, cookie: &str, entry_id: &str) -> (StatusCode, Value) {
    page_call(
        app,
        "get_entry_bundle",
        &format!("entry_id={entry_id}"),
        Some(cookie),
    )
    .await
}

/// The CSV export's header, which is also its field list.
const CSV_HEADER: &str = "bookedOn,entryId,kind,amountMinor,description,contentHash";

/// The JSON row's fields, sorted as the tests compare them: the CSV's field list plus
/// `request`, the settled turn's parsed record.
const JSON_FIELDS: [&str; 7] = [
    "amountMinor",
    "bookedOn",
    "contentHash",
    "description",
    "entryId",
    "kind",
    "request",
];

/// The page lists the organization's transactions — top-ups and settled entries, not
/// the holds between them — and both exports carry the same rows with the same
/// fields.
#[tokio::test]
async fn the_bills_page_lists_transactions_and_the_exports_carry_them() {
    let (app, _db) = app_or_skip!();
    let org = account(&app, "bills").await;

    let charged = seed(&app, &org.cookie, "a", 400, 316).await;
    let free = seed(&app, &org.cookie, "b", 100, 0).await;
    // The posting date is the server's current UTC date (crates/server/AGENTS.md).
    let today = today().to_string();

    // The page: both top-ups and both settlements, newest first, and neither hold.
    let res = call(&app, "GET", "/dashboard/bills", None, Some(&org.cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "the bills page renders");
    let html = res.text();
    assert!(
        html.contains("Bills"),
        "the page names itself: {}",
        &html[..html.len().min(400)]
    );
    for (name, hash) in [
        ("the charged settlement", &charged.content_hash),
        ("the free settlement", &free.content_hash),
        ("a top-up", &charged.top_up_hash),
    ] {
        assert!(html.contains(hash), "{name} is a listed transaction");
    }
    assert!(
        !html.contains(&charged.hold_hash),
        "a hold bills nothing yet and is not listed"
    );
    // The amount column is money a reader sees, so it is the dashboard's credits
    // formatting with the ledger's sign: a charge reads negative, a top-up positive.
    assert!(
        html.contains(">-0.000316<"),
        "a settled charge is signed credits, like every other amount in the dashboard"
    );
    assert!(
        html.contains(">+0.001400<"),
        "the top-up of 1_400 minor units reads with its plus"
    );
    assert!(
        !html.contains(">316<"),
        "the page never shows the bare integer"
    );
    // Every row links to /verify with its entry named, so a bill verifies straight
    // from the page.
    assert!(
        html.contains(&format!("/verify?entry={}", charged.entry_id)),
        "the row names its entry for the verify page"
    );
    assert!(
        html.contains("/dashboard/bills/export.csv")
            && html.contains("/dashboard/bills/export.json"),
        "the page offers both exports"
    );

    // The CSV: a file, with the field list as its header and one row per transaction,
    // newest first — the free settlement, its top-up, the charged settlement, its
    // top-up. Nothing a wallet-API entry records as its description needs quoting,
    // and an empty one writes an empty field.
    let res = call(
        &app,
        "GET",
        "/dashboard/bills/export.csv",
        None,
        Some(&org.cookie),
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
        5,
        "the header and the four transactions: {body}"
    );
    let expected = [
        format!(
            "{today},{},settlement,0,,{}",
            free.entry_id, free.content_hash
        ),
        format!(
            "{today},{},topUp,{},,{}",
            free.top_up_entry_id, free.top_up_minor, free.top_up_hash
        ),
        format!(
            "{today},{},settlement,-{},,{}",
            charged.entry_id, charged.charged_minor, charged.content_hash
        ),
        format!(
            "{today},{},topUp,{},,{}",
            charged.top_up_entry_id, charged.top_up_minor, charged.top_up_hash
        ),
    ];
    assert_eq!(
        lines[1..],
        expected,
        "newest first, the amount as the ledger's signed integer in minor units"
    );

    // The JSON: the same rows, the same fields plus the parsed request, money still a
    // signed integer.
    let res = call(
        &app,
        "GET",
        "/dashboard/bills/export.json",
        None,
        Some(&org.cookie),
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
    assert_eq!(rows.len(), 4, "the same four rows as the CSV: {payload}");
    for row in rows {
        let mut keys: Vec<&str> = row
            .as_object()
            .expect("a row is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, JSON_FIELDS, "the field names are fixed: {row}");
        assert!(
            row["amountMinor"].is_i64(),
            "money is an integer, never a string or a formatted amount: {row}"
        );
        assert!(
            row["request"].is_null(),
            "a wallet-API settlement carries no gateway record: {row}"
        );
    }
    assert_eq!(rows[0]["entryId"], free.entry_id.as_str());
    assert_eq!(rows[0]["kind"], "settlement");
    assert_eq!(rows[0]["amountMinor"], 0);
    assert_eq!(rows[1]["entryId"], free.top_up_entry_id.as_str());
    assert_eq!(rows[1]["kind"], "topUp");
    assert_eq!(rows[1]["amountMinor"], free.top_up_minor);
    assert_eq!(rows[2]["entryId"], charged.entry_id.as_str());
    assert_eq!(rows[2]["amountMinor"], -316);

    // And the two exports are the same records: each JSON row is the CSV line beside it.
    for (row, line) in rows.iter().zip(lines.iter().skip(1)) {
        assert_eq!(
            *line,
            format!(
                "{},{},{},{},{},{}",
                row["bookedOn"].as_str().unwrap(),
                row["entryId"].as_str().unwrap(),
                row["kind"].as_str().unwrap(),
                row["amountMinor"].as_i64().unwrap(),
                row["description"].as_str().unwrap(),
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
    let a = account(&app, "bills-a").await;
    let b = account(&app, "bills-b").await;
    let a_seed = seed(&app, &a.cookie, "iso-a", 400, 111).await;
    let b_seed = seed(&app, &b.cookie, "iso-b", 400, 222).await;

    let csv_a = call(
        &app,
        "GET",
        "/dashboard/bills/export.csv",
        None,
        Some(&a.cookie),
    )
    .await
    .text();
    assert!(
        csv_a.contains(&a_seed.content_hash),
        "its own entry is there"
    );
    assert!(
        !csv_a.contains(&b_seed.content_hash) && !csv_a.contains(&b_seed.top_up_hash),
        "another organization's transactions must not appear"
    );

    let json_b = call(
        &app,
        "GET",
        "/dashboard/bills/export.json",
        None,
        Some(&b.cookie),
    )
    .await
    .text();
    assert!(
        json_b.contains(&b_seed.content_hash),
        "its own entry is there"
    );
    assert!(
        !json_b.contains(&a_seed.content_hash) && !json_b.contains(&a_seed.top_up_hash),
        "another organization's transactions must not appear"
    );

    // The page itself renders for any session — the guard runs in the browser — but an
    // unauthenticated render never carries records.
    let page = call(&app, "GET", "/dashboard/bills", None, None).await;
    assert_eq!(page.status, StatusCode::OK, "the shell renders");
    assert!(
        !page.text().contains(&a_seed.content_hash) && !page.text().contains(&b_seed.content_hash),
        "no organization's records without a session"
    );
}

/// A member's bills are the requests page's rule: the transactions their own keys
/// paid, plus the organization's shared history — a top-up carries no key, so every
/// member sees it — and never another member's spend. The export is the page's rows,
/// so it filters the same way.
#[tokio::test]
async fn a_member_reads_own_key_spend_and_the_shared_history() {
    let (app, db) = app_or_skip!();
    let owner = account(&app, "bills-owner").await;
    let (member_cookie, _member) = member_of(&app, &db, &owner, "bills-member").await;
    // The member's key is minted inside the owner's organization, so a turn it pays
    // is the member's own spend; the owner's key pays for a turn of its own.
    let member_key = mint_key(&app, &member_cookie, "member-key").await;
    top_up(&app, &owner.cookie, "scope", 5_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    seed_request(
        &tenants,
        &owner.tenant_id,
        owner.key_id,
        "req-owner",
        SettlementKind::Usage,
        100,
        400,
    )
    .await;
    seed_request(
        &tenants,
        &owner.tenant_id,
        member_key,
        "req-member",
        SettlementKind::Usage,
        200,
        400,
    )
    .await;

    // The member's rows: their own settlement and the organization's top-up — the
    // owner's settlement is another member's spend and must not appear.
    let mine = bills(&app, &member_cookie).await;
    let requests: Vec<&str> = mine
        .iter()
        .filter(|row| row["kind"] == "settlement")
        .map(|row| row["description"].as_str().unwrap())
        .collect();
    assert_eq!(requests.len(), 1, "only the member's own turn: {mine:?}");
    assert!(
        requests[0].contains("req-member"),
        "the member's own settlement is listed"
    );
    assert!(
        mine.iter().any(|row| row["kind"] == "topUp"),
        "the key-less organization top-up is shared history"
    );

    // The export is the page's rows as a file, so it filters the same way.
    let csv = call(
        &app,
        "GET",
        "/dashboard/bills/export.csv",
        None,
        Some(&member_cookie),
    )
    .await
    .text();
    assert!(csv.contains("req-member"), "the member's turn exports");
    assert!(
        !csv.contains("req-owner"),
        "another member's spend must not export"
    );

    // The owner reads everything: both turns and the top-up.
    let all = bills(&app, &owner.cookie).await;
    let descriptions: Vec<&str> = all
        .iter()
        .map(|row| row["description"].as_str().unwrap())
        .collect();
    assert!(
        descriptions.iter().any(|d| d.contains("req-owner"))
            && descriptions.iter().any(|d| d.contains("req-member")),
        "the owner sees every member's spend: {all:?}"
    );
}

/// The proof bundle the verify link fetches answers inside the session's scope only:
/// a member gets their own rows and the key-less organization ones, never another
/// member's — and nobody gets another organization's.
#[tokio::test]
async fn the_entry_bundle_stays_within_the_sessions_scope() {
    let (app, _db) = app_or_skip!();
    let org = account(&app, "bills-proof").await;
    let seeded = seed(&app, &org.cookie, "proof", 400, 316).await;

    let (status, bundle) = entry_bundle(&app, &org.cookie, &seeded.entry_id).await;
    assert_eq!(status, StatusCode::OK, "the session's own entry answers");
    let bundle = bundle.as_str().expect("the bundle is a JSON string");
    let bundle: Value = serde_json::from_str(bundle).expect("the bundle parses");
    assert_eq!(
        bundle["entry"]["id"].as_str().unwrap(),
        seeded.entry_id,
        "the bundle is the named entry's"
    );

    // A top-up is organization history with no key attached: its bundle answers too.
    let (status, _) = entry_bundle(&app, &org.cookie, &seeded.top_up_entry_id).await;
    assert_eq!(status, StatusCode::OK, "the top-up's bundle answers");

    // What is not an entry of this organization — another tenant's, or no entry at
    // all — is "no such entry", and so is a malformed id.
    let other = account(&app, "bills-proof-b").await;
    let other_seed = seed(&app, &other.cookie, "proof-b", 400, 7).await;
    for entry_id in [
        other_seed.entry_id.as_str(),
        &Uuid::new_v4().to_string(),
        "nope",
    ] {
        let (status, _) = entry_bundle(&app, &org.cookie, entry_id).await;
        assert_ne!(
            status,
            StatusCode::OK,
            "{entry_id} is outside this session's records"
        );
    }
}

/// The bundle read is scoped like the page: a member pulls the bundle for their own
/// key's settlement and for the key-less top-up, but another member's settlement —
/// in the same organization — is "no such entry", not a leak.
#[tokio::test]
async fn a_member_cannot_pull_another_members_bundle() {
    let (app, db) = app_or_skip!();
    let owner = account(&app, "bills-scope-owner").await;
    let (member_cookie, _member) = member_of(&app, &db, &owner, "bills-scope-member").await;
    let member_key = mint_key(&app, &member_cookie, "member-key").await;
    let (top_up_id, _) = top_up(&app, &owner.cookie, "scope", 5_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    for (key_id, request) in [(owner.key_id, "req-owner"), (member_key, "req-member")] {
        seed_request(
            &tenants,
            &owner.tenant_id,
            key_id,
            request,
            SettlementKind::Usage,
            100,
            400,
        )
        .await;
    }

    // The entry ids come from the owner's unfiltered read: each settlement's row.
    let all = bills(&app, &owner.cookie).await;
    let entry_of = |request: &str| {
        all.iter()
            .find(|row| row["description"].as_str().unwrap_or("").contains(request))
            .and_then(|row| row["entryId"].as_str())
            .unwrap_or_else(|| panic!("{request} is in the owner's bills: {all:?}"))
            .to_owned()
    };
    let owner_entry = entry_of("req-owner");
    let member_entry = entry_of("req-member");

    let (status, _) = entry_bundle(&app, &member_cookie, &member_entry).await;
    assert_eq!(status, StatusCode::OK, "the member's own bundle answers");
    let (status, _) = entry_bundle(&app, &member_cookie, &top_up_id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the key-less top-up's bundle answers"
    );
    let (status, _) = entry_bundle(&app, &member_cookie, &owner_entry).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "another member's settlement is not this member's to prove"
    );
    // The owner, in scope for everything, pulls the same entry fine.
    let (status, _) = entry_bundle(&app, &owner.cookie, &owner_entry).await;
    assert_eq!(status, StatusCode::OK, "the owner's read is unscoped");
}
