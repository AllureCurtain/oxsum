//! HTTP tests: the customer evidence bundles (issue #616, T25).
//!
//! `POST /api/v1/org/evidence-bundles` mints a complete-period bundle — every
//! artifact the scope commits over an ended span, the manifest the ledger's
//! marker bound once, and a signed head — which
//! `oxsum_verify::verify_evidence_bundle` checks offline. `GET
//! /api/v1/evidence/{token}` serves the same frozen bytes to a share token
//! with no session. These tests seed settled usage the way the gateway writes
//! it, mint through the API, and run the real verifier on the answer: a minted
//! bundle verifies, a re-issue replays the standing marker, the share token
//! spends its download budget, and a dead token earns 410.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    BillLine, Db, Settlement, SettlementKind, Tenants, UsageRecord, UsageRow, entry_id_for,
    settlement_key_for,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
/// The deployment's head-signing seed, so the bundle carries the signature
/// fields the verifier's last check runs.
const HEAD_SEED: [u8; 32] = [7; 32];

/// An app over a real database with oxsum's tables migrated, signing heads.
async fn online_app(url: &str) -> (Router, Db) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    (
        crate::support::app(
            db.clone(),
            Config::new(Signup::Open, None).with_head_signing_seed(HEAD_SEED),
        ),
        db,
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

/// One response, with the body kept as bytes.
struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
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

/// A registered organization: the session that calls and the tenant the ledger
/// is under.
struct Account {
    organization_id: String,
    tenant_id: String,
    cookie: String,
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
    let registered = res.json();
    Account {
        organization_id: registered["data"]["organization"]["id"]
            .as_str()
            .expect("registration names an organization")
            .to_owned(),
        tenant_id: registered["data"]["organization"]["tenantId"]
            .as_str()
            .expect("registration names a tenant")
            .to_owned(),
        cookie: login(app, &email).await,
    }
}

/// Logs in and returns the session cookie.
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

/// Tops the organization up through its own session: holds need a balance behind them.
async fn top_up(app: &Router, cookie: &str, amount_minor: i64) {
    let res = call(
        app,
        "POST",
        "/__test/funding",
        Some(json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "amountMinor": amount_minor,
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "top-up failed: {}", res.json());
}

/// The server's date: the ledger records a booking date and accepts no client-supplied one.
fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}

/// One settled gateway request, written the way the gateway writes it — the
/// hold, the settlement, and the usage row. `on` is the booking date: a
/// bundle's span must end before today, so the seeded turn books into the past
/// the bundle then covers.
#[allow(clippy::too_many_arguments)]
async fn seed_request(
    db: &Db,
    tenants: &Tenants,
    tenant_id: &str,
    request: &str,
    end_user: &str,
    cost_minor: i64,
    freeze_minor: i64,
    on: time::Date,
) {
    let wallet = tenants
        .get(tenant_id)
        .await
        .expect("the organization's wallet opens");
    let hold_key = format!("req-{request}:hold");
    wallet
        .hold(&hold_key, "seed hold", freeze_minor, on)
        .await
        .expect("the hold is taken");
    let mut usage = UsageRecord::tokens(100, 50).expect("the seed counts");
    usage.end_user = Some(end_user.to_owned());
    let record = Settlement {
        request,
        channel: "mock",
        model: "mock-model",
        requested_model: "mock-model",
        served_model: "mock-model",
        upstream_model: None,
        price_version: 1,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &[BillLine {
            item: "input".to_owned(),
            units: usage.input_tokens,
            price_per_m: 1_000,
        }],
        matched_rule: None,
        discount_percent: None,
        charged: cost_minor,
        freeze: freeze_minor,
        upstream_attempts: 1,
        mandate: None,
        subscription: None,
        cache: None,
        guardrail: Vec::new(),
        reason: None,
        experiment: None,
        sale: None,
    };
    wallet
        .settle(
            &hold_key,
            &record
                .description()
                .expect("the settlement record serializes"),
            cost_minor,
            on,
        )
        .await
        .expect("the settlement lands");
    db.record_usage(&UsageRow {
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: tenant_id.to_owned(),
        key_id: None,
        mandate_id: None,
        model: "mock-model".to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
        status: "settled".to_owned(),
        failure_reason: None,
        usage: usage.clone(),
        counts: oxsum_core::CountsProvenance::Provider {
            input: usage.input_tokens,
            output: usage.output_tokens,
        },
        charged_minor: cost_minor,
        freeze_minor,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 1,
    })
    .await
    .expect("the usage row is written");
}

/// `POST` a period bundle; returns the raw response so refusals stay inspectable.
async fn mint(app: &Router, cookie: &str, from: &str, to: &str) -> Res {
    call(
        app,
        "POST",
        "/api/v1/org/evidence-bundles",
        Some(json!({"from": from, "to": to})),
        Some(cookie),
    )
    .await
}

/// The verifier's published key, rebuilt from the fields the bundle itself
/// serves — the same reconstruction `GET /api/v1/log/key` would answer.
fn published_key(bundle: &Value) -> oxsum_verify::PublishedKey<'_> {
    oxsum_verify::PublishedKey::new(
        bundle["keyName"]
            .as_str()
            .expect("a signing deployment names a key"),
        bundle["publicKey"]
            .as_str()
            .expect("a signing deployment serves a public key"),
        bundle["keyHash"]
            .as_str()
            .expect("a signing deployment serves a key hash"),
    )
}

