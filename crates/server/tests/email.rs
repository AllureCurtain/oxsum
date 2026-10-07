//! HTTP tests for the email flows (issue #150): verification on registration and
//! invitation redemption, the resend endpoint and its cooldown, and the
//! forgot/reset pair — exercised end to end against a stub SMTP receiver, so the
//! token under test is the one the mail actually carried.
//!
//! The tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Mailer, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// The DATABASE_URL tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

/// A minimal SMTP receiver: it speaks just enough of the protocol for lettre —
/// greeting, an EHLO answer with no STARTTLS and no AUTH so the transport stays
/// plain and anonymous, then MAIL/RCPT/DATA — and forwards every captured DATA
/// payload over the channel for the test to read.
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

/// The `oxt-` token the mailed link carries: the mark plus 64 hex characters.
fn mailed_token(mail: &str) -> String {
    let start = mail.find("oxt-").expect("the mail carries a token");
    mail[start..start + 68].to_owned()
}

/// An app over a real database, with the mailer pointed at the stub.
async fn online_app(url: &str, mailer: Option<Mailer>) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let mut config = Config::new(Signup::Open, None);
    if let Some(mailer) = mailer {
        config = config.with_mailer(mailer);
    }
    (oxsum_server::app(db, config), pool)
}

/// The app plus the stub it mails through.
async fn mailing_app(url: &str) -> (Router, PgPool, SmtpStub) {
    let stub = smtp_stub().await;
    let mailer = Mailer::new(
        &stub.url,
        "oxsum <noreply@oxsum.test>",
        "http://oxsum.test/",
    )
    .expect("a mailer over the stub");
    let (app, pool) = online_app(url, Some(mailer)).await;
    (app, pool, stub)
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => mailing_app(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
    (no_mailer) => {
        match url() {
            Some(u) => online_app(&u, None).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

struct Res {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    cookie: Option<&str>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        body,
        set_cookie,
    }
}

/// Registers a fresh account; returns the email, the registration body and the
/// first key's secret.
async fn register(app: &Router, name: &str) -> (String, Value, String) {
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
        None,
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "registration failed: {}",
        res.body
    );
    let secret = res.body["data"]["apiKey"]["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    (email, res.body["data"].clone(), secret)
}

async fn login(app: &Router, email: &str, password: &str) -> Res {
    call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": password})),
        None,
        None,
    )
    .await
}

/// The cookie value out of a login's `Set-Cookie` header.
fn session_cookie(res: &Res) -> String {
    res.set_cookie
        .as_deref()
        .expect("a cookie was set")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned()
}

async fn verify(app: &Router, token: &str) -> Res {
    call(
        app,
        "POST",
        "/api/v1/auth/verify",
        Some(json!({"token": token})),
        None,
        None,
    )
    .await
}

#[tokio::test]
async fn registration_mails_a_link_and_the_link_verifies() {
    let (app, _pool, mut stub) = app_or_skip!();
    let (email, registration, _secret) = register(&app, "ver").await;
    assert_eq!(
        registration["verificationSent"],
        json!(true),
        "a mailer is configured: {}",
        registration
    );
    assert_eq!(registration["user"]["emailVerified"], json!(false));

    let mail = next_mail(&mut stub).await;
    assert!(mail.contains("/verify-email?token="), "{mail}");
    let token = mailed_token(&mail);

    let res = verify(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["verified"], json!(true));

    // The flag reaches the session payload the dashboard reads.
    let res = login(&app, &email, PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["user"]["emailVerified"], json!(true));
}

#[tokio::test]
async fn a_verification_token_redeems_once_and_unknowns_read_the_same() {
    let (app, _pool, mut stub) = app_or_skip!();
    register(&app, "once").await;
    let token = mailed_token(&next_mail(&mut stub).await);

    assert_eq!(verify(&app, &token).await.status, StatusCode::OK);
    // The refresh and the replay meet the spent token; an unknown token and a
    // wrong-shaped one answer identically.
    for token in [token.as_str(), "oxt-0000", "not a token"] {
        let res = verify(&app, token).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{token}");
        assert_eq!(res.body["error"]["code"], "NOT_FOUND", "{token}");
    }
}

#[tokio::test]
async fn an_expired_verification_token_is_not_found() {
    let (app, pool, mut stub) = app_or_skip!();
    let (_email, registration, _secret) = register(&app, "exp").await;
    let token = mailed_token(&next_mail(&mut stub).await);

    // Scoped to this test's user: sibling tests share the database, and an
    // unscoped update would expire their tokens out from under them.
    let user_id: uuid::Uuid = registration["user"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.email_tokens SET expires_at = now() - interval '1 second' \
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    let res = verify(&app, &token).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
}

#[tokio::test]
async fn resending_is_a_persons_action_with_a_cooldown() {
    let (app, pool, mut stub) = app_or_skip!();
    let (email, registration, secret) = register(&app, "res").await;
    let _ = next_mail(&mut stub).await;

    // An API key names no user.
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/verify/request",
        None,
        Some(&secret),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    let cookie = session_cookie(&login(&app, &email, PASSWORD).await);
    // The registration mail just went out, so the first request lands inside
    // the cooldown: it answers sent:false and mails nothing.
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/verify/request",
        None,
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["sent"], json!(false));
    assert_eq!(res.body["data"]["alreadyVerified"], json!(false));

    // Once the cooldown's minute has passed the request mints and mails again;
    // the test ages the token row rather than sleeping — scoped to this user,
    // because sibling tests share the database.
    let user_id: uuid::Uuid = registration["user"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.email_tokens SET created_at = now() - interval '61 seconds' \
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/verify/request",
        None,
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["sent"], json!(true));
    let token = mailed_token(&next_mail(&mut stub).await);
    assert_eq!(verify(&app, &token).await.status, StatusCode::OK);

    // A verified user is answered, not mailed.
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/verify/request",
        None,
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["alreadyVerified"], json!(true));
    assert_eq!(res.body["data"]["sent"], json!(false));

    // Unauthenticated gets nothing.
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/verify/request",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

#[tokio::test]
async fn forgot_answers_the_same_for_known_and_unknown_addresses() {
    let (app, _pool, mut stub) = app_or_skip!();
    let (email, _registration, _secret) = register(&app, "fgt").await;
    let _ = next_mail(&mut stub).await; // the registration mail

    for address in [email.as_str(), "nobody@example.com"] {
        let res = call(
            &app,
            "POST",
            "/api/v1/auth/password/forgot",
            Some(json!({"email": address})),
            None,
            None,
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{address}: {}", res.body);
        assert_eq!(res.body["data"], json!({}), "{address}");
    }
    // Only the known address got a mail — and the answer never said which.
    let mail = tokio::time::timeout(Duration::from_secs(10), next_mail(&mut stub))
        .await
        .expect("the known address's mail");
    assert!(mail.contains("/reset-password?token="), "{mail}");
}

#[tokio::test]
async fn the_reset_link_sets_the_password_and_revokes_the_sessions() {
    let (app, _pool, mut stub) = app_or_skip!();
    let (email, _registration, _secret) = register(&app, "rst").await;
    let _ = next_mail(&mut stub).await;

    let cookie = session_cookie(&login(&app, &email, PASSWORD).await);
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/password/forgot",
        Some(json!({"email": email})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let token = mailed_token(&next_mail(&mut stub).await);

    let res = call(
        &app,
        "POST",
        "/api/v1/auth/password/reset",
        Some(json!({"token": token, "password": "a brand new passphrase"})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["sessionsRevoked"], json!(1));

    // The old session died with the old password; the new password logs in.
    let res = call(&app, "GET", "/api/v1/session", None, None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
    assert_eq!(
        login(&app, &email, "a brand new passphrase").await.status,
        StatusCode::OK
    );
    assert_eq!(
        login(&app, &email, PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );

    // And the token is spent.
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/password/reset",
        Some(json!({"token": token, "password": "yet another passphrase"})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
}

#[tokio::test]
async fn a_verify_token_does_not_reset_a_password() {
    let (app, _pool, mut stub) = app_or_skip!();
    let (email, _registration, _secret) = register(&app, "iso").await;
    let token = mailed_token(&next_mail(&mut stub).await);

    let res = call(
        &app,
        "POST",
        "/api/v1/auth/password/reset",
        Some(json!({"token": token, "password": "some other passphrase"})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    // The password is unchanged and the verify token still verifies.
    assert_eq!(login(&app, &email, PASSWORD).await.status, StatusCode::OK);
    assert_eq!(verify(&app, &token).await.status, StatusCode::OK);
}

#[tokio::test]
async fn redeeming_an_invitation_mails_the_verification_link() {
    let (app, _pool, mut stub) = app_or_skip!();
    let (owner_email, _r, _secret) = register(&app, "inv").await;
    let _ = next_mail(&mut stub).await;
    let cookie = session_cookie(&login(&app, &owner_email, PASSWORD).await);

    let res = call(
        &app,
        "POST",
        "/api/v1/org/invitations",
        None,
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let invite_token = res.body["data"]["token"].as_str().unwrap().to_owned();

    let invitee = format!(
        "new_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let res = call(
        &app,
        "POST",
        "/api/v1/invitations/redeem",
        Some(json!({"token": invite_token, "email": invitee, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["verificationSent"], json!(true));

    let mail = next_mail(&mut stub).await;
    assert!(mail.contains("/verify-email?token="), "{mail}");
    assert_eq!(
        verify(&app, &mailed_token(&mail)).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_deployment_without_a_mailer_still_works() {
    let (app, _pool) = app_or_skip!(no_mailer);
    let (email, registration, _secret) = register(&app, "nom").await;
    // Registration stands; it simply mails nothing and says so.
    assert_eq!(registration["verificationSent"], json!(false));

    // Forgot stays indistinguishable.
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/password/forgot",
        Some(json!({"email": email})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // Resend is the one surface that reports the missing mailer.
    let cookie = session_cookie(&login(&app, &email, PASSWORD).await);
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/verify/request",
        None,
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "SERVICE_UNAVAILABLE");
}
