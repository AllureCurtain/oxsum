//! HTTP staff invitation flow: email-bound grant, owner-only writes and a real
//! local SMTP stub proving at-most-once submission.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    BusinessSetupRequest, BusinessSetupTokenRequest, Db, NewUser, Registration, SessionContext,
    SessionContextRequest, SetupAuthority, User,
};
use oxsum_server::{Config, Mailer, Signup};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "staff invitation http password";
const STAFF: &str = "/api/v1/business/staff-invitations";

struct SmtpStub {
    url: String,
    mails: mpsc::Receiver<String>,
}
async fn smtp_stub() -> SmtpStub {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mails) = mpsc::channel(16);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut lines = BufReader::new(reader).lines();
                if writer.write_all(b"220 stub ESMTP\r\n").await.is_err() {
                    return;
                }
                let mut data = String::new();
                let mut in_data = false;
                while let Ok(Some(line)) = lines.next_line().await {
                    if in_data {
                        if line == "." {
                            let _ = tx.send(std::mem::take(&mut data)).await;
                            in_data = false;
                            if writer.write_all(b"250 queued\r\n").await.is_err() {
                                return;
                            }
                        } else {
                            data.push_str(&line);
                            data.push('\n');
                        }
                        continue;
                    }
                    let verb = line.split_whitespace().next().unwrap_or("");
                    let reply = match verb.to_ascii_uppercase().as_str() {
                        "EHLO" | "HELO" => "250 stub\r\n",
                        "MAIL" | "RCPT" | "RSET" | "NOOP" => "250 ok\r\n",
                        "DATA" => {
                            in_data = true;
                            "354 go ahead\r\n"
                        }
                        "QUIT" => {
                            let _ = writer.write_all(b"221 bye\r\n").await;
                            return;
                        }
                        _ => "502 not implemented\r\n",
                    };
                    if writer.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    SmtpStub {
        url: format!("smtp://127.0.0.1:{port}"),
        mails,
    }
}
async fn next_mail(stub: &mut SmtpStub) -> String {
    let raw = tokio::time::timeout(Duration::from_secs(10), stub.mails.recv())
        .await
        .expect("a mail arrives")
        .expect("the stub keeps running");
    raw.replace("=\r\n", "")
        .replace("=\n", "")
        .replace("=3D", "=")
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("cookie", format!("oxsum_session={token}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    (
        res.status(),
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
            .unwrap_or(Value::Null),
    )
}
async fn user(db: &Db, name: &str) -> Registration {
    db.register(NewUser {
        email: format!("{name}@example.com"),
        password: PASSWORD.into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
async fn login(db: &Db, user: &User) -> String {
    db.login(&user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token
}
async fn world(mailer: Option<Mailer>) -> Option<(Db, Router, String)> {
    let url = oxsum_testkit::suite_db_url("staff_invitation_delivery")?;
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = user(&db, "owner").await;
    let token = login(&db, &owner.user).await;
    let setup = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &BusinessSetupTokenRequest {
                idempotency_key: "setup".into(),
                owner_email: owner.user.email.clone(),
            },
        )
        .await
        .unwrap();
    db.accept_business_setup(
        &token,
        &BusinessSetupRequest {
            token: setup.token.unwrap(),
            business_name: "Staff invite business".into(),
        },
    )
    .await
    .unwrap();
    db.switch_session_context(
        &token,
        &SessionContextRequest {
            idempotency_key: "business".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    let mut config = Config::new(Signup::Invite, None);
    if let Some(mailer) = mailer {
        config = config.with_mailer(mailer);
    }
    let app = oxsum_server::app(db.clone(), config);
    Some((db, app, token))
}
fn terms(email: &str, role: &str, send: bool) -> Value {
    json!({"idempotencyKey":Uuid::new_v4(),"email":email,"role":role,"reason":"staff onboarding fixture","sendEmail":send})
}

#[tokio::test]
async fn a_staff_invitation_mails_once_and_enrolls_through_the_bound_session() {
    let mut smtp = smtp_stub().await;
    let mailer = Mailer::try_new(&smtp.url, "noreply@oxsum.test", "http://oxsum.test").unwrap();
    let Some((_db, app, owner)) = world(Some(mailer)).await else {
        return;
    };
    let req = terms("New.Staff@example.test", "finance", true);
    let (status, created) = call(&app, "POST", STAFF, Some(&owner), req.clone()).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["mailState"], "submitted");
    let secret = created["data"]["token"].as_str().unwrap();
    let mail = next_mail(&mut smtp).await;
    assert!(mail.to_lowercase().contains("to: new.staff@example.test"));
    assert!(mail.contains(&format!("http://oxsum.test/app/invite?token={secret}")));
    // The browser requires a successful public preview before enabling consent.
    let (status, preview) = call(
        &app,
        "POST",
        "/api/v1/invitations/preview",
        None,
        json!({"token":secret}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["data"]["organizationId"], Uuid::nil().to_string());
    assert_eq!(preview["data"]["name"], "Staff invite business");
    assert_eq!(preview["data"]["staffRole"], "finance");
    assert!(!preview.to_string().contains(secret));
    // A replayed create returns the same invitation and never resends.
    let (_, replay) = call(&app, "POST", STAFF, Some(&owner), req).await;
    assert!(replay["data"]["token"].is_null());
    assert_eq!(replay["data"]["mailState"], "submitted");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), smtp.mails.recv())
            .await
            .is_err()
    );
    // History shows the grant, never the token.
    let (_, history) = call(&app, "GET", STAFF, Some(&owner), Value::Null).await;
    assert_eq!(history["data"]["mailConfigured"], true);
    assert_eq!(
        history["data"]["invitations"][0]["email"],
        "new.staff@example.test"
    );
    assert_eq!(history["data"]["invitations"][0]["role"], "finance");
    assert!(!history.to_string().contains(secret));

    // The new recipient redeems the staff token into a staff-only account.
    let (status, redeemed) = call(
        &app,
        "POST",
        "/api/v1/invitations/redeem",
        None,
        json!({"token":secret,"email":"new.staff@example.test","password":PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{redeemed}");
    assert_eq!(redeemed["data"]["kind"], "staff");
    assert!(redeemed["data"]["user"]["email"].is_string());
    assert!(redeemed["data"]["apiKey"].is_null());
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/invitations/preview",
            None,
            json!({"token":secret})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn staff_invitation_writes_and_reads_need_owner_authority() {
    let Some((db, app, owner)) = world(None).await else {
        return;
    };
    let outsider = user(&db, "outsider").await;
    let wrong = login(&db, &outsider.user).await;
    for (cookie, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(wrong.as_str()), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            call(
                &app,
                "POST",
                STAFF,
                cookie,
                terms("a@b.test", "viewer", false)
            )
            .await
            .0,
            expected
        );
        assert_eq!(
            call(&app, "GET", STAFF, cookie, Value::Null).await.0,
            expected
        );
    }
    // The owner role is refused by the request contract itself.
    let (_, refused) = call(
        &app,
        "POST",
        STAFF,
        Some(&owner),
        terms("x@y.test", "owner", false),
    )
    .await;
    assert!(refused["error"].as_str().is_some() || refused.is_null() || refused["data"].is_null());
    // A viewer seated through the flow cannot mint a further invitation.
    let vuser = user(&db, "vaccept").await;
    let vsession = login(&db, &vuser.user).await;
    let (_, created2) = call(
        &app,
        "POST",
        STAFF,
        Some(&owner),
        terms("vaccept@example.com", "viewer", false),
    )
    .await;
    let vsecret = created2["data"]["token"].as_str().unwrap();
    call(
        &app,
        "POST",
        "/api/v1/invitations/accept",
        Some(&vsession),
        json!({"token":vsecret}),
    )
    .await;
    db.switch_session_context(
        &vsession,
        &SessionContextRequest {
            idempotency_key: "vc".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            STAFF,
            Some(&vsession),
            terms("another@example.test", "viewer", false)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn revoke_pulls_a_pending_staff_invitation() {
    let Some((_db, app, owner)) = world(None).await else {
        return;
    };
    let (_, created) = call(
        &app,
        "POST",
        STAFF,
        Some(&owner),
        terms("gone@example.test", "support", false),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap();
    let secret = created["data"]["token"].as_str().unwrap().to_string();
    let revoke = format!("{STAFF}/{id}/revoke");
    let (status, _) = call(
        &app,
        "POST",
        &revoke,
        Some(&owner),
        json!({"idempotencyKey":Uuid::new_v4(),"reason":"withdraw"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/invitations/preview",
            None,
            json!({"token":secret})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (_, history) = call(&app, "GET", STAFF, Some(&owner), Value::Null).await;
    assert_eq!(history["data"]["invitations"][0]["state"], "revoked");
}
