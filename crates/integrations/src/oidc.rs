//! The generic OIDC provider client (issue #504): the `code+PKCE` flow an
//! enterprise IdP (Okta, Keycloak, Authentik) speaks, as a sibling to the
//! GitHub [`crate::oauth`] client — same three legs (authorize URL, token
//! exchange, identity), but the identity is read from the verified `id_token`
//! rather than a userinfo call, and the exchange proves possession of the
//! PKCE verifier minted at authorize time.
//!
//! Each configured provider is a named entry under `oxsum.oauth_accounts`:
//! `provider` is `oidc:{name}` so a `sub` — which is only unique per issuer —
//! never collides across IdPs. Discovery is fetched once per provider and
//! cached; a provider that cannot discover cannot authorize either, which the
//! route surfaces as a login failure, not a startup error (an IdP that is
//! down must not refuse the whole deployment to boot).

use oxsum_core::OAuthIdentity;
use serde::Deserialize;

use crate::oauth::urlencoded;

/// A configured OIDC provider from `OXSUM_OIDC_PROVIDERS`: the issuer (which
/// owns the `sub` namespace and the discovery document), the client pair, and
/// this deployment's public origin the callback URL is built from.
pub struct OidcProvider {
    name: String,
    issuer: String,
    client_id: String,
    client_secret: String,
    public_url: String,
    /// The fetched discovery document, cached on first use — an IdP's
    /// endpoints are stable, so re-fetching per login would only add latency
    /// to the redirect the user waits on.
    discovery: tokio::sync::OnceCell<OidcDiscovery>,
}

impl std::fmt::Debug for OidcProvider {
    /// `Config` derives `Debug`, so the client pair would print with it — the
    /// secret is redacted here, the way `GitHub`'s is.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcProvider")
            .field("name", &self.name)
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("public_url", &self.public_url)
            .finish()
    }
}

impl Clone for OidcProvider {
    /// `Config` derives `Clone` (its `with_*` test builders), so the providers
    /// must clone too. A clone re-discovers on its first authorize — the cached
    /// document is per-cell, and the clone is rare enough that refetching beats
    /// sharing the cell behind an `Arc`.
    fn clone(&self) -> Self {
        Self::new(
            self.name.clone(),
            self.issuer.clone(),
            self.client_id.clone(),
            self.client_secret.clone(),
            self.public_url.clone(),
        )
    }
}

/// A leg of the OIDC round-trip failed. The callback redirects the browser
/// back to `/login` whatever went wrong, so this carries a message for the
/// logs rather than an API shape — same contract as [`crate::oauth::ProviderError`].
pub struct OidcError(pub String);

/// The provider's endpoints, read off `.well-known/openid-configuration`.
#[derive(Debug, Clone, Deserialize)]
pub struct OidcDiscovery {
    /// Where the authorize redirect goes.
    pub authorization_endpoint: String,
    /// Where the code is exchanged.
    pub token_endpoint: String,
    /// Where the `id_token` signing keys are published.
    pub jwks_uri: String,
}

/// The `id_token` claims the login reads — `iss`/`aud`/`exp` are absent here
/// because `jsonwebtoken::Validation` already verified them; what the code
/// still reads is who the token names and whether the nonce is the login's.
#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    sub: String,
    nonce: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
}

/// A JWK as the provider's JWKS document lists it — RSA signing keys only.
#[derive(Debug, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

/// The RSA public half, in JWK's base64url-unsigned-integer form.
#[derive(Debug, Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    n: String,
    e: String,
}

/// The provider name as `oauth_accounts.provider` stores it: `oidc:{name}`,
/// so each configured IdP's `sub` namespace stays distinct.
pub fn provider_name(name: &str) -> String {
    format!("oidc:{name}")
}

/// Mints the PKCE pair and the `nonce` an OIDC authorize round-trip needs
/// (RFC 7636): `(code_verifier, code_challenge, nonce)`. The verifier and
/// nonce are fresh CSPRNG bytes, base64url'd; the challenge is the base64url
/// of the verifier's SHA-256 — the S256 transform, so the verifier itself
/// never crosses the redirect the browser carries.
pub fn pkce_pair() -> (String, String, String) {
    use base64::Engine as _;
    use rand::RngExt as _;
    use sha2::Digest as _;
    let encode = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut verifier_bytes = [0u8; 32];
    rand::rng().fill(&mut verifier_bytes);
    let mut nonce_bytes = [0u8; 32];
    rand::rng().fill(&mut nonce_bytes);
    let verifier = encode.encode(verifier_bytes);
    let challenge = encode.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let nonce = encode.encode(nonce_bytes);
    (verifier, challenge, nonce)
}

