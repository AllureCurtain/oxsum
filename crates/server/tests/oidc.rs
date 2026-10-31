//! The OIDC login end to end (issue #504): a stub enterprise IdP behind a
//! second axum listener — discovery, token exchange and JWKS — against which
//! the generalized `/api/v1/auth/oauth/{provider}` pair runs a real
//! `code`+PKCE flow, down to the session cookie.
//!
//! The stub's `id_token` is a genuine RS256 JWS: signed with a checked-in
//! throwaway key (`fixtures/test_idp_rsa.pem`, minted for the test and good
//! for nothing else) and verified against the JWKS the stub serves, so the
//! signature path — not just the wire shape — is what proves the login.
//! Requires DATABASE_URL; without it the tests skip.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Json as AxumJson, Response};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The checked-in throwaway signing key — never a real credential.
const IDP_RSA_PEM: &str = include_str!("fixtures/test_idp_rsa.pem");
/// The stub provider's configured name: its `sub` namespaces under `oidc:test`.
const IDP_NAME: &str = "test";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("oidc")
}

macro_rules! world_or_skip {
    () => {
        match url() {
            Some(u) => world(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// The stub IdP's state: its own origin (discovery serves endpoints under it)
/// and the token request it last saw — the PKCE verifier must ride it.
struct Idp {
    issuer: String,
    seen: Arc<Mutex<Vec<Value>>>,
}

/// What the stub's `/authorize` expects the redirect to carry.
#[derive(Deserialize)]
struct AuthorizeQuery {
    client_id: String,
    redirect_uri: String,
    state: String,
    nonce: String,
    code_challenge: String,
}

/// The stub's legs: discovery, the authorize page (which redirects straight
/// back with a code), the token exchange and the JWKS document.
async fn start_idp() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the IdP binds");
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let state = Idp {
        issuer: issuer.clone(),
        seen: seen.clone(),
    };
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/jwks", get(jwks))
        .with_state(Arc::new(state));
    tokio::spawn(async move { axum::serve(listener, app).await });
    (issuer, seen)
}

async fn discovery(State(idp): State<Arc<Idp>>) -> AxumJson<Value> {
    AxumJson(json!({
        "issuer": idp.issuer,
        "authorization_endpoint": format!("{}/authorize", idp.issuer),
        "token_endpoint": format!("{}/token", idp.issuer),
        "jwks_uri": format!("{}/jwks", idp.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
}

/// The authorize page: a real IdP would render a login form; the stub is a
/// logged-in user already, so it redirects straight back with a code and the
/// state — recording the challenge it was handed for the token leg's assert.
async fn authorize(State(idp): State<Arc<Idp>>, Query(query): Query<AuthorizeQuery>) -> Response {
    idp.seen.lock().unwrap().push(json!({
        "leg": "authorize",
        "client_id": query.client_id,
        "nonce": query.nonce,
        "code_challenge": query.code_challenge,
    }));
    axum::response::Redirect::to(&format!(
        "{}?code=stub-auth-code&state={}",
        query.redirect_uri, query.state
    ))
    .into_response()
}

/// The token exchange: the form must carry the PKCE `code_verifier` — the
/// stub hashes it and compares to the authorize leg's challenge — then
/// answers an `id_token` signed with the test key, echoing the nonce.
async fn token(State(idp): State<Arc<Idp>>, body: String) -> AxumJson<Value> {
    // reqwest posts the exchange as application/x-www-form-urlencoded; the
    // fields the test reads are single-line pairs, no decoding needed.
    let get = |name: &str| {
        body.split('&')
            .find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                (k == name).then(|| urlencoding_decode(v))
            })
            .unwrap_or_default()
    };
    idp.seen.lock().unwrap().push(json!({
        "leg": "token",
        "grant_type": get("grant_type"),
        "code": get("code"),
        "code_verifier": get("code_verifier"),
    }));
    let authorize = idp
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|v| v["leg"] == "authorize")
        .cloned()
        .expect("authorize ran before token");
    // S256: the verifier's SHA-256 must equal the authorize leg's challenge —
    // the stub proves the flow end to end, not just the wire shapes.
    use base64::Engine as _;
    use sha2::Digest as _;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(get("code_verifier").as_bytes()));
    assert_eq!(
        challenge,
        authorize["code_challenge"].as_str().unwrap(),
        "the PKCE verifier must hash to the authorize leg's challenge"
    );

    let claims = json!({
        "iss": idp.issuer,
        "sub": "employee-42",
        "aud": "oxsum-test-client",
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 600,
        "iat": time::OffsetDateTime::now_utc().unix_timestamp(),
        "nonce": authorize["nonce"].as_str().unwrap(),
        "email": "employee-42@corp.example",
        "email_verified": true,
    });
    let id_token = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_rsa_pem(IDP_RSA_PEM.as_bytes()).unwrap(),
    )
    .unwrap();
    AxumJson(json!({
        "access_token": "stub-access",
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token,
    }))
}

/// Percent-decodes one form value — `+` for space, `%XX` for bytes; the code
/// verifier is base64url so this is belt and braces.
fn urlencoding_decode(value: &str) -> String {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(bytes[i + 1]) << 4 | hex(bytes[i + 2]));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// The JWKS document: the public half of `IDP_RSA_PEM` in JWK form — the
/// modulus and exponent extracted at keygen time and constant next to it.
async fn jwks() -> AxumJson<Value> {
    AxumJson(json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "kid": "test-key-1",
            "alg": "RS256",
            "n": "1t9O-iqAzaB_TuFmuRMpP4es8uFCohtgTfD4g6difXACUxfxz3v2148-8o_biO9FRnuLYOX-Awt7aOofD5YBMcw0ZS1pZSNqM6KOfgm73fzuYiHws-hEFbN5l8kSFBZN0ffPB9m2He48lU1ahnPlhyobrvrGV4w9EFTjN3zM15zW8J9HXW5EIxvAynehxOG614DB4gc8NE8ycfQVUmlCOkA9cdqVH3EzU31xvMAZhLlv7MbcZyfrvOLL_Agy98R86JoNJkvDsbrrYOWmZmHElJCc0MpNkV_vjr2EsZ4phLtKFe3Ab3Dxn2zDNcF9GUGV8TCr2PGYTryaGfpL9hWIDw",
            "e": "AQAB",
        }],
    }))
}

