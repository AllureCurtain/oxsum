//! Deployment configuration.
//!
//! Not secrets and not data: these are the few switches a deployer sets, and they shape how
//! a request is handled. Everything else the server needs is an environment variable read
//! once at startup, in main.rs.

use std::time::Duration;

use oxsum_core::{DEFAULT_HOLD_TIMEOUT, PriceBook, SecretKey};

/// Whether this deployment lets people register themselves.
///
/// product.md: `invite` is the default, because a public instance should not let whoever
/// arrives first create an account; `open` is what a demo or a private instance wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signup {
    Invite,
    Open,
}

impl Signup {
    /// Parses `OXSUM_SIGNUP`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "invite" => Ok(Self::Invite),
            "open" => Ok(Self::Open),
            other => Err(format!(
                "OXSUM_SIGNUP must be invite or open, got {other:?}"
            )),
        }
    }
}

/// The first upstream channel a deployment starts with, and what its models cost.
///
/// In v1 one model maps to exactly one channel (product.md), so a deployment has exactly one of
/// these. It is the *bootstrap*: [`crate::app`] writes it into `oxsum.channels` when the database has
/// no channels yet, and after that the rows are the configuration (docs/decisions.md, "channels and
/// versioned prices"). A deployment can therefore describe its first channel here and change its
/// prices over the admin API, without ever editing the environment again.
#[derive(Debug, Clone)]
pub struct Gateway {
    base_url: String,
    api_key: String,
    book: PriceBook,
}

impl Gateway {
    /// A channel described in code, which is what the tests build.
    #[must_use]
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, book: PriceBook) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            book,
        }
    }

    /// Reads the bootstrap channel from the environment.
    ///
    /// `None` means the environment describes no channel, which is a valid way to run the wallet and
    /// also what a deployment does after it has configured channels over the admin API: the three
    /// variables are all absent and nothing is seeded.
    ///
    /// # Errors
    ///
    /// Refuses a half-configured channel, an address that is not an http(s) URL, and a model list
    /// [`PriceBook::from_json`] refuses. The server does not start rather than serve prices it
    /// would have had to guess.
    pub fn from_env() -> Result<Option<Self>, String> {
        let base_url = var("OXSUM_UPSTREAM_BASE_URL");
        let api_key = var("OXSUM_UPSTREAM_API_KEY");
        let models = var("OXSUM_MODELS");
        let parts = [
            (base_url.is_some(), "OXSUM_UPSTREAM_BASE_URL"),
            (api_key.is_some(), "OXSUM_UPSTREAM_API_KEY"),
            (models.is_some(), "OXSUM_MODELS"),
        ];
        if parts.iter().all(|(present, _)| !present) {
            return Ok(None);
        }
        if let Some((_, missing)) = parts.iter().find(|(present, _)| !present) {
            return Err(format!(
                "{missing} is not set: a gateway channel needs OXSUM_UPSTREAM_BASE_URL, \
                 OXSUM_UPSTREAM_API_KEY and OXSUM_MODELS together, or none of them"
            ));
        }
        let (base_url, api_key, models) = (
            base_url.unwrap_or_default(),
            api_key.unwrap_or_default(),
            models.unwrap_or_default(),
        );
        let url = reqwest::Url::parse(&base_url)
            .map_err(|error| format!("OXSUM_UPSTREAM_BASE_URL is not a URL: {error}"))?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(format!(
                "OXSUM_UPSTREAM_BASE_URL must be http or https, got {:?}",
                url.scheme()
            ));
        }
        let channel = var("OXSUM_UPSTREAM_NAME").unwrap_or_else(|| "upstream".to_owned());
        let book = PriceBook::from_json(&channel, &models)?;
        Ok(Some(Self::new(base_url, api_key, book)))
    }

    /// The channel's base URL, without a trailing slash. `/chat/completions` is appended to it.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The upstream credential, sent as `Authorization: Bearer …`.
    #[must_use]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// The models this channel serves, with their prices.
    #[must_use]
    pub fn book(&self) -> &PriceBook {
        &self.book
    }
}

