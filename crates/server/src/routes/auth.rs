use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use oxsum_core::{CreatedSession, LoginOutcome, NewUser, Principal, Registration};

use crate::auth::session_cookie_value;
use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, Data, SessionInfo, clear_session_cookie, ok, session_cookie};
use crate::{AppState, Signup, today};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegisterReq {
    email: String,
    password: String,
    organization_name: Option<String>,
    /// The Turnstile widget's answer — required when the deployment configures
    /// the anti-bot check, ignored when it does not (issue #154).
    turnstile_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoginReq {
    email: String,
    password: String,
}

/// The logout answer: empty, because the logout is in the `Set-Cookie` header that clears
/// the cookie.
#[derive(Serialize)]
pub(crate) struct LogoutRes {}
pub(crate) async fn register(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<RegisterReq>,
) -> ApiResult<Registration> {
    if state.config.signup() != Signup::Open {
        return Err(ApiError::Forbidden(
            "this deployment registers by invitation only".into(),
        ));
    }
    require_bot_check(&state, r.turnstile_token.as_deref()).await?;
    let mut registration = state
        .db
        .register(NewUser {
            email: r.email,
            password: r.password,
            organization_name: r.organization_name,
        })
        .await?;
    grant_signup_bonus(&state, &registration).await?;
    registration.verification_sent = send_verification(&state, registration.user.id).await;
    ok(registration)
}

/// Mints a verify token and mails it — when the deployment has a mailer. A send
/// failure logs and answers `false` rather than failing the request it rode in
/// on: the account stands, the mail is retried through `auth/verify/request`.
pub(crate) async fn send_verification(state: &AppState, user_id: Uuid) -> bool {
    let Some(mailer) = state.config.mailer() else {
        return false;
    };
    let Ok(Some(minted)) = state
        .db
        .mint_email_token(user_id, oxsum_core::EmailPurpose::Verify)
        .await
    else {
        return false;
    };
    if let Err(error) = mailer.send_verification(&minted.email, &minted.token).await {
        tracing::warn!(%error, "the verification mail was not sent");
        return false;
    }
    true
}

/// The signup bonus: `OXSUM_SIGNUP_BONUS_MINOR` credits a brand-new organization's
/// wallet, booked as an adjustment with the registration itself as its reason. Only
/// self-registration grants one — an invitation's redeem joins an existing
/// organization and creates nothing to endow. Idempotent under the organization's
/// id, and a nonzero bonus is what makes registration the ledger's first use
/// rather than the first paid request.
async fn grant_signup_bonus(state: &AppState, registration: &Registration) -> Result<(), ApiError> {
    let bonus = state.config.signup_bonus_minor();
    if bonus == 0 {
        return Ok(());
    }
    let organization = &registration.organization;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    wallet
        .adjust(
            &format!("signup-bonus:{}", organization.id),
            "signup bonus",
            bonus,
            today(),
        )
        .await?;
    Ok(())
}

/// Logs in: verifies the password, then either mints a session in a cookie or —
/// for a TOTP-enabled account — withholds it behind a second-factor challenge
/// (issue #504): `requiresTotp` answers the challenge the client completes at
/// `POST /auth/totp`, and carries no cookie.
///
/// An unknown email and a wrong password fail identically (401, one message), so neither
/// reveals whether an account exists.
pub(crate) async fn login(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<LoginReq>,
) -> Result<Response, ApiError> {
    match state.db.login(&r.email, &r.password).await? {
        LoginOutcome::Session(created) => {
            let CreatedSession { principal, token } = *created;
            let mut response = Json(Data {
                data: SessionInfo::from(principal),
            })
            .into_response();
            response.headers_mut().insert(
                SET_COOKIE,
                HeaderValue::from_str(&session_cookie(
                    &token,
                    state.config.session_cookie_secure(),
                ))
                .map_err(|_| ApiError::Internal)?,
            );
            Ok(response)
        }
        LoginOutcome::TotpRequired { challenge_token } => Ok(Json(Data {
            data: json!({
                "requiresTotp": true,
                "challengeToken": challenge_token,
            }),
        })
        .into_response()),
    }
}

/// The body of `POST /api/v1/auth/totp`: the challenge `login` withheld the
/// session behind, plus the live six-digit code — or a single-use backup code.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TotpLoginReq {
    challenge_token: String,
    code: String,
}

/// The login's second leg: a challenge plus a live code (or a backup code,
/// consumed on use) mints the session the password leg withheld.
pub(crate) async fn totp_login(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<TotpLoginReq>,
) -> Result<Response, ApiError> {
    let Some(secret) = state.config.secret() else {
        return Err(ApiError::ServiceUnavailable(
            "the second factor is not configured on this deployment".into(),
        ));
    };
    let CreatedSession { principal, token } = state
        .db
        .verify_totp(&r.challenge_token, &r.code, secret)
        .await?;
    let mut response = Json(Data {
        data: SessionInfo::from(principal),
    })
    .into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session_cookie(
            &token,
            state.config.session_cookie_secure(),
        ))
        .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

