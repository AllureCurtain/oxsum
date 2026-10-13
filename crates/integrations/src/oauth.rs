//! The GitHub OAuth provider client (issue #152, roadmap P6-2): the three legs
//! of the authorization-code flow as plain functions over `reqwest`, with the
//! two base URLs overridable so a test can stand a stub where
//! `github.com`/`api.github.com` would be.
//!
//! The account the callback resolves by is the provider's stable user id, and
//! the email is GitHub's own primary-and-verified report — an unverified
//! address is never trusted to link an account, because anyone can type
//! somebody else's address into a provider profile.

use oxsum_core::OAuthIdentity;
use serde::Deserialize;

/// The provider name as `oauth_accounts.provider` stores it and the route
/// names it — one string, both ends.
pub const PROVIDER: &str = "github";

/// The scope the authorize redirect asks for: the profile's stable id, and the
/// email list so a verified address can be found when the profile's public
/// email field is empty.
const SCOPE: &str = "read:user user:email";

/// A configured GitHub OAuth app: the client pair, the public origin the
/// callback URL is built from, and the provider's two base URLs. Constructed
/// once at startup; `Config` holds it in an `Option`.
#[derive(Clone)]
pub struct GitHub {
    client_id: String,
    client_secret: String,
    public_url: String,
    /// `https://github.com` normally — the authorize page and the token
    /// exchange share the web origin. Overridable for a stub.
    web_url: String,
    /// `https://api.github.com` normally. Overridable for a stub.
    api_url: String,
}

impl std::fmt::Debug for GitHub {
    /// `Config` derives `Debug`, so the client pair would print with it — the
    /// secret is redacted here rather than trusting nobody ever logs a Config.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHub")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("public_url", &self.public_url)
            .field("web_url", &self.web_url)
            .field("api_url", &self.api_url)
            .finish()
    }
}

/// A leg of the provider round-trip failed. The callback redirects the browser
/// back to `/login` whatever went wrong, so this carries a message for the
/// logs rather than an API shape.
pub struct ProviderError(pub String);

impl GitHub {
    /// Builds the client from its pieces. `public_url` is the deployment's own
    /// origin (`OXSUM_PUBLIC_URL`); the callback route is appended when the
    /// authorize URL is built.
    #[must_use]
    pub fn new(
        client_id: String,
        client_secret: String,
        public_url: String,
        web_url: String,
        api_url: String,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            public_url: public_url.trim_end_matches('/').to_owned(),
            web_url: web_url.trim_end_matches('/').to_owned(),
            api_url: api_url.trim_end_matches('/').to_owned(),
        }
    }

    /// The `Location` the start endpoint redirects to: the provider's
    /// authorize page carrying the client id, the callback this deployment
    /// answers, the scope and the minted CSRF state.
    #[must_use]
    pub fn authorize_url(&self, state: &str) -> String {
        format!(
            "{}/login/oauth/authorize?client_id={}&redirect_uri={}&scope={}&state={}",
            self.web_url,
            urlencoded(&self.client_id),
            urlencoded(&self.callback_url()),
            urlencoded(SCOPE),
            urlencoded(state),
        )
    }

    /// Where the provider sends the browser back: the callback route on this
    /// deployment's public origin.
    fn callback_url(&self) -> String {
        format!("{}/api/v1/auth/oauth/github/callback", self.public_url)
    }

    /// The second leg: the code for an access token. The provider answers JSON
    /// when asked (`Accept: application/json`); an error body there, or any
    /// transport failure, is a `ProviderError` — the callback logs it and sends
    /// the browser back to login either way.
    ///
    /// # Errors
    ///
    /// `ProviderError` on a transport failure, a provider error body, or a
    /// response with no `access_token`.
    pub async fn exchange(
        &self,
        http: &reqwest::Client,
        code: &str,
    ) -> Result<String, ProviderError> {
        #[derive(Deserialize)]
        struct Token {
            access_token: Option<String>,
            error: Option<String>,
            error_description: Option<String>,
        }
        let response = http
            .post(format!("{}/login/oauth/access_token", self.web_url))
            .header("Accept", "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", self.callback_url().as_str()),
            ])
            .send()
            .await
            .map_err(|e| ProviderError(format!("token exchange request failed: {e}")))?;
        let body: Token = response
            .json()
            .await
            .map_err(|e| ProviderError(format!("token exchange response unreadable: {e}")))?;
        if let Some(error) = body.error {
            let detail = body.error_description.unwrap_or_default();
            return Err(ProviderError(format!(
                "token exchange refused: {error} {detail}"
            )));
        }
        body.access_token
            .ok_or_else(|| ProviderError("token exchange answered no access_token".into()))
    }

    /// The third leg: who the token speaks for. The stable id comes from
    /// `/user`; the email comes from `/user/emails`, which reports verification
    /// — a primary and verified address wins, any other verified one follows,
    /// and no verified address at all fails the login rather than trusting an
    /// address the provider never checked.
    ///
    /// # Errors
    ///
    /// `ProviderError` on a transport failure or when the provider reports no
    /// verified email.
    pub async fn identity(
        &self,
        http: &reqwest::Client,
        token: &str,
    ) -> Result<OAuthIdentity, ProviderError> {
        #[derive(Deserialize)]
        struct User {
            id: u64,
        }
        #[derive(Deserialize)]
        struct Email {
            email: String,
            primary: bool,
            verified: bool,
        }
        let user: User = self
            .api(http, token, "/user")
            .await?
            .json()
            .await
            .map_err(|e| ProviderError(format!("user response unreadable: {e}")))?;
        let emails: Vec<Email> = self
            .api(http, token, "/user/emails")
            .await?
            .json()
            .await
            .map_err(|e| ProviderError(format!("emails response unreadable: {e}")))?;
        let email = emails
            .iter()
            .find(|e| e.primary && e.verified)
            .or_else(|| emails.iter().find(|e| e.verified))
            .map(|e| e.email.clone())
            .ok_or_else(|| ProviderError("provider reports no verified email".into()))?;
        Ok(OAuthIdentity {
            provider: PROVIDER.to_owned(),
            provider_user_id: user.id.to_string(),
            email,
        })
    }

    /// One authenticated GET against the API origin. GitHub refuses requests
    /// without a `User-Agent`; the server names itself.
    async fn api(
        &self,
        http: &reqwest::Client,
        token: &str,
        path: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let response = http
            .get(format!("{}{}", self.api_url, path))
            .bearer_auth(token)
            .header("User-Agent", "oxsum")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| ProviderError(format!("api request failed: {e}")))?;
        if !response.status().is_success() {
            return Err(ProviderError(format!(
                "api {path} answered {}",
                response.status()
            )));
        }
        Ok(response)
    }
}

/// Percent-encodes one query value — enough for the URLs this module builds
/// (`client_id`s, states and callback URLs are all safely inside the
/// unreserved-and-friends set, but a public URL needn't be).
fn urlencoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
