//! Local SMTP and HTTP customer enrollment; no external mail is sent.
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser};
use oxsum_server::{Config, Mailer, Signup};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower::ServiceExt;
use uuid::Uuid;
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
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

/// The next mail the stub captured, reassembled from its quoted-printable
/// wrapping (`=\r\n` soft breaks; `=3D` for the literal `=`).
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
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("cookie", format!("oxsum_session={token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap(),
    )
}
async fn world(mailer: Option<Mailer>) -> Option<(Db, Router, String)> {
    let url = oxsum_testkit::suite_db_url("customer_invitation_delivery")?;
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let mut config = Config::new(Signup::Invite, None);
    if let Some(mailer) = mailer {
        config = config.with_mailer(mailer);
    }
    let app = oxsum_server::app(db.clone(), config);
    Some((db, app, owner))
}
const INVITES: &str = "/api/v1/business/customer-invitations";
fn terms(email: &str, mail: bool) -> Value {
    json!({"idempotencyKey":Uuid::new_v4(),"email":email,"name":"Invited company","externalId":Uuid::new_v4(),"reason":"local onboarding fixture","sendEmail":mail})
}

#[tokio::test]
async fn one_smtp_submission_enrolls_a_customer_without_staff_or_funds() {
    let mut smtp = smtp_stub().await;
    let mailer = Mailer::try_new(&smtp.url, "noreply@oxsum.test", "http://oxsum.test").unwrap();
    let Some((db, app, owner)) = world(Some(mailer)).await else {
        return;
    };
    let request = terms("New.Customer@example.test", true);
    let (status, created) = call(&app, "POST", INVITES, Some(&owner), request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["mailState"], "submitted");
    let mail = next_mail(&mut smtp).await;
    assert!(
        mail.to_lowercase()
            .contains("to: new.customer@example.test")
    );
    let secret = created["data"]["token"].as_str().unwrap();
    assert!(mail.contains(&format!("http://oxsum.test/app/invite?token={secret}")));
    let (status, replay) = call(&app, "POST", INVITES, Some(&owner), request).await;
    assert_eq!(status, StatusCode::OK);
    assert!(replay["data"]["token"].is_null());
    assert_eq!(replay["data"]["mailState"], "submitted");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), smtp.mails.recv())
            .await
            .is_err()
    );
    let (_, history) = call(&app, "GET", INVITES, Some(&owner), Value::Null).await;
    assert_eq!(history["data"]["mailConfigured"], true);
    assert_eq!(history["data"]["registrationMode"], "invite");
    assert!(!history.to_string().contains(secret));
    assert!(history["data"]["invitations"][0].get("tokenHash").is_none());
    let wrong = json!({"token":secret,"email":"wrong@example.test","password":"customer delivery fixture password"});
    assert_eq!(
        call(&app, "POST", "/api/v1/invitations/redeem", None, wrong)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let valid = json!({"token":secret,"email":"new.customer@example.test","password":"customer delivery fixture password"});
    let (status, registration) = call(
        &app,
        "POST",
        "/api/v1/invitations/redeem",
        None,
        valid.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{registration}");
    assert_eq!(
        call(&app, "POST", "/api/v1/invitations/redeem", None, valid)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let organization =
        Uuid::parse_str(created["data"]["organizationId"].as_str().unwrap()).unwrap();
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(organization)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant)
        .await
        .unwrap();
    // A customer registered through an invitation holds zero funds and has no
    // staff authority; it cannot spend or operate the business.
    assert_eq!(wallet.available().await.unwrap(), 0);
    let staff:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.business_staff s JOIN oxsum.users u USING(user_id) WHERE u.email_normalized='new.customer@example.test'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(staff, 0);
    let (_, history) = call(&app, "GET", INVITES, Some(&owner), Value::Null).await;
    assert_eq!(history["data"]["invitations"][0]["state"], "accepted");
}

#[tokio::test]
async fn missing_and_failed_smtp_keep_copy_links_and_do_not_resend() {
    let Some((db, app, owner)) = world(None).await else {
        return;
    };
    let request = terms("absent@example.test", true);
    let (_, created) = call(&app, "POST", INVITES, Some(&owner), request.clone()).await;
    assert_eq!(created["data"]["mailState"], "unavailable");
    assert!(created["data"]["token"].is_string());
    let (_, copy) = call(
        &app,
        "POST",
        INVITES,
        Some(&owner),
        terms("copy@example.test", false),
    )
    .await;
    assert_eq!(copy["data"]["mailState"], "notRequested");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (sent, mut attempts) = mpsc::channel(8);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            sent.send(()).await.unwrap();
            let _ = socket.write_all(b"554 mail refused\r\n").await;
        }
    });
    let mailer = Mailer::try_new(
        &format!("smtp://127.0.0.1:{port}"),
        "noreply@oxsum.test",
        "http://oxsum.test",
    )
    .unwrap();
    let app = oxsum_server::app(db, Config::new(Signup::Invite, None).with_mailer(mailer));
    let request = terms("refused@example.test", true);
    let (status, created) = call(&app, "POST", INVITES, Some(&owner), request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["mailState"], "unknown");
    tokio::time::timeout(Duration::from_secs(2), attempts.recv())
        .await
        .unwrap()
        .unwrap();
    let (_, replay) = call(&app, "POST", INVITES, Some(&owner), request).await;
    assert_eq!(replay["data"]["mailState"], "unknown");
    assert!(replay["data"]["token"].is_null());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), attempts.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn registration_policy_and_invitation_reads_require_current_business_authority() {
    let Some((db, app, owner)) = world(None).await else {
        return;
    };
    let request = terms("revoke@example.test", false);
    let (_, created) = call(&app, "POST", INVITES, Some(&owner), request).await;
    let (status, _) = call(&app, "GET", INVITES, None, Value::Null).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let outsider = db
        .register(NewUser {
            email: "outsider@example.test".into(),
            password: "customer delivery fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let token = db
        .login(&outsider.user.email, "customer delivery fixture password")
        .await
        .unwrap()
        .into_session()
        .token;
    assert_eq!(
        call(&app, "GET", INVITES, Some(&token), Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let policy = json!({"idempotencyKey":Uuid::new_v4(),"mode":"open","reason":"isolated onboarding fixture"});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/registration",
            Some(&token),
            policy.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/registration",
            Some(&owner),
            policy.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/registration",
            Some(&owner),
            policy
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, read) = call(&app, "GET", INVITES, Some(&owner), Value::Null).await;
    assert_eq!(read["data"]["registrationMode"], "open");
    let revoke = format!(
        "{INVITES}/{}/revoke",
        created["data"]["id"].as_str().unwrap()
    );
    let request = json!({"idempotencyKey":Uuid::new_v4(),"reason":"withdraw before consent"});
    assert_eq!(
        call(&app, "POST", &revoke, Some(&owner), request).await.0,
        StatusCode::OK
    );
    let (_, history) = call(&app, "GET", INVITES, Some(&owner), Value::Null).await;
    assert_eq!(history["data"]["invitations"][0]["state"], "revoked");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/invitations/preview",
            None,
            json!({"token":created["data"]["token"]})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