impl OidcProvider {
    /// Builds the provider from its configured pieces.
    pub fn new(
        name: String,
        issuer: String,
        client_id: String,
        client_secret: String,
        public_url: String,
    ) -> Self {
        Self {
            name,
            issuer: issuer.trim_end_matches('/').to_owned(),
            client_id,
            client_secret,
            public_url: public_url.trim_end_matches('/').to_owned(),
            discovery: tokio::sync::OnceCell::new(),
        }
    }

    /// The configured provider's name — what `provider_name` scopes.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Fetches (once) the provider's discovery document.
    ///
    /// # Errors
    ///
    /// `OidcError` on a transport failure, a non-2xx, or a document missing the
    /// endpoints the flow needs.
    async fn discovery(&self, http: &reqwest::Client) -> Result<&OidcDiscovery, OidcError> {
        self.discovery
            .get_or_try_init(|| async {
                let url = format!("{}/.well-known/openid-configuration", self.issuer);
                let response = http
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| OidcError(format!("discovery request failed: {e}")))?;
                if !response.status().is_success() {
                    return Err(OidcError(format!(
                        "discovery answered {}",
                        response.status()
                    )));
                }
                let doc: OidcDiscovery = response
                    .json()
                    .await
                    .map_err(|e| OidcError(format!("discovery unreadable: {e}")))?;
                if doc.authorization_endpoint.is_empty()
                    || doc.token_endpoint.is_empty()
                    || doc.jwks_uri.is_empty()
                {
                    return Err(OidcError(
                        "discovery document lacks the endpoints the flow needs".into(),
                    ));
                }
                Ok(doc)
            })
            .await
    }

    /// Where the provider sends the browser back: the callback route on this
    /// deployment's public origin, named by the provider.
    fn callback_url(&self) -> String {
        format!(
            "{}/api/v1/auth/oauth/{}/callback",
            self.public_url,
            provider_name(&self.name)
        )
    }

    /// The `Location` the start endpoint redirects to: the provider's
    /// authorize page carrying the client id, the callback, the OIDC scope,
    /// the minted CSRF state and the S256 PKCE challenge.
    ///
    /// # Errors
    ///
    /// `OidcError` when discovery cannot be fetched.
    pub async fn authorize_url(
        &self,
        http: &reqwest::Client,
        state: &str,
        pkce_challenge: &str,
        nonce: &str,
    ) -> Result<String, OidcError> {
        let discovery = self.discovery(http).await?;
        Ok(format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
            discovery.authorization_endpoint,
            urlencoded(&self.client_id),
            urlencoded(&self.callback_url()),
            urlencoded("openid email profile"),
            urlencoded(state),
            urlencoded(nonce),
            urlencoded(pkce_challenge),
        ))
    }

    /// The second leg: the code plus the server-held `code_verifier` for an
    /// `id_token`. The exchange proves the deployment still holds the verifier
    /// the authorize challenge was built from — a stolen code without the
    /// verifier is worthless to whoever intercepted the redirect.
    ///
    /// # Errors
    ///
    /// `OidcError` on discovery, transport, a provider error body, or a
    /// response with no `id_token`.
    pub async fn exchange(
        &self,
        http: &reqwest::Client,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<String, OidcError> {
        let discovery = self.discovery(http).await?;
        #[derive(Deserialize)]
        struct Token {
            id_token: Option<String>,
            error: Option<String>,
            error_description: Option<String>,
        }
        let response = http
            .post(&discovery.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", self.callback_url().as_str()),
                ("code_verifier", pkce_verifier),
            ])
            .send()
            .await
            .map_err(|e| OidcError(format!("token exchange request failed: {e}")))?;
        let body: Token = response
            .json()
            .await
            .map_err(|e| OidcError(format!("token exchange response unreadable: {e}")))?;
        if let Some(error) = body.error {
            let detail = body.error_description.unwrap_or_default();
            return Err(OidcError(format!(
                "token exchange refused: {error} {detail}"
            )));
        }
        body.id_token
            .ok_or_else(|| OidcError("token exchange answered no id_token".into()))
    }

    /// The third leg: who the verified `id_token` speaks for. The signature is
    /// checked against the provider's JWKS, `iss`/`aud`/`exp`/`nonce` are
    /// validated, and the email must be verified — the same rule GitHub's
    /// verified-email requirement keeps, because anyone can type somebody
    /// else's address into a provider profile.
    ///
    /// # Errors
    ///
    /// `OidcError` on a JWKS/validation failure or an unverified email.
    pub async fn identity(
        &self,
        http: &reqwest::Client,
        id_token: &str,
        expected_nonce: &str,
    ) -> Result<OAuthIdentity, OidcError> {
        let discovery = self.discovery(http).await?;
        let claims = verify_id_token(
            http,
            discovery,
            id_token,
            &self.issuer,
            &self.client_id,
            expected_nonce,
        )
        .await?;
        let email = claims.email.unwrap_or_default();
        if email.is_empty() || claims.email_verified != Some(true) {
            return Err(OidcError("provider reports no verified email".into()));
        }
        Ok(OAuthIdentity {
            provider: provider_name(&self.name),
            provider_user_id: claims.sub,
            email,
        })
    }
}

