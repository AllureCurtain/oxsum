//! T05/A01/A06: the production router enforces consumption and member record scope.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod support;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use oxsum_core::{Db, KeyConstraints, NewUser, OpenHold, Principal, Tenants};
use oxsum_server::{BillingEvent, Config, Signup};
use serde_json::{Value, json};
use uuid::Uuid;

const PASSWORD: &str = "scope test personal password";

struct World {
    app: Router,
    db: Db,
    reg: oxsum_core::Registration,
    session: String,
    model: String,
    billing: tokio::sync::broadcast::Sender<BillingEvent>,
}
async fn world() -> Option<World> {
    let url = oxsum_testkit::suite_db_url("consumption_scope")?;
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let reg = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let session = db
        .login(&reg.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token;
    let secret = oxsum_core::SecretKey::from_bytes([5; 32]);
    let model = support::flat_model(&db, &secret, 10).await;
    let (app, billing, _) = oxsum_server::app_with_billing(
        db.clone(),
        Config::new(Signup::Open, None)
            .with_secret(secret)
            .with_head_signing_seed([7; 32]),
    );
    let w = Tenants::new(db.pool().clone())
        .get(&reg.organization.tenant_id)
        .await
        .unwrap();
    w.top_up("fixture", 10000, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    Some(World {
        app,
        db,
        reg,
        session,
        model,
        billing,
    })
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    credential: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> (StatusCode, Value, HeaderMap) {
    let mut r = Request::builder().method(method).uri(path);
    if credential.starts_with("oxsess-") {
        r = r.header("cookie", format!("oxsum_session={credential}"));
    } else {
        r = r.header("authorization", format!("Bearer {credential}"));
    }
    for (k, v) in extra {
        r = r.header(*k, *v);
    }
    if body.is_some() {
        r = r.header("content-type", "application/json");
    }
    let res = app
        .clone()
        .oneshot(
            r.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}
use tower::ServiceExt;

#[tokio::test]
async fn consumption_route_matrix_denies_account_authority_without_effects() {
    let Some(w) = world().await else { return };
    let key = &w.reg.api_key.secret;
    let wallet = Tenants::new(w.db.pool().clone())
        .get(&w.reg.organization.tenant_id)
        .await
        .unwrap();
    let head = wallet.log_size().await.unwrap();
    let balance = wallet.available().await.unwrap();
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.admin_audit")
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let denied = [
        ("GET", "/balance"),
        ("GET", "/budget"),
        ("GET", "/orgs"),
        ("POST", "/orgs"),
        ("PATCH", "/org"),
        ("GET", "/org/groups"),
        ("POST", "/org/keys"),
        ("GET", "/org/members"),
        ("POST", "/org/members"),
        ("GET", "/org/teams"),
        ("POST", "/org/teams"),
        ("GET", "/org/subscription"),
        ("GET", "/org/subscription/periods"),
        ("POST", "/org/subscription/checkout"),
        ("GET", "/webhooks"),
        ("POST", "/webhooks"),
        ("GET", "/statements"),
        ("GET", "/org/usage-reports"),
        ("POST", "/org/usage-reports"),
        ("GET", "/org/usage-reports/links"),
        ("POST", "/org/usage-reports/links"),
        ("GET", "/org/experiments"),
        ("POST", "/topups"),
        ("POST", "/topups/stripe"),
        ("POST", "/topups/airwallex"),
        ("POST", "/topups/hyperswitch"),
        ("POST", "/topups/x402"),
        ("POST", "/redemptions"),
        ("POST", "/holds"),
        ("POST", "/settlements"),
        ("GET", "/session"),
        ("POST", "/session/organization"),
        ("GET", "/device/request"),
        ("POST", "/device/authorize"),
        ("POST", "/org/ownership"),
    ];
    let mut paths: Vec<_> = denied
        .into_iter()
        .map(|(m, p)| (m, format!("/api/v1{p}")))
        .collect();
    for (m, p) in [
        ("PATCH", format!("/org/keys/{id}")),
        ("DELETE", format!("/org/keys/{id}")),
        ("GET", format!("/org/keys/{id}/mandates")),
        ("POST", format!("/org/keys/{id}/mandates")),
        ("PATCH", format!("/org/members/{id}")),
        ("DELETE", format!("/org/members/{id}")),
        ("PATCH", format!("/org/teams/{id}")),
        ("DELETE", format!("/org/teams/{id}")),
        ("DELETE", format!("/webhooks/{id}")),
        ("GET", format!("/webhooks/{id}/deliveries")),
        ("GET", format!("/statements/{id}")),
        ("GET", format!("/statements/{id}/proof")),
        ("GET", format!("/statements/{id}/export.pdf")),
        ("GET", format!("/org/usage-reports/{id}")),
        ("POST", format!("/org/usage-reports/{id}/links")),
        ("DELETE", format!("/org/usage-reports/links/{id}")),
        ("PUT", "/org/experiments/example".into()),
        ("POST", "/org/experiments/example/end".into()),
        ("GET", format!("/org/experiments/{id}/results")),
    ] {
        paths.push((m, format!("/api/v1{p}")));
    }
    for (method, path) in paths {
        let (status, body, _) = request(&w.app, method, &path, key, Some(json!({})), &[]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }
    for path in [
        "/api/v1/business",
        "/api/v1/session/context",
        "/api/v1/org/invitations",
    ] {
        let method = if path.ends_with("invitations") {
            "POST"
        } else {
            "GET"
        };
        let (status, _, _) = request(
            &w.app,
            method,
            path,
            key,
            Some(json!({"idempotencyKey":"denied"})),
            &[],
        )
        .await;
        assert!(
            matches!(status, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
            "{path}: {status}"
        );
    }
    for path in [
        "/dashboard/bills/export.csv",
        "/dashboard/bills/export.json",
        "/ws/billing",
    ] {
        assert_eq!(
            request(&w.app, "GET", path, key, None, &[]).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(wallet.available().await.unwrap(), balance);
    assert_eq!(wallet.log_size().await.unwrap(), head);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM oxsum.admin_audit")
            .fetch_one(w.db.pool())
            .await
            .unwrap(),
        audit
    );
    assert_eq!(
        w.db.list_keys(w.reg.organization.id, oxsum_core::KeyScope::All)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn own_evidence_is_readable_and_replay_cannot_cross_key_or_surface() {
    let Some(w) = world().await else { return };
    let key = &w.reg.api_key.secret;
    let owner = Principal::Session(
        w.db.authenticate_session(&w.session)
            .await
            .unwrap()
            .unwrap(),
    );
    let sibling =
        w.db.create_key_as(&owner, None, None, KeyConstraints::default())
            .await
            .unwrap();
    let body = support::chat_body(&w.model);
    let (status, first, headers) = request(
        &w.app,
        "POST",
        "/v1/chat/completions",
        key,
        Some(body.clone()),
        &[("idempotency-key", "turn")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert!(headers.get("x-oxsum-balance-minor").is_none());
    let request_id = headers["x-oxsum-request-id"].to_str().unwrap();
    let entry = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!(
        "req-{request_id}:hold"
    )));
    let proof_path = format!("/api/v1/entries/{entry}/proof");
    assert_eq!(
        request(&w.app, "GET", &proof_path, key, None, &[]).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(&w.app, "GET", &proof_path, &sibling.secret, None, &[])
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    for _ in 0..2 {
        let (status, replay, headers) = request(
            &w.app,
            "POST",
            "/v1/chat/completions",
            key,
            Some(body.clone()),
            &[("idempotency-key", "turn")],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        assert_eq!(replay, first);
        assert_eq!(headers["idempotent-replayed"], "true");
    }
    let mut changed = body.clone();
    changed["messages"][0]["content"] = json!("another private prompt");
    assert_eq!(
        request(
            &w.app,
            "POST",
            "/v1/chat/completions",
            key,
            Some(changed),
            &[("idempotency-key", "turn")]
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for (credential, path) in [
        (&sibling.secret[..], "/v1/chat/completions"),
        (key.as_str(), "/v1/responses"),
    ] {
        let (status, res, _) = request(
            &w.app,
            "POST",
            path,
            credential,
            Some(body.clone()),
            &[("idempotency-key", "turn")],
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{res}");
        assert_eq!(res["error"]["code"], "IDEMPOTENCY_MISMATCH");
    }
    for path in [
        "/api/v1/org",
        "/api/v1/org/keys",
        "/api/v1/holds",
        "/api/v1/entries",
        "/api/v1/bills",
        "/api/v1/billing-records",
        "/api/v1/usage",
        "/api/v1/requests",
        "/api/v1/pricing",
        "/api/v1/log/head",
        "/api/v1/log/consistency?from=1",
        "/api/v1/log/checkpoint",
    ] {
        let (status, res, _) = request(&w.app, "GET", path, key, None, &[]).await;
        assert_eq!(status, StatusCode::OK, "{path}: {res}");
    }
    let (status, res, _) = request(&w.app, "GET", "/api/v1/org/keys", key, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        request(
            &w.app,
            "GET",
            &format!("/api/v1/org/keys/{}/entitlement", sibling.key.id),
            key,
            None,
            &[]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &w.app,
            "GET",
            &format!("/api/v1/org/keys/{}/entitlement", w.reg.api_key.key.id),
            key,
            None,
            &[]
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, res, _) = request(
        &w.app,
        "GET",
        &format!("/api/v1/requests?keyId={}", w.reg.api_key.key.id),
        &sibling.secret,
        None,
        &[],
    )
    .await;
    assert!(res["data"]["rows"].as_array().unwrap().is_empty(), "{res}");
    let (_, res, _) = request(&w.app, "GET", "/api/v1/entries", &sibling.secret, None, &[]).await;
    assert!(res["data"]["rows"].as_array().unwrap().is_empty(), "{res}");
}

#[tokio::test]
async fn explicit_machine_headers_do_not_inherit_cookie_authority() {
    let Some(w) = world().await else { return };
    for header in ["authorization", "x-api-key", "x-goog-api-key"] {
        for path in [
            "/api/v1/balance",
            "/api/v1/session/context",
            "/ws/billing",
            "/dashboard/bills/export.json",
        ] {
            let (status, _, _) = request(
                &w.app,
                "GET",
                path,
                &w.session,
                None,
                &[(header, "malformed")],
            )
            .await;
            assert!(
                matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
                "{header} {path}: {status}"
            );
        }
    }
    assert_eq!(
        request(&w.app, "GET", "/api/v1/balance", &w.session, None, &[])
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn member_socket_filters_snapshot_and_events_and_rechecks_revocation() {
    let Some(w) = world().await else { return };
    let member =
        w.db.register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id,user_id,role) VALUES ($1,$2,'member')",
    )
    .bind(w.reg.organization.id)
    .bind(member.user.id)
    .execute(w.db.pool())
    .await
    .unwrap();
    let session =
        w.db.login(&member.user.email, PASSWORD)
            .await
            .unwrap()
            .into_session();
    w.db.switch_organization(session.session.id, member.user.id, w.reg.organization.id)
        .await
        .unwrap();
    let principal = Principal::Session(
        w.db.authenticate_session(&session.token)
            .await
            .unwrap()
            .unwrap(),
    );
    let own =
        w.db.create_key_as(&principal, None, None, KeyConstraints::default())
            .await
            .unwrap();
    for (label, id) in [
        ("own", Some(own.key.id)),
        ("sibling", Some(w.reg.api_key.key.id)),
        ("shared", None),
    ] {
        w.db.note_open_hold(&OpenHold {
            hold_key: format!("{}-{label}", Uuid::new_v4()),
            tenant_id: w.reg.organization.tenant_id.clone(),
            request_id: label.into(),
            model: w.model.clone(),
            channel: "fixture".into(),
            price_version: 1,
            input_price: 0,
            output_price: 0,
            freeze_minor: 10,
            key_id: id,
            mandate_id: None,
            end_user: None,
            service_tier: None,
            tags: Default::default(),
            sweep_attempts: 0,
            last_error: None,
            dead_at: None,
        })
        .await
        .unwrap();
    }
    let (_, holds, _) = request(&w.app, "GET", "/api/v1/holds", &session.token, None, &[]).await;
    assert_eq!(holds["data"]["holds"].as_array().unwrap().len(), 2);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = w.app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
    let mut req = format!("ws://{addr}/ws/billing")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "cookie",
        format!("oxsum_session={}", session.token).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
        panic!("snapshot")
    };
    let snapshot: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(snapshot["holds"].as_array().unwrap().len(), 2);
    assert!(!text.contains("sibling"));
    for (label, id) in [("hidden", w.reg.api_key.key.id), ("visible", own.key.id)] {
        w.billing
            .send(BillingEvent::TurnProgress {
                tenant_id: w.reg.organization.tenant_id.clone(),
                key_id: id,
                request_id: label.into(),
                output_chars: 1,
            })
            .unwrap();
    }
    let Message::Text(text) =
        tokio::time::timeout(std::time::Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    else {
        panic!("event")
    };
    assert!(text.contains("visible"));
    assert!(!text.contains("keyId"));
    w.db.logout(&session.token).await.unwrap();
    w.billing
        .send(BillingEvent::TurnProgress {
            tenant_id: w.reg.organization.tenant_id.clone(),
            key_id: own.key.id,
            request_id: "after-logout".into(),
            output_chars: 1,
        })
        .unwrap();
    let after = tokio::time::timeout(std::time::Duration::from_secs(3), socket.next())
        .await
        .unwrap();
    assert!(!matches!(after, Some(Ok(Message::Text(_)))));
    server.abort();
}

#[tokio::test]
async fn member_reads_proofs_exports_and_history_only_within_current_scope() {
    let Some(w) = world().await else { return };
    let member =
        w.db.register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id,user_id,role) VALUES ($1,$2,'member')",
    )
    .bind(w.reg.organization.id)
    .bind(member.user.id)
    .execute(w.db.pool())
    .await
    .unwrap();
    let session =
        w.db.login(&member.user.email, PASSWORD)
            .await
            .unwrap()
            .into_session();
    w.db.switch_organization(session.session.id, member.user.id, w.reg.organization.id)
        .await
        .unwrap();
    let principal = Principal::Session(
        w.db.authenticate_session(&session.token)
            .await
            .unwrap()
            .unwrap(),
    );
    let own =
        w.db.create_key_as(&principal, None, None, Default::default())
            .await
            .unwrap();
    async fn turn(w: &World, key: &str) -> String {
        let (status, body, headers) = request(
            &w.app,
            "POST",
            "/v1/chat/completions",
            key,
            Some(support::chat_body(&w.model)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let id = headers["x-oxsum-request-id"].to_str().unwrap();
        oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!("req-{id}:hold")))
            .to_string()
    }
    let own_entry = turn(&w, &own.secret).await;
    let sibling_entry = turn(&w, &w.reg.api_key.secret).await;
    w.db.revoke_key_as(&principal, own.key.id).await.unwrap();
    // Revoking the spending secret does not erase its owner's historical evidence.
    assert_eq!(
        request(
            &w.app,
            "GET",
            &format!("/api/v1/entries/{own_entry}/proof"),
            &session.token,
            None,
            &[]
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &w.app,
            "GET",
            &format!("/api/v1/entries/{sibling_entry}/proof"),
            &session.token,
            None,
            &[]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for path in [
        "/api/v1/entries",
        "/api/v1/bills",
        "/api/v1/billing-records",
        "/dashboard/bills/export.json",
    ] {
        let (status, body, _) = request(&w.app, "GET", path, &session.token, None, &[]).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert!(body.to_string().contains(&own_entry), "{path}: {body}");
        assert!(!body.to_string().contains(&sibling_entry), "{path}: {body}");
        let (status, body, _) = request(&w.app, "GET", path, &w.session, None, &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.to_string().contains(&sibling_entry),
            "owner {path}: {body}"
        );
    }
    let res = w
        .app
        .clone()
        .oneshot(
            Request::get("/dashboard/bills/export.csv")
                .header("cookie", format!("oxsum_session={}", session.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let csv =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(csv.contains(&own_entry));
    assert!(!csv.contains(&sibling_entry));
    for path in [
        "/api/v1/statements",
        "/api/v1/webhooks",
        "/api/v1/org/usage-reports",
        "/api/v1/org/usage-reports/links",
        "/api/v1/org/experiments",
    ] {
        assert_eq!(
            request(&w.app, "GET", path, &session.token, None, &[])
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    sqlx::query(
        "UPDATE oxsum.memberships SET role='admin' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(w.reg.organization.id)
    .bind(member.user.id)
    .execute(w.db.pool())
    .await
    .unwrap();
    assert_eq!(
        request(
            &w.app,
            "GET",
            &format!("/api/v1/entries/{sibling_entry}/proof"),
            &session.token,
            None,
            &[]
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("DELETE FROM oxsum.memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(w.reg.organization.id)
        .bind(member.user.id)
        .execute(w.db.pool())
        .await
        .unwrap();
    for path in [
        "/api/v1/entries",
        "/dashboard/bills/export.json",
        "/ws/billing",
    ] {
        assert_eq!(
            request(&w.app, "GET", path, &session.token, None, &[])
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
}