/// What `POST /auth/totp/enroll` answers: the shared secret and the otpauth URL
/// the client renders a QR from — the secret is the credential to protect.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TotpEnrollRes {
    secret: String,
    otpauth_url: String,
}

/// Begins second-factor enrollment: a fresh secret, sealed and held
/// unconfirmed until `confirm` proves a live code against it.
pub(crate) async fn totp_enroll(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<TotpEnrollRes> {
    let Some(user) = principal.user() else {
        return Err(ApiError::Forbidden(
            "a second factor is a person's setting: an API key names no user".into(),
        ));
    };
    let Some(secret) = state.config.secret() else {
        return Err(ApiError::ServiceUnavailable(
            "the second factor is not configured on this deployment".into(),
        ));
    };
    let raw = oxsum_core::totp::generate_secret();
    let sealed = secret
        .seal(&oxsum_core::totp::base32_encode_pub(&raw))
        .map_err(|_| ApiError::Internal)?;
    state.db.enroll_totp(user.id, &sealed).await?;
    let b32 = oxsum_core::totp::base32_encode_pub(&raw);
    ok(TotpEnrollRes {
        secret: b32.clone(),
        otpauth_url: oxsum_core::totp::otpauth_url("oxsum", &user.email, &raw),
    })
}

/// The body of `POST /auth/totp/confirm` and `/disable`: one live code.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TotpCodeReq {
    code: String,
}

/// What `confirm` answers: the single-use backup codes, shown exactly once.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TotpBackupRes {
    backup_codes: Vec<String>,
}

/// Confirms enrollment on a first live code: proves the authenticator scanned
/// the secret, then mints and answers the backup codes — once, in the clear.
pub(crate) async fn totp_confirm(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<TotpCodeReq>,
) -> ApiResult<TotpBackupRes> {
    let Some(user) = principal.user() else {
        return Err(ApiError::Forbidden(
            "a second factor is a person's setting: an API key names no user".into(),
        ));
    };
    let Some(secret) = state.config.secret() else {
        return Err(ApiError::ServiceUnavailable(
            "the second factor is not configured on this deployment".into(),
        ));
    };
    let codes = state.db.confirm_totp(user.id, &r.code, secret).await?;
    ok(TotpBackupRes {
        backup_codes: codes,
    })
}

/// Disables the factor on a live code — removing it is itself second-factored.
pub(crate) async fn totp_disable(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<TotpCodeReq>,
) -> ApiResult<LogoutRes> {
    let Some(user) = principal.user() else {
        return Err(ApiError::Forbidden(
            "a second factor is a person's setting: an API key names no user".into(),
        ));
    };
    let Some(secret) = state.config.secret() else {
        return Err(ApiError::ServiceUnavailable(
            "the second factor is not configured on this deployment".into(),
        ));
    };
    state.db.disable_totp(user.id, &r.code, secret).await?;
    ok(LogoutRes {})
}

/// Logs out: revokes the session the cookie names and clears the cookie.
///
/// Always 200: logging out twice is not an error, and neither is logging out without a
/// session at all.
pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = session_cookie_value(&headers) {
        state.db.logout(&token).await?;
    }
    let mut response = Json(Data { data: LogoutRes {} }).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&clear_session_cookie(state.config.session_cookie_secure()))
            .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

/// The `Set-Cookie` value for a session: `HttpOnly`, `SameSite=Lax`, `Path=/`, and `Secure`
/// when the deployment says so (`OXSUM_SESSION_COOKIE_SECURE`, documented in
/// docs/development.md).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthMethods {
    oauth_github: bool,
    /// The configured OIDC providers' route names (`oidc:{name}`), so the login
    /// page draws one button per IdP the deployment federates to (issue #504).
    oidc_providers: Vec<String>,
    turnstile_site_key: Option<String>,
}

/// The login page's discovery read: no credential, because it runs before the
/// user has one to present.
pub(crate) async fn auth_methods(State(state): State<AppState>) -> Json<Data<AuthMethods>> {
    Json(Data {
        data: AuthMethods {
            oauth_github: state.config.github().is_some(),
            oidc_providers: state
                .config
                .oidc_providers()
                .iter()
                .map(|p| crate::oidc::provider_name(p.name()))
                .collect(),
            turnstile_site_key: state.config.turnstile().map(|t| t.site_key().to_owned()),
        },
    })
}