/// Verifies an `id_token` against the provider's JWKS and the registered
/// claims. The token must be an RS256 JWS signed by one of the issuer's keys;
/// `iss`, `aud` (a string or an array containing the client id), `exp` and the
/// echoed `nonce` are all checked — a token minted for a different login or a
/// different client verifies nothing here.
async fn verify_id_token(
    http: &reqwest::Client,
    discovery: &OidcDiscovery,
    id_token: &str,
    issuer: &str,
    client_id: &str,
    expected_nonce: &str,
) -> Result<IdTokenClaims, OidcError> {
    // The token's `kid` picks the signing key; a header with no `kid` falls
    // back to trying every RSA key the document lists.
    let header = jsonwebtoken::decode_header(id_token)
        .map_err(|e| OidcError(format!("id_token header unreadable: {e}")))?;
    let jwks: Jwks = http
        .get(&discovery.jwks_uri)
        .send()
        .await
        .map_err(|e| OidcError(format!("jwks request failed: {e}")))?
        .json()
        .await
        .map_err(|e| OidcError(format!("jwks unreadable: {e}")))?;

    let keys: Vec<&Jwk> = jwks
        .keys
        .iter()
        .filter(|k| {
            k.kty == "RSA"
                && header
                    .kid
                    .as_ref()
                    .is_none_or(|kid| Some(kid) == k.kid.as_ref())
        })
        .collect();
    if keys.is_empty() {
        return Err(OidcError("no JWKS key matches the id_token".into()));
    }

    let mut last_error = String::new();
    for jwk in keys {
        match decode_with_jwk(jwk, id_token, issuer, client_id) {
            Ok(claims) => {
                if claims.nonce.as_deref() != Some(expected_nonce) {
                    return Err(OidcError(
                        "id_token nonce does not match the login that asked for it".into(),
                    ));
                }
                return Ok(claims);
            }
            Err(error) => last_error = error,
        }
    }
    Err(OidcError(format!("id_token did not verify: {last_error}")))
}

/// Decodes and validates one candidate key: RS256 signature, `iss`, `aud`
/// (string or array) and `exp`.
fn decode_with_jwk(
    jwk: &Jwk,
    id_token: &str,
    issuer: &str,
    client_id: &str,
) -> Result<IdTokenClaims, String> {
    let key = jsonwebtoken::DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
        .map_err(|e| format!("jwk rejected: {e}"))?;
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_issuer(&[issuer]);
    // `aud` is compared manually below because it is legitimately a string or
    // an array; jsonwebtoken's own audience check handles only the list form.
    validation.set_audience(&[client_id]);
    validation.validate_exp = true;
    validation.leeway = 60;
    let data = jsonwebtoken::decode::<serde_json::Value>(id_token, &key, &validation)
        .map_err(|e| format!("{e}"))?;
    let claims: IdTokenClaims = serde_json::from_value(data.claims)
        .map_err(|e| format!("id_token claims unreadable: {e}"))?;
    Ok(claims)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_provider_name_scopes_the_sub() {
        assert_eq!(provider_name("okta"), "oidc:okta");
        assert_eq!(provider_name("corp-idp"), "oidc:corp-idp");
    }
}