/// The bundle's organization id — the customer the bundle is for. The verifier
/// pins `organizationId` to it, and the serve-side org is a uuid.
fn organization_of(account: &Account) -> &str {
    &account.organization_id
}

/// The bundle's ledger origin — read back out of the signed note the way the
/// browser verifier resolves it, not supplied by hand.
fn origin_of(bundle: &Value) -> String {
    let note = bundle["note"].as_str().expect("a signed note");
    doubleentry::witness::SignedTreeHead::parse(note)
        .expect("the note parses")
        .origin
        .as_str()
        .to_owned()
}

/// Regenerates the v2 consumer fixture (`examples/verify-consumer/fixtures`):
/// mints a real period bundle against a scratch org and writes the three files
/// the offline example reads — the bundle, the published operator key it
/// carries, and the manifest naming the organization and origin the check
/// binds. Gated on `OXSUM_EVIDENCE_FIXTURE_DIR` so it never runs in CI:
/// `OXSUM_EVIDENCE_FIXTURE_DIR=examples/verify-consumer/fixtures cargo test
/// -p oxsum-server --test evidence_bundles dump_v2_fixture -- --ignored`.
#[tokio::test]
#[ignore = "fixture regeneration, not a check"]
async fn dump_v2_fixture() {
    let Some(dir) = std::env::var_os("OXSUM_EVIDENCE_FIXTURE_DIR") else {
        eprintln!("OXSUM_EVIDENCE_FIXTURE_DIR not set, skipping");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the fixture dir is writable");

    let (app, db) = app_or_skip!();
    let account = account(&app, "evidence-fixture").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    for request in ["req-a", "req-b"] {
        seed_request(
            &db,
            &tenants,
            &account.tenant_id,
            request,
            "customer-1",
            300,
            400,
            to,
        )
        .await;
    }
    let to = to.to_string();
    let bundle_id = mint(&app, &account.cookie, &to, &to).await.json()["data"]["bundleId"]
        .as_str()
        .unwrap()
        .to_owned();
    let bundle = call(
        &app,
        "GET",
        &format!("/api/v1/org/evidence-bundles/{bundle_id}"),
        None,
        Some(&account.cookie),
    )
    .await
    .json()["data"]
        .clone();
    // The fixture must verify, or the example would check bytes the writer
    // never produced.
    oxsum_verify::verify_evidence_bundle(
        &serde_json::to_string(&bundle).unwrap(),
        &origin_of(&bundle),
        organization_of(&account),
        Some(&published_key(&bundle)),
    )
    .expect("the dumped fixture verifies before it is written");

    let key = json!({
        "keyName": bundle["keyName"],
        "publicKey": bundle["publicKey"],
        "keyHash": bundle["keyHash"],
    });
    let manifest = json!({
        "organizationId": organization_of(&account),
        "origin": origin_of(&bundle),
        "bundleId": bundle_id,
    });
    for (name, value) in [
        ("evidence-bundle.json", &bundle),
        ("operator-key.json", &key),
        ("evidence-manifest.json", &manifest),
    ] {
        std::fs::write(
            dir.join(name),
            serde_json::to_string_pretty(value).expect("serializes"),
        )
        .expect("the fixture writes");
    }
    eprintln!("wrote v2 fixture to {}", dir.display());
}

/// Two settled turns over yesterday — a span that has fully ended — and the
/// minted bundle verifies and replays.
#[tokio::test]
async fn a_minted_period_bundle_verifies_and_replays() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "evidence").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    for request in ["req-a", "req-b"] {
        seed_request(
            &db,
            &tenants,
            &account.tenant_id,
            request,
            "customer-1",
            300,
            400,
            to,
        )
        .await;
    }

    let span = (to.to_string(), to.to_string());
    let res = mint(&app, &account.cookie, &span.0, &span.1).await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the mint failed: {}",
        res.json()
    );
    let descriptor = res.json()["data"].clone();
    let bundle_id = descriptor["bundleId"].as_str().expect("a bundle id");
    assert_eq!(descriptor["kind"].as_str(), Some("period"));
    assert!(descriptor["entryCount"].as_i64().unwrap() >= 2);

    // The download serves the frozen bundle — the whole `data` object the
    // verifier consumes.
    let fetched = call(
        &app,
        "GET",
        &format!("/api/v1/org/evidence-bundles/{bundle_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(fetched.status, StatusCode::OK, "{}", fetched.json());
    let bundle = fetched.json()["data"].clone();
    assert_eq!(bundle["version"].as_i64(), Some(2));
    assert_eq!(bundle["kind"].as_str(), Some("period"));
    assert!(
        bundle["note"].is_string(),
        "a signing deployment signs the head"
    );
    let json = serde_json::to_string(&bundle).unwrap();
    oxsum_verify::verify_evidence_bundle(
        &json,
        &origin_of(&bundle),
        organization_of(&account),
        Some(&published_key(&bundle)),
    )
    .expect("the minted bundle verifies");

    // Re-issuing the same span replays the standing marker — the once-per-span
    // rule the manifest's commitment rests on.
    let replay = mint(&app, &account.cookie, &span.0, &span.1).await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(
        replay.json()["data"]["bundleId"].as_str(),
        Some(bundle_id),
        "a re-issue replays the standing bundle"
    );
}

/// The bundle list names the minted descriptor, newest first.
#[tokio::test]
async fn the_bundle_list_names_minted_descriptors() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "evidence-list").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "req-a",
        "customer-1",
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let id = mint(&app, &account.cookie, &to, &to).await.json()["data"]["bundleId"]
        .as_str()
        .unwrap()
        .to_owned();

    let res = call(
        &app,
        "GET",
        "/api/v1/org/evidence-bundles",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = res.json();
    let rows = body["data"]["rows"].as_array().expect("rows is a list");
    let row = rows
        .iter()
        .find(|row| row["bundleId"].as_str() == Some(&id))
        .expect("the list names the minted bundle");
    assert_eq!(row["kind"].as_str(), Some("period"));
    assert_eq!(row["periodFrom"].as_str(), Some(to.as_str()));
}