/// The env-gated anti-bot check both account-creation endpoints run (issue
/// #154). Unconfigured, it is a no-op; configured, a missing answer or a
/// `success: false` verdict is `FORBIDDEN`, and a verifier that cannot be
/// reached is `SERVICE_UNAVAILABLE` — the check never fails open.
pub(crate) async fn require_bot_check(
    state: &AppState,
    token: Option<&str>,
) -> Result<(), ApiError> {
    let Some(turnstile) = state.config.turnstile() else {
        return Ok(());
    };
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Err(ApiError::Forbidden("the anti-bot check is required".into()));
    };
    match turnstile.verify(&state.http, token).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(ApiError::Forbidden(
            "the anti-bot check did not pass".into(),
        )),
        Err(why) => {
            tracing::warn!(error = %why.0, "the anti-bot verifier could not be reached");
            Err(ApiError::ServiceUnavailable(
                "the anti-bot check could not be reached".into(),
            ))
        }
    }
}

/// Begins an OAuth login on the named provider — `github`, or `oidc:{name}`
/// for one of the deployment's configured IdPs (issue #504). The answer is a
/// `Location` to the provider's authorize page; an unconfigured or unknown
/// provider is a 404 like every unknown path.
pub(crate) async fn oauth_start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Response, ApiError> {
    if provider == crate::oauth::PROVIDER {
        let Some(github) = state.config.github() else {
            return Err(ApiError::NotFound("not found".into()));
        };
        let oauth_state = state.db.mint_oauth_state(&provider).await?;
        return Ok(Redirect::to(&github.authorize_url(&oauth_state)).into_response());
    }
    // An OIDC provider: discover, mint the PKCE pair, and redirect to the
    // authorize page carrying the S256 challenge.
    let Some(oidc) = state.config.oidc_provider(&provider).cloned() else {
        return Err(ApiError::NotFound("not found".into()));
    };
    let (verifier, challenge, nonce) = crate::oidc::pkce_pair();
    let oauth_state = state
        .db
        .mint_oauth_state_pkce(&provider, &verifier, &nonce)
        .await?;
    let url = oidc
        .authorize_url(&state.http, &oauth_state, &challenge, &nonce)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e.0, provider = %provider, "oidc authorize failed");
            ApiError::ServiceUnavailable("the identity provider could not be reached".into())
        })?;
    Ok(Redirect::to(&url).into_response())
}

/// What the provider's redirect carries: `code` and `state` on success,
/// `error` when it refuses (the user declined, the app was mis-registered).
#[derive(Deserialize)]
pub(crate) struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Finishes an OAuth login on the named provider. The caller is a browser
/// mid-redirect, so the answer is always a `Location` — `/app` with the
/// session cookie on success, `/app/login?error=oauth` on any failure; the
/// detail goes to the logs, never to the query string the page would show.
pub(crate) async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    ApiQuery(query): ApiQuery<OAuthCallbackQuery>,
) -> Response {
    match oauth_finish(&state, &provider, &query).await {
        Ok(response) => response,
        Err(why) => {
            tracing::warn!(error = %why, provider = %provider, "oauth login failed");
            Redirect::to("/app/login?error=oauth").into_response()
        }
    }
}

/// The callback's legs, each one a possible refusal: the provider's own
/// `error`, a spent or expired state, the token exchange, the identity's own
/// check (GitHub's verified email; OIDC's JWKS verify and `nonce`), and the
/// account resolution — which itself refuses when the deployment registers by
/// invitation only and the identity is new.
async fn oauth_finish(
    state: &AppState,
    provider: &str,
    query: &OAuthCallbackQuery,
) -> Result<Response, String> {
    if let Some(error) = &query.error {
        return Err(format!("provider refused: {error}"));
    }
    let (Some(code), Some(oauth_state)) = (&query.code, &query.state) else {
        return Err("callback carried no code or state".to_owned());
    };
    // The identity comes from the provider's own leg: GitHub's access-token +
    // userinfo, or the OIDC `id_token` verified against the consumed state's
    // PKCE verifier and nonce.
    let identity = if provider == crate::oauth::PROVIDER {
        let Some(github) = state.config.github() else {
            return Err("provider not configured".to_owned());
        };
        let spent = state
            .db
            .consume_oauth_state(oauth_state, provider)
            .await
            .map_err(|e| e.to_string())?;
        if !spent {
            return Err("unknown, spent or expired state".to_owned());
        }
        let token = github.exchange(&state.http, code).await.map_err(|e| e.0)?;
        github
            .identity(&state.http, &token)
            .await
            .map_err(|e| e.0)?
    } else {
        let Some(oidc) = state.config.oidc_provider(provider) else {
            return Err("provider not configured".to_owned());
        };
        let consumed = state
            .db
            .consume_oauth_state_full(oauth_state, provider)
            .await
            .map_err(|e| e.to_string())?;
        let Some(consumed) = consumed else {
            return Err("unknown, spent or expired state".to_owned());
        };
        let (Some(verifier), Some(nonce)) = (consumed.pkce_verifier, consumed.nonce) else {
            return Err("the consumed state held no PKCE pair".to_owned());
        };
        let id_token = oidc
            .exchange(&state.http, code, &verifier)
            .await
            .map_err(|e| e.0)?;
        oidc.identity(&state.http, &id_token, &nonce)
            .await
            .map_err(|e| e.0)?
    };
    let created = state
        .db
        .oauth_login(&identity, state.config.signup() == Signup::Open)
        .await
        .map_err(|e| e.to_string())?;
    let mut response = Redirect::to("/app").into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session_cookie(
            &created.token,
            state.config.session_cookie_secure(),
        ))
        .map_err(|_| "cookie header rejected the token".to_owned())?,
    );
    Ok(response)
}