/// An environment variable that counts as unset when it is absent or blank.
fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Deployment configuration, passed to [`crate::app`].
///
/// The gateway part is the *bootstrap* channel: channels and their prices are rows in oxsum's own
/// tables (docs/decisions.md, "channels and versioned prices"), and the environment only describes
/// what a brand-new database starts with. Everything else here is process configuration: the signup
/// policy, the key that seals channel credentials, and the operator token on the admin surface.
#[derive(Debug, Clone)]
pub struct Config {
    signup: Signup,
    gateway: Option<Gateway>,
    secret: Option<SecretKey>,
    admin_token: Option<String>,
    head_signing_seed: Option<[u8; 32]>,
    hold_timeout: Duration,
    session_cookie_secure: bool,
}

impl Config {
    #[must_use]
    pub fn new(signup: Signup, gateway: Option<Gateway>) -> Self {
        Self {
            signup,
            gateway,
            secret: None,
            admin_token: None,
            head_signing_seed: None,
            hold_timeout: DEFAULT_HOLD_TIMEOUT,
            session_cookie_secure: false,
        }
    }

    /// Sets the key that seals upstream credentials. Required as soon as a channel exists.
    #[must_use]
    pub fn with_secret(mut self, secret: SecretKey) -> Self {
        self.secret = Some(secret);
        self
    }

    /// Sets the operator token that opens `/api/v1/admin`.
    #[must_use]
    pub fn with_admin_token(mut self, token: impl Into<String>) -> Self {
        self.admin_token = Some(token.into());
        self
    }

    /// Sets the seed the operator signs tree heads with. Tests use this; the server reads
    /// `OXSUM_HEAD_SIGNING_KEY`.
    #[must_use]
    pub fn with_head_signing_seed(mut self, seed: [u8; 32]) -> Self {
        self.head_signing_seed = Some(seed);
        self
    }

    /// Sets whether the session cookie carries the `Secure` attribute. Tests use this; the
    /// server reads `OXSUM_SESSION_COOKIE_SECURE`.
    #[must_use]
    pub fn with_session_cookie_secure(mut self, secure: bool) -> Self {
        self.session_cookie_secure = secure;
        self
    }

    /// Reads the configuration from the environment.
    ///
    /// # Errors
    ///
    /// Returns a message naming the variable when a value is present but unusable; the
    /// server refuses to start rather than guess.
    pub fn from_env() -> Result<Self, String> {
        let signup = match std::env::var("OXSUM_SIGNUP") {
            Ok(raw) => Signup::parse(&raw)?,
            Err(_) => Signup::Invite,
        };
        let secret = var("OXSUM_SECRET_KEY")
            .map(|raw| SecretKey::parse(&raw))
            .transpose()?;
        let admin_token = var("OXSUM_ADMIN_TOKEN").map(admin_token_of).transpose()?;
        let head_signing_seed = var("OXSUM_HEAD_SIGNING_KEY")
            .map(head_signing_seed_of)
            .transpose()?;
        let hold_timeout = var("OXSUM_HOLD_TIMEOUT")
            .map(hold_timeout_of)
            .transpose()?
            .unwrap_or(DEFAULT_HOLD_TIMEOUT);
        let session_cookie_secure = var("OXSUM_SESSION_COOKIE_SECURE")
            .map(session_cookie_secure_of)
            .transpose()?
            .unwrap_or(false);
        Ok(Self {
            signup,
            gateway: Gateway::from_env()?,
            secret,
            admin_token,
            head_signing_seed,
            hold_timeout,
            session_cookie_secure,
        })
    }

    #[must_use]
    pub(crate) fn signup(&self) -> Signup {
        self.signup
    }

    /// The bootstrap channel, or `None` when the environment describes none.
    #[must_use]
    pub(crate) fn bootstrap(&self) -> Option<&Gateway> {
        self.gateway.as_ref()
    }

    /// The key that opens sealed channel credentials, or `None` when none is configured — in which
    /// case this deployment can hold no channels at all ([`crate::app`] enforces that).
    #[must_use]
    pub(crate) fn secret(&self) -> Option<&SecretKey> {
        self.secret.as_ref()
    }

    /// The operator token, or `None` when the admin surface is closed.
    #[must_use]
    pub(crate) fn admin_token(&self) -> Option<&str> {
        self.admin_token.as_deref()
    }

    /// The seed the operator signs tree heads with, or `None` when the deployment did not
    /// configure one — in which case the `/api/v1/log` endpoints answer 503.
    #[must_use]
    pub(crate) fn head_signing_seed(&self) -> Option<[u8; 32]> {
        self.head_signing_seed
    }