/// A bundle answers only for the organization that minted it — another org's
/// id probes nothing.
#[tokio::test]
async fn a_bundle_answers_only_for_its_organization() {
    let (app, db) = app_or_skip!();
    let owner = account(&app, "evidence-owner").await;
    let other = account(&app, "evidence-other").await;
    top_up(&app, &owner.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &owner.tenant_id,
        "req-a",
        "customer-1",
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let id = mint(&app, &owner.cookie, &to, &to).await.json()["data"]["bundleId"]
        .as_str()
        .unwrap()
        .to_owned();

    for method_path in [
        format!("/api/v1/org/evidence-bundles/{id}"),
        format!("/api/v1/org/evidence-bundles/{id}/share"),
    ] {
        let probe = call(&app, "GET", &method_path, None, Some(&other.cookie)).await;
        assert_eq!(
            probe.status,
            StatusCode::NOT_FOUND,
            "{method_path} answers only for its organization"
        );
    }
    // Minting a share against another org's bundle is the same 404.
    let share = call(
        &app,
        "POST",
        &format!("/api/v1/org/evidence-bundles/{id}/share"),
        Some(json!({})),
        Some(&other.cookie),
    )
    .await;
    assert_eq!(share.status, StatusCode::NOT_FOUND);
}

/// A share token opens the public download without a session, spends its
/// `maxDownloads` budget, and the count is audited on the descriptor.
#[tokio::test]
async fn a_share_token_resolves_downloads_and_spends_its_budget() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "evidence-share").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "req-a",
        "customer-1",
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let bundle_id = mint(&app, &account.cookie, &to, &to).await.json()["data"]["bundleId"]
        .as_str()
        .unwrap()
        .to_owned();

    // One download only: the second resolution must go dead.
    let created = call(
        &app,
        "POST",
        &format!("/api/v1/org/evidence-bundles/{bundle_id}/share"),
        Some(json!({"label": "auditor", "maxDownloads": 1})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.json());
    let data = created.json()["data"].clone();
    let token = data["token"].as_str().expect("a token").to_owned();
    assert!(token.starts_with("oxe-"), "the mark identifies the token");
    assert_eq!(data["share"]["downloadCount"].as_i64(), Some(0));
    assert_eq!(data["share"]["maxDownloads"].as_i64(), Some(1));

    // The token alone downloads the bundle — no session, the token is the
    // credential — and the bytes verify.
    let downloaded = call(
        &app,
        "GET",
        &format!("/api/v1/evidence/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(downloaded.status, StatusCode::OK, "{}", downloaded.json());
    assert_eq!(
        downloaded.header("content-type"),
        "application/json",
        "the download serves JSON"
    );
    let bundle = downloaded.json()["data"].clone();
    oxsum_verify::verify_evidence_bundle(
        &serde_json::to_string(&bundle).unwrap(),
        &origin_of(&bundle),
        organization_of(&account),
        Some(&published_key(&bundle)),
    )
    .expect("the shared bundle verifies");

    // The download was spent: a second resolution is the same 410 a dead
    // token earns, and the descriptor's audit count moved to 1.
    let again = call(
        &app,
        "GET",
        &format!("/api/v1/evidence/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        again.status,
        StatusCode::GONE,
        "a spent token resolves to nothing"
    );
    let shares = call(
        &app,
        "GET",
        &format!("/api/v1/org/evidence-bundles/{bundle_id}/share"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(shares.status, StatusCode::OK);
    let row = shares.json()["data"]["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|r| r["shareId"] == data["share"]["shareId"])
        .expect("the minted share is listed")
        .clone();
    assert_eq!(
        row["downloadCount"].as_i64(),
        Some(1),
        "the count is audited"
    );
    assert!(
        row["lastDownloadAt"].is_string(),
        "the download is recorded"
    );
    assert!(
        row.get("token").is_none(),
        "the list never echoes the token"
    );
}

/// Revoking a bundle's shares closes the public download; an unknown token is
/// the same 410.
#[tokio::test]
async fn a_revoked_share_and_an_unknown_token_resolve_to_gone() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "evidence-revoke").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "req-a",
        "customer-1",
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let bundle_id = mint(&app, &account.cookie, &to, &to).await.json()["data"]["bundleId"]
        .as_str()
        .unwrap()
        .to_owned();
    let token = call(
        &app,
        "POST",
        &format!("/api/v1/org/evidence-bundles/{bundle_id}/share"),
        Some(json!({})),
        Some(&account.cookie),
    )
    .await
    .json()["data"]["token"]
        .as_str()
        .expect("a token")
        .to_owned();

    // Live before revocation.
    let live = call(
        &app,
        "GET",
        &format!("/api/v1/evidence/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(live.status, StatusCode::OK);

    let revoked = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/evidence-bundles/{bundle_id}/share"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.json());
    assert!(revoked.json()["data"]["revoked"].as_i64().unwrap() >= 1);

    let closed = call(
        &app,
        "GET",
        &format!("/api/v1/evidence/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        closed.status,
        StatusCode::GONE,
        "a revoked token resolves to nothing"
    );
    let unknown = call(&app, "GET", "/api/v1/evidence/oxe-unknown", None, None).await;
    assert_eq!(unknown.status, StatusCode::GONE);
}

/// An expired share resolves to the same 410 — the database moves the expiry
/// into the past the way time would.
#[tokio::test]
async fn an_expired_share_resolves_to_gone() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "evidence-expire").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "req-a",
        "customer-1",
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let bundle_id = mint(&app, &account.cookie, &to, &to).await.json()["data"]["bundleId"]
        .as_str()
        .unwrap()
        .to_owned();
    let created = call(
        &app,
        "POST",
        &format!("/api/v1/org/evidence-bundles/{bundle_id}/share"),
        Some(json!({})),
        Some(&account.cookie),
    )
    .await;
    let data = created.json()["data"].clone();
    let share_id = Uuid::parse_str(data["share"]["shareId"].as_str().expect("a share id"))
        .expect("the id parses");
    let token = data["token"].as_str().expect("a token").to_owned();

    sqlx::query(
        "UPDATE oxsum.evidence_share_tokens SET expires_at = now() - interval '1 second' \
         WHERE share_id = $1",
    )
    .bind(share_id)
    .execute(db.pool())
    .await
    .expect("the expiry moves into the past");

    let closed = call(
        &app,
        "GET",
        &format!("/api/v1/evidence/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        closed.status,
        StatusCode::GONE,
        "an expired token resolves to nothing"
    );
}

/// The statement-scoped bundle answers 404 for a statement the organization
/// does not have — a foreign or missing id is never a cross-customer read.
#[tokio::test]
async fn a_statement_evidence_for_a_missing_statement_is_not_found() {
    let (app, _db) = app_or_skip!();
    let account = account(&app, "evidence-stmt").await;
    let missing = Uuid::new_v4();
    let res = call(
        &app,
        "GET",
        &format!("/api/v1/org/statements/{missing}/evidence"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::NOT_FOUND,
        "a statement the org does not own is 404: {}",
        res.json()
    );
}

/// The default bundle redacts `upstreamCostMinor` — the platform's supplier
/// cost is internal — while `costBasis` always exports, and the scope block
/// honestly keeps `upstreamCostFigures` on the excluded list.
#[tokio::test]
async fn the_usage_artifact_redacts_upstream_cost_by_default() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "evidence-redact").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "req-a",
        "customer-1",
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let bundle_id = mint(&app, &account.cookie, &to, &to).await.json()["data"]["bundleId"]
        .as_str()
        .unwrap()
        .to_owned();
    let fetched = call(
        &app,
        "GET",
        &format!("/api/v1/org/evidence-bundles/{bundle_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    let bundle = fetched.json()["data"].clone();

    // The scope block names what it does not attest: the supplier cost is on
    // the excluded list because the flag is off.
    let excluded = bundle["scope"]["excluded"].as_array().expect("excluded");
    assert!(
        excluded.iter().any(|c| c == "upstreamCostFigures"),
        "upstreamCostFigures stays excluded: {excluded:?}"
    );
    assert_eq!(
        bundle["scope"]["upstreamCostIncluded"].as_bool(),
        Some(false)
    );

    // The usage artifacts carry costBasis but no upstreamCostMinor.
    let usage = bundle["artifacts"]["usageRecord"]
        .as_object()
        .expect("usageRecord artifacts");
    assert!(!usage.is_empty(), "the seeded usage is committed");
    for artifact in usage.values() {
        assert!(
            artifact.get("upstreamCostMinor").is_none(),
            "the supplier cost is redacted: {artifact}"
        );
        assert!(
            artifact.get("costBasis").is_some(),
            "costBasis always exports: {artifact}"
        );
        assert!(
            artifact.get("chargedMinor").is_some(),
            "the charged figure exports: {artifact}"
        );
    }
}

/// An open `to` is refused — the manifest commits a complete artifact set and
/// an open day can still book rows the bundle would omit.
#[tokio::test]
async fn an_open_span_is_refused() {
    let (app, _db) = app_or_skip!();
    let account = account(&app, "evidence-open").await;
    let today = today().to_string();
    let res = mint(&app, &account.cookie, "2026-01-01", &today).await;
    assert_eq!(
        res.status,
        StatusCode::BAD_REQUEST,
        "an open `to` is refused: {}",
        res.json()
    );
}