/// The current session: who is logged in, as which organization, in which role.
///
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerifyRequestRes {
    sent: bool,
    already_verified: bool,
}

/// Mails the session user a verification link. The one email-flow endpoint that
/// reports a missing mailer: sending the mail is its whole job (`SERVICE_UNAVAILABLE`).
/// A resend inside the sixty-second cooldown mints and mails nothing (`sent: false`).
pub(crate) async fn request_verification(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<VerifyRequestRes> {
    let Some(user) = principal.user() else {
        return Err(ApiError::Forbidden(
            "verifying an email is a person's action: an API key names no user".into(),
        ));
    };
    let Some(mailer) = state.config.mailer() else {
        return Err(ApiError::ServiceUnavailable(
            "email is not configured on this deployment".into(),
        ));
    };
    if user.email_verified {
        return ok(VerifyRequestRes {
            sent: false,
            already_verified: true,
        });
    }
    let Some(minted) = state
        .db
        .mint_email_token(user.id, oxsum_core::EmailPurpose::Verify)
        .await?
    else {
        return ok(VerifyRequestRes {
            sent: false,
            already_verified: false,
        });
    };
    mailer
        .send_verification(&minted.email, &minted.token)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "the verification mail was not sent");
            ApiError::Internal
        })?;
    ok(VerifyRequestRes {
        sent: true,
        already_verified: false,
    })
}

/// The body of `POST /api/v1/auth/verify`: the mailed token.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct VerifyEmailReq {
    token: String,
}

/// What `POST /api/v1/auth/verify` answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerifiedRes {
    verified: bool,
}

/// Consumes a verification token and marks its user's email verified. An unknown,
/// spent or expired token is `NOT_FOUND` — which is wrong is the holder's business.
pub(crate) async fn verify_email(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<VerifyEmailReq>,
) -> ApiResult<VerifiedRes> {
    let Some(user_id) = state
        .db
        .consume_email_token(&r.token, oxsum_core::EmailPurpose::Verify)
        .await?
    else {
        return Err(ApiError::NotFound("not found".into()));
    };
    state.db.mark_email_verified(user_id).await?;
    ok(VerifiedRes { verified: true })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ForgotPasswordReq {
    email: String,
}

/// Mails a reset link when the address has an account — and answers the same
/// empty 200 when it does not, so the endpoint enumerates no accounts.
pub(crate) async fn forgot_password(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<ForgotPasswordReq>,
) -> Result<Response, ApiError> {
    if let Some(mailer) = state.config.mailer()
        && let Some(minted) = state
            .db
            .mint_email_token_for_address(&r.email, oxsum_core::EmailPurpose::Reset)
            .await?
        && let Err(error) = mailer
            .send_password_reset(&minted.email, &minted.token)
            .await
    {
        // A send failure still answers 200: the answer must not say whether the
        // account exists, and the next request mints a fresh link.
        tracing::warn!(%error, "the reset mail was not sent");
    }
    Ok(Json(Data { data: json!({}) }).into_response())
}

/// The body of `POST /api/v1/auth/password/reset`: the mailed token and the new
/// password.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResetPasswordReq {
    token: String,
    password: String,
}

/// What `POST /api/v1/auth/password/reset` answers: how many sessions died with
/// the old password.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetPasswordRes {
    sessions_revoked: u64,
}

/// Consumes a reset token, sets the new password and revokes every session the
/// user holds — the admin reset's semantics, self-served through the mail.
pub(crate) async fn reset_password(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<ResetPasswordReq>,
) -> ApiResult<ResetPasswordRes> {
    let Some(user_id) = state
        .db
        .consume_email_token(&r.token, oxsum_core::EmailPurpose::Reset)
        .await?
    else {
        return Err(ApiError::NotFound("not found".into()));
    };
    let sessions_revoked = state.db.reset_password(user_id, &r.password).await?;
    ok(ResetPasswordRes { sessions_revoked })
}
