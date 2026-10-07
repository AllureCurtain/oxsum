//! The anti-bot check (issue #154, roadmap P6-3): Cloudflare Turnstile over
//! one `siteverify` POST — the provider was picked for being free,
//! self-hosting-friendly and tracking nothing, and the deployment pair is
//! env-gated so an install without it runs the same endpoints unchecked.
//!
//! The check is deliberately fail-closed: a widget answer the verifier calls
//! bad, or a verifier that cannot be reached, both refuse the request — an
//! unchecked registration is not a registered account. The same rule the
//! gateway applies to money admission applies to account admission.

use serde::Deserialize;

/// The widget's default script origin — the page loads it from here when the
/// check is configured.
pub const SCRIPT_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/api.js";

/// A configured Turnstile pair: the site key the page hands the widget, the
/// secret `siteverify` authenticates with, and the verify endpoint — the
/// Cloudflare URL normally, a stub in tests.
#[derive(Clone)]
pub struct Turnstile {
    site_key: String,
    secret_key: String,
    verify_url: String,
}

/// The verifier call failed at transport level. The route turns it into 503 —
/// distinct from a token the verifier judged, which is 403.
pub struct VerifyError(pub String);

impl Turnstile {
    #[must_use]
    pub fn new(site_key: String, secret_key: String, verify_url: String) -> Self {
        Self {
            site_key,
            secret_key,
            verify_url,
        }
    }

    /// The public site key — `auth/methods` publishes it, because the page
    /// needs it to render the widget; a site key is designed to be public.
    #[must_use]
    pub fn site_key(&self) -> &str {
        &self.site_key
    }

    /// Asks the verifier whether a widget answer is genuine: `secret` +
    /// `response` in, `success` out. A refusal is `Ok(false)` — the caller
    /// rejects the request the token rode in on.
    ///
    /// # Errors
    ///
    /// `VerifyError` when the verifier cannot be reached or answers
    /// unreadably — the caller fails closed.
    pub async fn verify(&self, http: &reqwest::Client, token: &str) -> Result<bool, VerifyError> {
        #[derive(Deserialize)]
        struct Verdict {
            success: bool,
        }
        let verdict: Verdict = http
            .post(&self.verify_url)
            .json(&serde_json::json!({
                "secret": self.secret_key,
                "response": token,
            }))
            .send()
            .await
            .map_err(|e| VerifyError(format!("siteverify request failed: {e}")))?
            .json()
            .await
            .map_err(|e| VerifyError(format!("siteverify response unreadable: {e}")))?;
        Ok(verdict.success)
    }
}

impl std::fmt::Debug for Turnstile {
    /// `Config` derives `Debug`; the secret key is redacted rather than
    /// trusting nobody ever logs a Config.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Turnstile")
            .field("site_key", &self.site_key)
            .field("secret_key", &"<redacted>")
            .field("verify_url", &self.verify_url)
            .finish()
    }
}