/// The test world: the app over a real database with the stub IdP configured
/// as `oidc:test` — `public_url` a placeholder, since the stub's authorize leg
/// redirects to whatever `redirect_uri` it is sent.
struct World {
    app: Router,
    db: Db,
    issuer: String,
    seen: Arc<Mutex<Vec<Value>>>,
}

async fn world(database_url: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");

    let (issuer, seen) = start_idp().await;
    let provider = oxsum_server::oidc::OidcProvider::new(
        IDP_NAME.to_owned(),
        issuer.clone(),
        "oxsum-test-client".to_owned(),
        "oxsum-test-secret".to_owned(),
        "http://oxsum.test".to_owned(),
    );
    let config = Config::new(Signup::Open, None)
        .with_secret(SecretKey::from_bytes([5; 32]))
        .with_oidc_providers(vec![provider]);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = oxsum_server::app(db.clone(), config);
    World {
        app,
        db,
        issuer,
        seen,
    }
}

/// One request whose status, `Location` and `Set-Cookie` are the assertions.
async fn call(app: &Router, path: &str) -> (StatusCode, Option<String>, Option<String>, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("the test's own request"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body collects")
        .to_bytes();
    let body: Value = if bytes.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&bytes).unwrap_or(json!(null))
    };
    (status, location, set_cookie, body)
}

#[tokio::test]
async fn the_login_page_learns_the_providers() {
    let world = world_or_skip!();
    let (status, _, _, body) = call(&world.app, "/api/v1/auth/methods").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["oidcProviders"],
        json!(["oidc:test"]),
        "auth methods: {body}"
    );
}

#[tokio::test]
async fn an_oidc_login_mints_the_session_and_links_the_identity() {
    let world = world_or_skip!();

    // Leg one: the start route discovers and redirects to the stub's authorize.
    let (status, location, _, _) = call(&world.app, "/api/v1/auth/oauth/oidc:test").await;
    assert_eq!(status, StatusCode::SEE_OTHER, "start: {location:?}");
    let authorize_url = location.expect("the authorize redirect");
    assert!(
        authorize_url.starts_with(&format!("{}/authorize?", world.issuer)),
        "authorize url: {authorize_url}"
    );
    assert!(authorize_url.contains("code_challenge="), "{authorize_url}");
    assert!(authorize_url.contains("nonce="), "{authorize_url}");

    // The stub's authorize answers the callback URL with code+state: run that
    // redirect against the app's callback — the state's PKCE pair rides inside
    // the stored row, so the callback finishes the exchange itself.
    let callback = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(&authorize_url)
        .send()
        .await
        .expect("the stub authorizes");
    assert_eq!(callback.status(), reqwest::StatusCode::SEE_OTHER);
    let back = callback
        .headers()
        .get(reqwest::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    // `back` is `{public_url}/api/v1/auth/oauth/oidc:test/callback?code=…&state=…`;
    // rewrite it onto the in-process app's path.
    let query = back.split('?').nth(1).expect("the callback query");
    let (status, location, set_cookie, _) = call(
        &world.app,
        &format!("/api/v1/auth/oauth/oidc:test/callback?{query}"),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "callback failed");
    assert_eq!(location.as_deref(), Some("/app"), "{location:?}");
    let cookie = set_cookie.expect("the callback mints the session cookie");
    assert!(cookie.starts_with("oxsum_session=oxsess-"), "{cookie}");

    // The identity linked under the provider's own namespace.
    let (provider, sub): (String, String) = sqlx::query_as(
        "SELECT provider, provider_user_id FROM oxsum.oauth_accounts ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(world.db.pool())
    .await
    .unwrap();
    assert_eq!(provider, "oidc:test");
    assert_eq!(sub, "employee-42");

    // The state was consumed — a replay of the same callback finds nothing.
    let (status, location, _, _) = call(
        &world.app,
        &format!("/api/v1/auth/oauth/oidc:test/callback?{query}"),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/app/login?error=oauth"));
    // And the token exchange ran exactly once: the replay died at the state.
    let exchanges = world
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v["leg"] == "token")
        .count();
    assert_eq!(exchanges, 1, "the replay must not reach the token leg");
}

#[tokio::test]
async fn an_unknown_provider_is_a_404() {
    let world = world_or_skip!();
    let (status, _, _, _) = call(&world.app, "/api/v1/auth/oauth/oidc:absent").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _, _) = call(&world.app, "/api/v1/auth/oauth/github").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "github is not configured in this world"
    );
}

#[tokio::test]
async fn an_unknown_state_fails_the_login() {
    let world = world_or_skip!();
    // A callback whose state was never minted — or was already spent — answers
    // the login page, not a session.
    let (status, location, cookie, _) = call(
        &world.app,
        "/api/v1/auth/oauth/oidc:test/callback?code=x&state=forged-state",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/app/login?error=oauth"));
    assert!(cookie.is_none());
}