    /// How long a hold may sit unsettled before the background sweeper releases it.
    #[must_use]
    pub fn hold_timeout(&self) -> Duration {
        self.hold_timeout
    }

    /// Whether the session cookie carries the `Secure` attribute. A deployment behind TLS
    /// must enable it; local http development needs it off.
    #[must_use]
    pub(crate) fn session_cookie_secure(&self) -> bool {
        self.session_cookie_secure
    }
}

/// Reads `OXSUM_HEAD_SIGNING_KEY`: 32 bytes, base64 — the seed of the Ed25519 key the
/// operator signs tree heads with.
///
/// A bad value is refused at startup rather than accepted quietly: a head signed by a
/// key the operator cannot reproduce is a head nobody can verify.
fn head_signing_seed_of(raw: String) -> Result<[u8; 32], String> {
    oxsum_core::seed_from_base64(&raw)
}

/// A token worth putting in front of the admin surface: long enough that it is not worth guessing.
///
/// A short token is refused at startup rather than accepted quietly: the surface it guards can point
/// the gateway at another upstream and change what every request costs.
fn admin_token_of(token: String) -> Result<String, String> {
    const MIN: usize = 16;
    if token.chars().count() < MIN {
        return Err(format!(
            "OXSUM_ADMIN_TOKEN must be at least {MIN} characters, got {}",
            token.chars().count()
        ));
    }
    Ok(token)
}

/// Parses `OXSUM_SESSION_COOKIE_SECURE`: whether the session cookie carries the `Secure`
/// attribute.
///
/// `true`/`1` enable it, `false`/`0` (and absence) disable it for local http development; a
/// deployment behind TLS must set it, or browsers will not send the cookie back over https.
/// Anything else refuses to start rather than guess.
fn session_cookie_secure_of(raw: String) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(format!(
            "OXSUM_SESSION_COOKIE_SECURE must be true or false, got {other:?}"
        )),
    }
}

/// Parses `OXSUM_HOLD_TIMEOUT`: how long a hold may sit unsettled before the background sweeper
/// releases it.
///
/// A number with an `s`, `m` or `h` suffix, or a bare number of seconds. Refused at startup when
/// it does not parse or is below a minute: a timeout that short stops being a crash
/// backstop and starts racing legitimate requests — including the classic `30s`-for-`30m` typo —
/// so the server does not start rather than sweep with a timeout it had to guess.
fn hold_timeout_of(raw: String) -> Result<Duration, String> {
    /// Below a minute the timeout is a hazard rather than a backstop (docs/product.md: it must
    /// exceed the longest possible single request).
    const MIN_HOLD_TIMEOUT: Duration = Duration::from_secs(60);
    let text = raw.trim();
    let (number, factor) = match text.strip_suffix(['s', 'm', 'h']) {
        Some(number) => {
            let factor = match text.chars().last() {
                Some('s') => 1,
                Some('m') => 60,
                Some('h') => 3600,
                _ => unreachable!("the suffix was just stripped"),
            };
            (number, factor)
        }
        None => (text, 1),
    };
    let seconds: u64 = number.parse().map_err(|_| {
        format!("OXSUM_HOLD_TIMEOUT must be a number of seconds with an optional s/m/h suffix, got {raw:?}")
    })?;
    let timeout = seconds
        .checked_mul(factor)
        .map(Duration::from_secs)
        .filter(|timeout| !timeout.is_zero())
        .ok_or_else(|| format!("OXSUM_HOLD_TIMEOUT must be a positive duration, got {raw:?}"))?;
    if timeout < MIN_HOLD_TIMEOUT {
        return Err(format!(
            "OXSUM_HOLD_TIMEOUT must be at least {} seconds, got {raw:?}",
            MIN_HOLD_TIMEOUT.as_secs()
        ));
    }
    Ok(timeout)
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Config, Gateway, Signup};
    use oxsum_core::{PriceBook, SecretKey};

    fn book() -> PriceBook {
        PriceBook::from_json(
            "c",
            r#"{"m":{"inputPricePerMillion":1,"outputPricePerMillion":1,"maxOutputTokens":8}}"#,
        )
        .expect("the test's own price book parses")
    }

    #[test]
    fn parses_signup_modes() {
        assert_eq!(Signup::parse("open"), Ok(Signup::Open));
        assert_eq!(Signup::parse(" Open "), Ok(Signup::Open));
        assert_eq!(Signup::parse("invite"), Ok(Signup::Invite));
        assert!(Signup::parse("yes").is_err());
    }

    #[test]
    fn a_channel_drops_the_trailing_slash_and_keeps_its_book() {
        let gateway = Gateway::new("http://127.0.0.1:9/v1/", "secret", book());
        assert_eq!(gateway.base_url(), "http://127.0.0.1:9/v1");
        assert_eq!(gateway.api_key(), "secret");
        assert!(gateway.book().get("m").is_some());
    }

    #[test]
    fn configuration_carries_its_bootstrap_channel_and_its_secrets() {
        let config = Config::new(
            Signup::Open,
            Some(Gateway::new("http://127.0.0.1:9/v1", "k", book())),
        );
        assert_eq!(config.signup(), Signup::Open);
        assert!(config.bootstrap().is_some());
        assert!(config.secret().is_none());
        assert!(config.admin_token().is_none());
        let config = config
            .with_secret(SecretKey::from_bytes([1; 32]))
            .with_admin_token("operator-token-1234");
        assert!(config.secret().is_some());
        assert_eq!(config.admin_token(), Some("operator-token-1234"));
        assert_eq!(config.clone().signup(), Signup::Open);
        assert!(Config::new(Signup::Invite, None).bootstrap().is_none());
        // A token short enough to guess is refused where it is read, not accepted quietly.
        assert!(config_from_token("short").is_err());
        assert!(config_from_token("long-enough-token").is_ok());
    }

    /// A configuration built the way `from_env` builds the operator token.
    fn config_from_token(token: &str) -> Result<Config, String> {
        super::admin_token_of(token.to_owned())
            .map(|token| Config::new(Signup::Invite, None).with_admin_token(token))
    }

    #[test]
    fn the_hold_timeout_defaults_to_thirty_minutes_and_parses_suffixes() {
        use std::time::Duration;

        assert_eq!(
            Config::new(Signup::Invite, None).hold_timeout(),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            super::hold_timeout_of("90s".to_owned()).expect("seconds parse"),
            Duration::from_secs(90)
        );
        assert_eq!(
            super::hold_timeout_of("30m".to_owned()).expect("minutes parse"),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            super::hold_timeout_of("2h".to_owned()).expect("hours parse"),
            Duration::from_secs(2 * 3600)
        );
        assert_eq!(
            super::hold_timeout_of("3600".to_owned()).expect("a bare number is seconds"),
            Duration::from_secs(3600)
        );
        assert_eq!(
            super::hold_timeout_of(" 45m ".to_owned()).expect("surrounding space is trimmed"),
            Duration::from_secs(45 * 60)
        );
    }

    #[test]
    fn a_hold_timeout_that_is_not_a_sane_duration_is_refused() {
        // Not a number at all.
        assert!(super::hold_timeout_of("soon".to_owned()).is_err());
        assert!(super::hold_timeout_of("30x".to_owned()).is_err());
        assert!(super::hold_timeout_of("".to_owned()).is_err());
        // Zero is not a timeout.
        assert!(super::hold_timeout_of("0".to_owned()).is_err());
        assert!(super::hold_timeout_of("0m".to_owned()).is_err());
        // Below a minute the timeout races legitimate requests rather than backing them up,
        // including the classic `30s`-for-`30m` typo.
        assert!(super::hold_timeout_of("30s".to_owned()).is_err());
        assert!(super::hold_timeout_of("59s".to_owned()).is_err());
        assert!(super::hold_timeout_of("60s".to_owned()).is_ok());
    }

    #[test]
    fn the_session_cookie_secure_flag_defaults_to_off_and_parses_booleans() {
        assert!(!Config::new(Signup::Invite, None).session_cookie_secure());
        assert!(
            Config::new(Signup::Invite, None)
                .with_session_cookie_secure(true)
                .session_cookie_secure()
        );
        assert_eq!(super::session_cookie_secure_of("true".to_owned()), Ok(true));
        assert_eq!(super::session_cookie_secure_of("1".to_owned()), Ok(true));
        assert_eq!(
            super::session_cookie_secure_of("false".to_owned()),
            Ok(false)
        );
        assert_eq!(
            super::session_cookie_secure_of(" True ".to_owned()),
            Ok(true)
        );
        // Anything else refuses to start rather than guess.
        assert!(super::session_cookie_secure_of("yes".to_owned()).is_err());
        assert!(super::session_cookie_secure_of("".to_owned()).is_err());
    }
}
