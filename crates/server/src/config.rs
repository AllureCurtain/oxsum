//! Deployment configuration.
//!
//! Not secrets and not data: these are the few switches a deployer sets, and they shape how
//! a request is handled. Everything else the server needs is an environment variable read
//! once at startup, in main.rs.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use oxsum_core::{DEFAULT_HOLD_TIMEOUT, PriceBook, SecretKey};

use crate::airwallex::Airwallex;
use crate::antibot::Turnstile;
use crate::hyperswitch::Hyperswitch;
use crate::mail::Mailer;
use crate::oauth::GitHub;
use crate::oidc::OidcProvider;
use crate::stripe::Stripe;

/// Whether this deployment lets people register themselves.
///
/// product.md: `invite` is the default, because a public instance should not let whoever
/// arrives first create an account; `open` is what a demo or a private instance wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signup {
    /// Registration is invitation-only — the default, so a public instance does
    /// not let whoever arrives first create an account.
    Invite,
    /// Anyone may register — the demo or private-instance shape.
    Open,
}

impl Signup {
    /// Parses `OXSUM_SIGNUP`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        raw.parse()
    }
}

impl FromStr for Signup {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "invite" => Ok(Self::Invite),
            "open" => Ok(Self::Open),
            other => Err(format!(
                "OXSUM_SIGNUP must be invite or open, got {other:?}"
            )),
        }
    }
}

/// Which backend `requestsPerMinute` counts inside (issue #498). `memory` — the
/// default — is the in-process `SlidingWindow` a single binary needs; `postgres`
/// counts the window in `oxsum.rate_windows`, so a deployment running more than
/// one instance behind a load balancer shares the count instead of
/// double-admitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitBackend {
    /// The in-process `SlidingWindow` — the single-binary default.
    #[default]
    Memory,
    /// `PostgresRateLimiter` over `oxsum.rate_windows` — the shared backend a
    /// multi-instance deployment picks, on the database it already runs.
    Postgres,
}

impl RateLimitBackend {
    /// Parses `OXSUM_RATE_LIMIT_BACKEND`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        raw.parse()
    }
}

impl FromStr for RateLimitBackend {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "memory" => Ok(Self::Memory),
            "postgres" => Ok(Self::Postgres),
            other => Err(format!(
                "OXSUM_RATE_LIMIT_BACKEND must be memory or postgres, got {other:?}"
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
#[derive(Clone)]
pub struct Gateway {
    base_url: String,
    api_key: String,
    book: PriceBook,
    destination_policy: oxsum_core::ProviderDestinationPolicy,
}
impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gateway([provider credentials redacted])")
    }
}

impl Gateway {
    /// A channel described in code, which is what the tests build.
    #[must_use]
    pub fn new(base_url: &str, api_key: &str, book: PriceBook) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            book,
            destination_policy: Default::default(),
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
        let policy = var("OXSUM_UPSTREAM_DESTINATION_POLICY")
            .map(|raw| {
                serde_json::from_str::<oxsum_core::ProviderDestinationPolicy>(&raw).map_err(|_| {
                    "OXSUM_UPSTREAM_DESTINATION_POLICY must be a provider network policy".to_owned()
                })
            })
            .transpose()?
            .unwrap_or_default();
        policy.endpoint(&base_url).map_err(|_| {
            "OXSUM_UPSTREAM_BASE_URL is not permitted by OXSUM_UPSTREAM_DESTINATION_POLICY"
                .to_owned()
        })?;
        let mut gateway = Self::new(&base_url, &api_key, book);
        gateway.destination_policy = policy;
        Ok(Some(gateway))
    }

    /// The provider network policy the channel's requests must satisfy, from
    /// `OXSUM_UPSTREAM_DESTINATION_POLICY`. Every request the gateway builds for
    /// this channel is checked against it before it is sent.
    pub fn destination_policy(&self) -> &oxsum_core::ProviderDestinationPolicy {
        &self.destination_policy
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

/// One pinned witness key — what a `cosignature/v1` signature line under
/// `witness_url` must verify against (issue #259).
///
/// The pinset is the deployment's own claim about which keys are real: serving
/// it on the checkpoint endpoint lets a reader run the check without trusting
/// the server's verdict, while a verifier that distrusts the deployment can
/// substitute its own pinset — the same choice `GET /api/v1/log/key` already
/// offers for the operator's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessKey {
    /// The witness endpoint this pin applies to, as `OXSUM_WITNESS_URLS` names it.
    pub witness_url: String,
    /// The name the witness's cosignature lines carry (`— <name> <blob>`).
    pub key_name: String,
    /// The Ed25519 verifying key, hex.
    pub public_key: String,
    /// The cosignature selector — `SHA-256(name || '\n' || 0x04 || public)`'s
    /// first four bytes, hex. Validated against `key_name` + `public_key` at
    /// boot, so a misconfigured pin refuses to start rather than answering
    /// `BadSignature` forever.
    pub key_hash: String,
}

/// One trust root `OXSUM_WITNESS_TRUST` pins: a foreign log's origin and the
/// starting head this witness vouches forward from (issue #260).
///
/// The root is the out-of-band act — the head the operator independently
/// confirms the foreign log stands at before the first `/add-checkpoint`
/// arrives. A submission for an origin with no root is refused: there is no
/// trust-on-first-use, because a witness that accepts whatever arrives first
/// has already been made to forget a fork.
///
/// Pinning `size: 0` is the deliberate onboarding case: a witness holding a
/// size-0 root takes the first non-empty head it is offered on trust — every
/// log extends the empty tree, so there is nothing to prove — and vouches
/// forward from there. That is TOFU by explicit operator choice, which is how
/// a brand-new log is enrolled; pinning a real head is the stronger act for a
/// log whose history is already out there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessTrust {
    /// The log's origin, as the note's first line carries it
    /// (`oxsum/ledgers/<tenant>` for an oxsum log, anything for a foreign one).
    pub origin: String,
    /// The starting tree size.
    pub size: u64,
    /// The starting head root, base64 — exactly as the note body carries it.
    pub root: String,
}

/// The deployment's own tlog-witness, opted into by `OXSUM_WITNESS_SIGNING_KEY`
/// (issue #260): the key it cosigns under, the name its signature lines carry,
/// and the trust roots `OXSUM_WITNESS_TRUST` seeds.
#[derive(Debug, Clone)]
pub struct WitnessServe {
    /// 32 bytes: the Ed25519 seed the witness's cosignatures are drawn from.
    pub seed: [u8; 32],
    /// The name the cosignature lines carry (`— <name> <blob>`), default
    /// `oxsum/witness`.
    pub key_name: String,
    /// The trust roots, one per foreign log this witness vouches for.
    pub trust: Vec<WitnessTrust>,
}

/// Public seller identity defaults, superseded by the owner-managed profile.
pub type Branding = oxsum_core::PublicBusinessProfile;

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
    signup_bonus_minor: i64,
    mailer: Option<Mailer>,
    github: Option<GitHub>,
    /// The configured OIDC providers from `OXSUM_OIDC_PROVIDERS` (issue #504):
    /// one entry per enterprise IdP the deployment federates logins to. Empty —
    /// the default — mounts no OIDC login and the `{provider}` route 404s like
    /// every unknown provider.
    oidc_providers: Vec<OidcProvider>,
    turnstile: Option<Turnstile>,
    stripe: Option<Stripe>,
    /// The Airwallex deposit rail (issue #285): the credential pair plus
    /// webhook secret when the `OXSUM_AIRWALLEX_*` set is complete, `None` —
    /// the default — when the deployment takes no Airwallex payments.
    airwallex: Option<Airwallex>,
    /// The Hyperswitch deposit rail (issue #319): the API key, the business
    /// profile's webhook hash key and the browser publishable key when the
    /// `OXSUM_HYPERSWITCH_*` set is complete, `None` — the default — when the
    /// deployment takes no switch payments.
    hyperswitch: Option<Hyperswitch>,
    merchant_bootstraps: Vec<oxsum_core::MerchantBootstrap>,
    /// The deployment's public origin, `OXSUM_PUBLIC_URL` — required by the
    /// mailer and the OAuth client, optional on its own: the device grant
    /// builds `verificationUri` from it when set (issue #156).
    public_url: Option<String>,
    /// Which `RateLimiter` backend `requestsPerMinute` counts inside,
    /// `OXSUM_RATE_LIMIT_BACKEND` (issue #498): `memory` — the default — is the
    /// in-process `SlidingWindow` a single binary needs; `postgres` counts the
    /// window in `oxsum.rate_windows` so a multi-instance deployment shares it
    /// rather than double-admitting.
    rate_limit_backend: RateLimitBackend,
    /// The retention windows the `retention` job applies, `OXSUM_RETENTION_*`.
    retention: oxsum_core::Retention,
    /// Whether `OXSUM_UPSTREAM_PRICE_SYNC` gates the `SYNC_UPSTREAM_PRICES`
    /// periodic job (issue #222). Optional and off by default, like SMTP.
    upstream_price_sync: bool,
    /// Where OTLP spans go, `OXSUM_OTLP_ENDPOINT` (issue #224). `None` — the
    /// default — keeps tracing strictly local.
    otlp_endpoint: Option<String>,
    /// The external witnesses the `witness-checkpoints` pass publishes heads
    /// to, `OXSUM_WITNESS_URLS` comma-separated (issue #231). Empty — the
    /// default — runs no anchoring: `/api/v1/log/checkpoint` serves an empty
    /// snapshot.
    witness_urls: Vec<String>,
    /// The pinned keyset for those witnesses, `OXSUM_WITNESS_KEYS` as a JSON
    /// array (issue #259): which `cosignature/v1` key a signature line under
    /// each URL must verify against. Served on `/api/v1/log/checkpoint` as
    /// `witnessKeys` so a reader can check the cosignatures itself — under the
    /// same trust choice the operator's own published key already asks for.
    /// Empty means the endpoint serves cosigned notes but no pins.
    witness_keys: Vec<WitnessKey>,
    /// The deployment's own tlog-witness, `OXSUM_WITNESS_SIGNING_KEY` plus
    /// `OXSUM_WITNESS_KEY_NAME`/`OXSUM_WITNESS_TRUST` (issue #260). `None` —
    /// the default — runs no witness and `/add-checkpoint` is 404.
    witness_serve: Option<WitnessServe>,
    /// The deployment-wide display currency default (issue #257):
    /// `OXSUM_DISPLAY_CURRENCY` plus `OXSUM_DISPLAY_RATE_PER_CREDIT` — what an
    /// organization with no override of its own renders. `None` means the
    /// deployment displays credits only.
    display_currency: Option<oxsum_core::Display>,
    /// The x402 deposit rail's verifier (issues #264, #282): the facilitator-
    /// delegated implementation when the `OXSUM_X402_FACILITATOR_*` set is
    /// complete, else the Ed25519 stand-in `OXSUM_X402_VERIFYING_KEY` pins.
    /// `None` — the default — runs no rail and `POST /api/v1/topups/x402`
    /// answers 503.
    x402: Option<Arc<dyn oxsum_core::RailVerifier>>,
    /// The Solana deposit rail's verifier (issue #1117): a `SolanaRpcClient`
    /// bound to the deployment's `OXSUM_SOLANA_*` terms, wrapped in the
    /// `SolanaRailVerifier` that answers the `RailVerifier` seam. `None` — the
    /// default — runs no rail and the solana endpoints answer 503.
    solana: Option<Arc<oxsum_core::SolanaRailVerifier>>,
    /// The provider the `upstream-bill-sync` job asks for each channel's daily
    /// billed cost (issue #283): the OpenAI Costs impl when
    /// `OXSUM_UPSTREAM_BILL_SYNC` is enabled (and `OXSUM_SECRET_KEY` present to
    /// open `bill_key_sealed`), a test's stub otherwise. `None` — the default —
    /// schedules no `upstream-bill-sync` pass.
    bill_provider: Option<Arc<dyn oxsum_core::UpstreamBillProvider>>,
    /// Environment defaults for public seller identity before an owner save.
    /// The accepted setup name already overrides the environment name; a
    /// stored profile overrides every default, including explicit nulls.
    branding: Branding,
    /// Whether evidence bundles export `upstreamCostMinor` on usage records,
    /// `OXSUM_EVIDENCE_UPSTREAM_COST` (issue #616). `false` — the default —
    /// redacts the platform's own supplier cost, which is internal: the
    /// bundle's `costBasis` still says how the charge was priced, and the
    /// scope block's `upstreamCostFigures` exclusion says the figure is not
    /// attested either way.
    evidence_upstream_cost: bool,
}

impl Config {
    /// The minimal configuration: a signup policy and an optional bootstrap
    /// channel, with every optional integration, rail and feature switched off
    /// and the built-in defaults in place.
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
            signup_bonus_minor: 0,
            mailer: None,
            github: None,
            oidc_providers: Vec::new(),
            turnstile: None,
            stripe: None,
            airwallex: None,
            hyperswitch: None,
            merchant_bootstraps: Vec::new(),
            public_url: None,
            rate_limit_backend: RateLimitBackend::default(),
            retention: oxsum_core::Retention::default(),
            upstream_price_sync: false,
            otlp_endpoint: None,
            witness_urls: Vec::new(),
            witness_keys: Vec::new(),
            witness_serve: None,
            display_currency: None,
            x402: None,
            solana: None,
            bill_provider: None,
            branding: Branding::default(),
            evidence_upstream_cost: false,
        }
    }

    /// Sets the anti-bot check. Tests use this; the server reads
    /// `OXSUM_TURNSTILE_SITE_KEY` and `OXSUM_TURNSTILE_SECRET_KEY` together —
    /// a lone key is a startup error, because a half-configured check only
    /// fails inside a registration.
    #[must_use]
    pub fn with_turnstile(mut self, turnstile: Turnstile) -> Self {
        self.turnstile = Some(turnstile);
        self
    }

    /// Sets the GitHub OAuth client. Tests use this; the server reads
    /// `OXSUM_GITHUB_CLIENT_ID`, `OXSUM_GITHUB_CLIENT_SECRET` and
    /// `OXSUM_PUBLIC_URL` together — a subset is a startup error, because a
    /// half-configured provider only fails inside a callback.
    #[must_use]
    pub fn with_github(mut self, github: GitHub) -> Self {
        self.github = Some(github);
        self
    }

    /// Sets the mailer. Tests use this; the server reads `OXSUM_SMTP_URL`,
    /// `OXSUM_MAIL_FROM` and `OXSUM_PUBLIC_URL` together — any subset is a
    /// startup error, because a half-configured mailer only fails at send time.
    #[must_use]
    pub fn with_mailer(mut self, mailer: Mailer) -> Self {
        self.mailer = Some(mailer);
        self
    }

    /// Sets the key that seals upstream credentials. Required as soon as a channel exists.
    #[must_use]
    pub fn with_secret(mut self, secret: SecretKey) -> Self {
        self.secret = Some(secret);
        self
    }

    /// Sets the rate-limit backend. Tests use this; the server reads
    /// `OXSUM_RATE_LIMIT_BACKEND` (issue #498).
    #[must_use]
    pub fn with_rate_limit_backend(mut self, backend: RateLimitBackend) -> Self {
        self.rate_limit_backend = backend;
        self
    }

    /// Sets the operator token that opens `/api/v1/admin`.
    #[must_use]
    pub fn with_admin_token(mut self, token: &str) -> Self {
        self.admin_token = Some(token.to_owned());
        self
    }

    /// Sets the seed the operator signs tree heads with. Tests use this; the server reads
    /// `OXSUM_HEAD_SIGNING_KEY`.
    #[must_use]
    pub fn with_head_signing_seed(mut self, seed: [u8; 32]) -> Self {
        self.head_signing_seed = Some(seed);
        self
    }

    /// Sets the signup bonus, in minor units. Tests use this; the server reads
    /// `OXSUM_SIGNUP_BONUS_MINOR`.
    #[must_use]
    pub fn with_signup_bonus(mut self, minor: i64) -> Self {
        self.signup_bonus_minor = minor;
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
        let signup_bonus_minor = var("OXSUM_SIGNUP_BONUS_MINOR")
            .map(signup_bonus_of)
            .transpose()?
            .unwrap_or(0);
        let mailer = mailer_of()?;
        // Optional on its own: the mailer and the OAuth clients each require it,
        // and the device grant only prefers it for a stable `verificationUri`.
        let public_url = var("OXSUM_PUBLIC_URL");
        let github = github_of()?;
        let oidc_providers = oidc_providers_of(public_url.as_deref())?;
        let turnstile = turnstile_of()?;
        let stripe = stripe_of()?;
        let airwallex = airwallex_of()?;
        let hyperswitch = hyperswitch_of()?;
        let merchant_bootstraps =
            merchant_bootstraps_of(stripe.as_ref(), airwallex.as_ref(), hyperswitch.as_ref())?;
        let rate_limit_backend = rate_limit_backend_of()?;
        let retention = oxsum_core::Retention {
            provider_raw_days: retention_days("OXSUM_RETENTION_PROVIDER_RAW_DAYS")?.unwrap_or(7),
            deliveries_days: retention_days("OXSUM_RETENTION_DELIVERIES_DAYS")?.unwrap_or(30),
            jobs_days: retention_days("OXSUM_RETENTION_JOBS_DAYS")?.unwrap_or(90),
            request_log_days: retention_days("OXSUM_RETENTION_REQUEST_LOG_DAYS")?.unwrap_or(30),
        };
        let upstream_price_sync = var("OXSUM_UPSTREAM_PRICE_SYNC")
            .map(upstream_price_sync_of)
            .transpose()?
            .unwrap_or(false);
        let witness_urls = witness_urls_of()?;
        let witness_keys = witness_keys_of(&witness_urls)?;
        let witness_serve = witness_serve_of()?;
        // Trailing slashes come off: the exporter appends `/v1/traces`, and a
        // doubled slash is only a cosmetic wrong.
        let otlp_endpoint = var("OXSUM_OTLP_ENDPOINT").map(|v| v.trim_end_matches('/').to_owned());
        let display_currency = display_currency_of()?;
        // Parsed before `secret` moves into the struct — it only reads the
        // env flag and the sealing key's presence, not the struct itself.
        let bill_provider = bill_provider_of(secret.as_ref())?;
        let x402 = x402_of()?;
        let solana = solana_of()?;
        // Operator-intent cross-checks (issue #619): `OXSUM_RAILS` declares the
        // deposit rails the operator means to run and `OXSUM_CUSTOMER_SELF_REGISTER`
        // the registration shape — both validated against what the rest of the
        // environment actually configured, so a contradictory .env fails the
        // boot here with the named variable instead of inside a request.
        rails_intent_of(
            var("OXSUM_RAILS").as_deref(),
            stripe.as_ref(),
            airwallex.as_ref(),
            hyperswitch.as_ref(),
            x402.as_ref(),
            solana.as_ref(),
        )?;
        self_register_intent_of(
            var("OXSUM_CUSTOMER_SELF_REGISTER").as_deref(),
            mailer.as_ref(),
        )?;
        Ok(Self {
            signup,
            gateway: Gateway::from_env()?,
            secret,
            admin_token,
            head_signing_seed,
            hold_timeout,
            session_cookie_secure,
            signup_bonus_minor,
            mailer,
            github,
            oidc_providers,
            turnstile,
            stripe,
            airwallex,
            hyperswitch,
            merchant_bootstraps,
            public_url,
            rate_limit_backend,
            retention,
            upstream_price_sync,
            otlp_endpoint,
            witness_urls,
            witness_keys,
            witness_serve,
            display_currency,
            x402,
            solana,
            bill_provider,
            branding: branding_of()?,
            evidence_upstream_cost: evidence_upstream_cost_of()?,
        })
    }

    #[must_use]
    /// Legacy parsed value for configuration compatibility. Business policy controls signup.
    pub fn signup(&self) -> Signup {
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
    pub fn secret(&self) -> Option<&SecretKey> {
        self.secret.as_ref()
    }

    /// Which `RateLimiter` backend `requestsPerMinute` counts inside,
    /// `OXSUM_RATE_LIMIT_BACKEND` — `memory` (the default) or `postgres`
    /// (issue #498).
    #[must_use]
    pub(crate) fn rate_limit_backend(&self) -> RateLimitBackend {
        self.rate_limit_backend
    }

    /// The mailer, or `None` when the deployment sends no email: every email
    /// surface then behaves as documented — registration sends nothing, forgot
    /// is indistinguishable, and `verify/request` alone reports the absence.
    #[must_use]
    pub fn mailer(&self) -> Option<&Mailer> {
        self.mailer.as_ref()
    }

    /// The GitHub OAuth client, or `None` when the provider pair is not
    /// configured — in which case the start endpoint answers 404 and the
    /// callback redirects home, and the login page draws no GitHub button.
    #[must_use]
    pub fn github(&self) -> Option<&GitHub> {
        self.github.as_ref()
    }

    /// The configured OIDC providers, `OXSUM_OIDC_PROVIDERS` (issue #504): one
    /// per IdP the deployment federates logins to. Empty — the default — mounts
    /// no OIDC login; the login page draws no buttons and the `{provider}`
    /// routes 404 like every unknown provider.
    #[must_use]
    pub fn oidc_providers(&self) -> &[OidcProvider] {
        &self.oidc_providers
    }

    /// One configured provider by its `oidc:{name}` route name, or `None`.
    #[must_use]
    pub fn oidc_provider(&self, route_name: &str) -> Option<&OidcProvider> {
        self.oidc_providers
            .iter()
            .find(|p| crate::oidc::provider_name(p.name()) == route_name)
    }

    /// For tests: a provider list in place of the environment's.
    #[must_use]
    pub fn with_oidc_providers(mut self, providers: Vec<OidcProvider>) -> Self {
        self.oidc_providers = providers;
        self
    }

    /// The anti-bot check, or `None` when the pair is not configured — the
    /// two registration surfaces then run unchecked, exactly as before the
    /// check existed.
    #[must_use]
    pub fn turnstile(&self) -> Option<&Turnstile> {
        self.turnstile.as_ref()
    }

    /// Explicit default-business merchant bootstrap, never populated from a
    /// webhook body or a client payment request.
    pub fn merchant_bootstraps(&self) -> &[oxsum_core::MerchantBootstrap] {
        &self.merchant_bootstraps
    }
    /// Adds one explicit merchant bootstrap for a payment rail: its merchant
    /// account id, mode and destination policy. An operator-supplied bootstrap,
    /// never populated from a webhook body or a client payment request.
    #[must_use]
    pub fn with_merchant_bootstrap(mut self, input: oxsum_core::MerchantBootstrap) -> Self {
        self.merchant_bootstraps.push(input);
        self
    }

    /// The Stripe rail, or `None` when `OXSUM_STRIPE_SECRET_KEY` and
    /// `OXSUM_STRIPE_WEBHOOK_SECRET` are not both set — in which case
    /// `POST /api/v1/topups/stripe` answers 503 and `POST
    /// /api/v1/webhooks/stripe` answers 404.
    #[must_use]
    pub fn stripe(&self) -> Option<&Stripe> {
        self.stripe.as_ref()
    }

    /// Sets the Stripe rail. Tests use this; the server reads
    /// `OXSUM_STRIPE_SECRET_KEY`, `OXSUM_STRIPE_WEBHOOK_SECRET` and
    /// `OXSUM_PUBLIC_URL` together — a subset is a startup error.
    #[must_use]
    pub fn with_stripe(mut self, stripe: Stripe) -> Self {
        self.stripe = Some(stripe);
        self
    }

    /// The Airwallex rail, or `None` when `OXSUM_AIRWALLEX_CLIENT_ID`,
    /// `OXSUM_AIRWALLEX_API_KEY`, `OXSUM_AIRWALLEX_WEBHOOK_SECRET` and
    /// `OXSUM_PUBLIC_URL` are not all set — in which case
    /// `POST /api/v1/topups/airwallex` answers 503 and
    /// `POST /api/v1/webhooks/airwallex` answers 404.
    #[must_use]
    pub fn airwallex(&self) -> Option<&Airwallex> {
        self.airwallex.as_ref()
    }

    /// Sets the Airwallex rail. Tests use this; the server reads the
    /// `OXSUM_AIRWALLEX_*` set plus `OXSUM_PUBLIC_URL` together — a subset is
    /// a startup error.
    #[must_use]
    pub fn with_airwallex(mut self, airwallex: Airwallex) -> Self {
        self.airwallex = Some(airwallex);
        self
    }

    /// The Hyperswitch rail, or `None` when `OXSUM_HYPERSWITCH_API_KEY`,
    /// `OXSUM_HYPERSWITCH_WEBHOOK_SECRET`, `OXSUM_HYPERSWITCH_PUBLISHABLE_KEY`
    /// and `OXSUM_PUBLIC_URL` are not all set — in which case
    /// `POST /api/v1/topups/hyperswitch` answers 503 and
    /// `POST /api/v1/webhooks/hyperswitch` answers 404.
    #[must_use]
    pub fn hyperswitch(&self) -> Option<&Hyperswitch> {
        self.hyperswitch.as_ref()
    }

    /// Sets the Hyperswitch rail. Tests use this; the server reads the
    /// `OXSUM_HYPERSWITCH_*` set plus `OXSUM_PUBLIC_URL` together — a subset is
    /// a startup error.
    #[must_use]
    pub fn with_hyperswitch(mut self, hyperswitch: Hyperswitch) -> Self {
        self.hyperswitch = Some(hyperswitch);
        self
    }

    /// The x402 deposit rail's verifier, or `None` when neither the
    /// facilitator set nor `OXSUM_X402_VERIFYING_KEY` is configured — in which
    /// case `POST /api/v1/topups/x402` answers 503. Returned behind the
    /// [`RailVerifier`][oxsum_core::RailVerifier] trait: the rail's seam, so a
    /// real crypto verifier drops in without touching the handler.
    #[must_use]
    pub fn x402(&self) -> Option<&dyn oxsum_core::RailVerifier> {
        self.x402
            .as_ref()
            .map(|v| v.as_ref() as &dyn oxsum_core::RailVerifier)
    }

    /// Sets the x402 rail's verifier. Tests use this; the server reads the
    /// `OXSUM_X402_*` environment.
    #[must_use]
    pub fn with_x402(mut self, verifier: impl oxsum_core::RailVerifier + 'static) -> Self {
        self.x402 = Some(Arc::new(verifier));
        self
    }

    /// The Solana deposit rail's verifier (issue #1117), or `None` when the
    /// `OXSUM_SOLANA_*` set is not complete — in which case the solana top-up
    /// endpoints answer 503. The returned verifier is the `RailVerifier` seam
    /// *and* the deployment's terms (`SolanaRailVerifier::terms`) the intent
    /// route writes into `meta`.
    #[must_use]
    pub fn solana(&self) -> Option<&oxsum_core::SolanaRailVerifier> {
        self.solana.as_ref().map(|v| v.as_ref())
    }

    /// Sets the Solana rail's verifier. Tests use this; the server reads the
    /// `OXSUM_SOLANA_*` environment.
    #[must_use]
    pub fn with_solana(mut self, verifier: oxsum_core::SolanaRailVerifier) -> Self {
        self.solana = Some(Arc::new(verifier));
        self
    }

    /// `OXSUM_PUBLIC_URL` when set — the origin outward-facing links are built
    /// from. `None` is legal: the device grant's `verificationUri` falls back
    /// to the relative `/device`.
    #[must_use]
    pub fn public_url(&self) -> Option<&str> {
        self.public_url.as_deref()
    }

    /// The retention windows the `retention` job applies.
    #[must_use]
    pub fn retention(&self) -> oxsum_core::Retention {
        self.retention
    }

    /// Sets the retention windows. Tests use this; the server reads
    /// `OXSUM_RETENTION_PROVIDER_RAW_DAYS`, `OXSUM_RETENTION_DELIVERIES_DAYS`,
    /// `OXSUM_RETENTION_JOBS_DAYS` and `OXSUM_RETENTION_REQUEST_LOG_DAYS`.
    #[must_use]
    pub fn with_retention(mut self, retention: oxsum_core::Retention) -> Self {
        self.retention = retention;
        self
    }

    /// Whether the `SYNC_UPSTREAM_PRICES` periodic job runs (issue #222).
    /// `false` is the default: a deployment opts in with
    /// `OXSUM_UPSTREAM_PRICE_SYNC`, and even then the job only probes when a
    /// channel credential can be unsealed — which needs `OXSUM_SECRET_KEY`.
    #[must_use]
    pub fn upstream_price_sync(&self) -> bool {
        self.upstream_price_sync
    }

    /// The provider the `upstream-bill-sync` job reconciles through (issue
    /// #283), or `None` when `OXSUM_UPSTREAM_BILL_SYNC` is off — the job is
    /// only scheduled when `Some`.
    #[must_use]
    pub fn bill_provider(&self) -> Option<&Arc<dyn oxsum_core::UpstreamBillProvider>> {
        self.bill_provider.as_ref()
    }

    /// The witness endpoints `OXSUM_WITNESS_URLS` configured (issue #231):
    /// the pass publishes each organization's signed head here; empty means
    /// no anchoring runs.
    #[must_use]
    pub fn witness_urls(&self) -> &[String] {
        &self.witness_urls
    }

    /// The pinned witness keyset `OXSUM_WITNESS_KEYS` configured (issue #259):
    /// served on `/api/v1/log/checkpoint` as `witnessKeys` so a reader can
    /// verify each cosignature line itself. Empty means the endpoint serves
    /// the cosigned notes with no pins — the notes are still verifiable
    /// against a pinset the reader configured itself.
    #[must_use]
    pub fn witness_keys(&self) -> &[WitnessKey] {
        &self.witness_keys
    }

    /// The display unit an organization with no override of its own renders
    /// (issue #257): the org's stored `display_currency` pair when set, else
    /// the deployment's `OXSUM_DISPLAY_*` default, else `None` — credits only.
    /// Returns the owned [`std::fmt::Display`]; the conversion is a cheap struct, so a
    /// per-request rebuild beats a borrowed cache an org override would
    /// invalidate anyway.
    #[must_use]
    pub fn display_for(
        &self,
        organization: &oxsum_core::Organization,
    ) -> Option<oxsum_core::Display> {
        self.display_for_pair(organization.display_currency.as_ref())
    }

    /// The same resolution off the raw stored pair — for callers holding an
    /// `AdminOrganization` rather than the org-facing `Organization` (the admin
    /// statement export renders the organization's own unit, not the
    /// deployment's default).
    #[must_use]
    pub fn display_for_pair(
        &self,
        stored: Option<&oxsum_core::DisplayCurrency>,
    ) -> Option<oxsum_core::Display> {
        stored
            .and_then(|currency| {
                oxsum_core::display_currency(
                    Some(&currency.code),
                    Some(&currency.rate_per_credit),
                    currency.decimals,
                )
                .ok()
                .flatten()
            })
            .or_else(|| self.display_currency.clone())
    }

    /// Sets the witness URL list. Tests use this; the server reads
    /// `OXSUM_WITNESS_URLS`.
    #[must_use]
    pub fn with_witness_urls(mut self, urls: Vec<String>) -> Self {
        self.witness_urls = urls;
        self
    }

    /// Sets the pinned witness keyset. Tests use this; the server reads
    /// `OXSUM_WITNESS_KEYS`.
    #[must_use]
    pub fn with_witness_keys(mut self, keys: Vec<WitnessKey>) -> Self {
        self.witness_keys = keys;
        self
    }

    /// The deployment's own tlog-witness, or `None` when
    /// `OXSUM_WITNESS_SIGNING_KEY` is unset — in which case `/add-checkpoint`
    /// answers 404 like any path the server does not serve (issue #260).
    #[must_use]
    pub fn witness_serve(&self) -> Option<&WitnessServe> {
        self.witness_serve.as_ref()
    }

    /// Sets the witness-serve config. Tests use this; the server reads
    /// `OXSUM_WITNESS_SIGNING_KEY`/`OXSUM_WITNESS_KEY_NAME`/`OXSUM_WITNESS_TRUST`.
    #[must_use]
    pub fn with_witness_serve(mut self, serve: WitnessServe) -> Self {
        self.witness_serve = Some(serve);
        self
    }

    /// Sets the upstream price-sync gate. Tests use this; the server reads
    /// `OXSUM_UPSTREAM_PRICE_SYNC`.
    #[must_use]
    pub fn with_upstream_price_sync(mut self, enabled: bool) -> Self {
        self.upstream_price_sync = enabled;
        self
    }

    /// Mounts a bill provider — a test's scripted stub. Production reads it
    /// through `OXSUM_UPSTREAM_BILL_SYNC` + `OXSUM_SECRET_KEY`; a test hands
    /// the impl directly and needs no env.
    #[must_use]
    pub fn with_bill_provider(
        mut self,
        provider: impl oxsum_core::UpstreamBillProvider + 'static,
    ) -> Self {
        self.bill_provider = Some(Arc::new(provider));
        self
    }

    /// Whether evidence bundles export `upstreamCostMinor` on usage records
    /// (issue #616). `false` — the default — redacts the platform's supplier
    /// cost: it is internal, and the bundle's `costBasis` plus the scope
    /// block's `upstreamCostFigures` exclusion say what is and is not attested
    /// without it. A deployment opts in with `OXSUM_EVIDENCE_UPSTREAM_COST`.
    #[must_use]
    pub fn evidence_upstream_cost(&self) -> bool {
        self.evidence_upstream_cost
    }

    /// Sets the upstream-cost export flag. Tests use this; the server reads
    /// `OXSUM_EVIDENCE_UPSTREAM_COST`.
    #[must_use]
    pub fn with_evidence_upstream_cost(mut self, enabled: bool) -> Self {
        self.evidence_upstream_cost = enabled;
        self
    }

    /// `OXSUM_OTLP_ENDPOINT` when set — the collector base URL trace export
    /// points at (issue #224). `None` keeps the subscriber fmt-only.
    #[must_use]
    pub fn otlp_endpoint(&self) -> Option<&str> {
        self.otlp_endpoint.as_deref()
    }

    /// Unsaved seller identity defaults. Public reads resolve the stored profile
    /// first; environment values never overwrite an explicitly saved profile.
    #[must_use]
    pub fn branding(&self) -> &Branding {
        &self.branding
    }

    /// Sets the seller branding. Tests use this; the server reads
    /// `OXSUM_BUSINESS_NAME`, `OXSUM_BRAND_LOGO_URL` and
    /// `OXSUM_BRAND_ACCENT_COLOR`.
    #[must_use]
    pub fn with_branding(mut self, branding: Branding) -> Self {
        self.branding = branding;
        self
    }

    /// Sets the public origin. Tests use this; the server reads
    /// `OXSUM_PUBLIC_URL`.
    #[must_use]
    pub fn with_public_url(mut self, url: &str) -> Self {
        self.public_url = Some(url.to_owned());
        self
    }

    /// The operator token, or `None` when the admin surface is closed.
    #[must_use]
    pub(crate) fn admin_token(&self) -> Option<&str> {
        self.admin_token.as_deref()
    }

    /// The seed the operator signs tree heads with, or `None` when the deployment did not
    /// configure one — in which case the `/api/v1/log` endpoints answer 503.
    #[must_use]
    pub fn head_signing_seed(&self) -> Option<[u8; 32]> {
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

    /// The signup bonus in minor units, granted to a new organization's wallet at
    /// registration — 0 (the default) credits nothing and leaves the ledger lazy.
    #[must_use]
    pub(crate) fn signup_bonus_minor(&self) -> i64 {
        self.signup_bonus_minor
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

/// Parses `OXSUM_WITNESS_URLS`: the comma-separated C2SP `tlog-witness`
/// endpoints the `witness-checkpoints` pass publishes each organization's
/// signed head to (issue #231). Absent or empty means no anchoring — the pass
/// never schedules, and `GET /api/v1/log/checkpoint` answers an empty
/// snapshot. Each entry must parse as a URL; a witness that wants plain HTTP
/// names it deliberately — an unreachable scheme fails loudly at startup
/// rather than silently never anchoring.
fn witness_urls_of() -> Result<Vec<String>, String> {
    let Some(raw) = var("OXSUM_WITNESS_URLS") else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            reqwest::Url::parse(entry)
                .map(|_| entry.trim_end_matches('/').to_owned())
                .map_err(|error| format!("OXSUM_WITNESS_URLS entry {entry:?} is no URL: {error}"))
        })
        .collect()
}

/// Parses `OXSUM_WITNESS_KEYS`: the deployment's pinned witness keyset, a JSON
/// array of `{ "url", "keyName", "publicKey", "keyHash" }` (issue #259).
/// Absent or empty means no pins — the checkpoint endpoint serves the cosigned
/// notes and an empty `witnessKeys`, which a verifier holding its own pinset
/// can still check.
///
/// Validation is deliberately at boot, not per request: a pin whose `url` is
/// not one of `OXSUM_WITNESS_URLS`, whose `keyName` is not a valid note key
/// name, whose `publicKey` is not a 32-byte Ed25519 point, or whose `keyHash`
/// does not equal the recomputed cosignature selector is a misconfiguration —
/// it refuses to start rather than serve a pinset that can only ever answer
/// `BadSignature`.
fn witness_keys_of(witness_urls: &[String]) -> Result<Vec<WitnessKey>, String> {
    use oxsum_core::{VerifyingKey, WitnessKeyName};

    let Some(raw) = var("OXSUM_WITNESS_KEYS") else {
        return Ok(Vec::new());
    };
    let entries: Vec<serde_json::Value> = serde_json::from_str(&raw)
        .map_err(|error| format!("OXSUM_WITNESS_KEYS is not a JSON array: {error}"))?;
    entries
        .iter()
        .map(|entry| {
            let field = |name: &str| {
                entry
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("OXSUM_WITNESS_KEYS entry is missing {name:?}"))
            };
            let url = field("url")?;
            let url = reqwest::Url::parse(&url)
                .map(|_| url.trim_end_matches('/').to_owned())
                .map_err(|error| format!("OXSUM_WITNESS_KEYS url {url:?} is no URL: {error}"))?;
            if !witness_urls.iter().any(|u| u == &url) {
                return Err(format!(
                    "OXSUM_WITNESS_KEYS pins {url:?}, which OXSUM_WITNESS_URLS does not publish to"
                ));
            }
            let key_name = field("keyName")?;
            let name = WitnessKeyName::new(key_name.clone())
                .map_err(|error| format!("OXSUM_WITNESS_KEYS keyName {key_name:?}: {error}"))?;
            let public_key = field("publicKey")?;
            let public = hex_decode::<32>(&public_key).map_err(|_| {
                format!("OXSUM_WITNESS_KEYS publicKey {public_key:?} is not 32 bytes of hex")
            })?;
            let verifying = VerifyingKey::from_bytes(name, public).map_err(|_| {
                format!("OXSUM_WITNESS_KEYS publicKey {public_key:?} is not an Ed25519 key")
            })?;
            let key_hash = field("keyHash")?;
            let expected = verifying
                .cosignature_key_hash()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            if !key_hash.eq_ignore_ascii_case(&expected) {
                return Err(format!(
                    "OXSUM_WITNESS_KEYS keyHash {key_hash:?} does not match the key's cosignature selector {expected:?}"
                ));
            }
            Ok(WitnessKey {
                witness_url: url,
                key_name,
                public_key: public_key.to_ascii_lowercase(),
                key_hash: expected,
            })
        })
        .collect()
}

/// Decodes a fixed-length hex string; used to validate `OXSUM_WITNESS_KEYS`
/// pins at boot.
fn hex_decode<const N: usize>(hex: &str) -> Result<[u8; N], ()> {
    let bytes = hex.as_bytes();
    if bytes.len() != N * 2 {
        return Err(());
    }
    let mut out = [0u8; N];
    for (i, &[hi, lo]) in bytes.as_chunks::<2>().0.iter().enumerate() {
        let hi = (hi as char).to_digit(16).ok_or(())?;
        let lo = (lo as char).to_digit(16).ok_or(())?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

/// Parses the deployment's own tlog-witness (issue #260):
/// `OXSUM_WITNESS_SIGNING_KEY` (32 bytes, base64) opts in — unset means no
/// witness and `/add-checkpoint` is 404. `OXSUM_WITNESS_KEY_NAME` names the
/// cosignature lines, default `oxsum/witness`. `OXSUM_WITNESS_TRUST` is the
/// trust roots, comma-separated `origin=size:root` entries — the heads the
/// operator independently confirmed before any submission may arrive.
///
/// A seed without a valid base64, a key name that is not a valid note key
/// name, or a trust entry that does not parse — or a trust list left empty
/// when a seed is set — refuses to start: a witness that tracks nothing can
/// only answer 404, and a misconfigured one would cosign under a key the
/// operator cannot name.
fn witness_serve_of() -> Result<Option<WitnessServe>, String> {
    let Some(seed_raw) = var("OXSUM_WITNESS_SIGNING_KEY") else {
        return Ok(None);
    };
    let seed = oxsum_core::seed_from_base64(&seed_raw)
        .map_err(|error| format!("OXSUM_WITNESS_SIGNING_KEY: {error}"))?;
    let key_name = var("OXSUM_WITNESS_KEY_NAME").unwrap_or_else(|| "oxsum/witness".to_owned());
    oxsum_core::WitnessKeyName::new(key_name.clone())
        .map_err(|error| format!("OXSUM_WITNESS_KEY_NAME {key_name:?}: {error}"))?;
    let trust = var("OXSUM_WITNESS_TRUST")
        .map(|raw| parse_witness_trust(&raw))
        .transpose()?
        .unwrap_or_default();
    if trust.is_empty() {
        return Err(
            "OXSUM_WITNESS_SIGNING_KEY is set but OXSUM_WITNESS_TRUST names no origins \
             — a witness that tracks nothing can only ever answer 404"
                .to_owned(),
        );
    }
    Ok(Some(WitnessServe {
        seed,
        key_name,
        trust,
    }))
}

/// Parses one `OXSUM_WITNESS_TRUST` entry list: comma-separated
/// `origin=size:root`. The first `=` splits the pair — an origin never
/// contains `=` — and the `:` after it splits size from root. An empty
/// origin, a non-numeric size, an origin the note format would not carry, or
/// a root that is not 32 bytes of base64 is a startup error.
fn parse_witness_trust(raw: &str) -> Result<Vec<WitnessTrust>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (origin, head) = entry.split_once('=').ok_or_else(|| {
                format!("OXSUM_WITNESS_TRUST entry {entry:?} is not `origin=size:root`")
            })?;
            let origin = origin.trim();
            // The origin is the note's first line — validate it against the
            // same rules the parser will apply, so a pin that could never
            // match a submission fails at boot rather than at request time.
            oxsum_core::Origin::new(origin)
                .map_err(|error| format!("OXSUM_WITNESS_TRUST entry {entry:?}: {error}"))?;
            let (size, root) = head.split_once(':').ok_or_else(|| {
                format!("OXSUM_WITNESS_TRUST entry {entry:?} is not `origin=size:root`")
            })?;
            let size = size.trim().parse::<u64>().map_err(|_| {
                format!("OXSUM_WITNESS_TRUST entry {entry:?} size is not an integer")
            })?;
            let root = root.trim();
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(root)
                .ok()
                .filter(|b| b.len() == 32)
                .ok_or_else(|| {
                    format!("OXSUM_WITNESS_TRUST entry {entry:?} root is not 32 bytes of base64")
                })?;
            Ok(WitnessTrust {
                origin: origin.to_owned(),
                size,
                root: root.to_owned(),
            })
        })
        .collect()
}

/// Parses `OXSUM_UPSTREAM_PRICE_SYNC`: whether the `SYNC_UPSTREAM_PRICES`
/// periodic job probes each channel's live catalog for drift (issue #222).
///
/// `true`/`1` enable it, `false`/`0` (and absence) leave the stored upstream
/// rates unprobed — the default, matching the other opt-in switches. Anything
/// else refuses to start rather than guess.
fn upstream_price_sync_of(raw: String) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(format!(
            "OXSUM_UPSTREAM_PRICE_SYNC must be true or false, got {other:?}"
        )),
    }
}

/// Parses `OXSUM_EVIDENCE_UPSTREAM_COST` (issue #616): whether evidence
/// bundles export `upstreamCostMinor` on usage records. `true`/`1` opt in;
/// `false`/`0`/absent keep the platform's supplier cost redacted — the
/// default. Anything else refuses to start rather than guess.
fn evidence_upstream_cost_of() -> Result<bool, String> {
    var("OXSUM_EVIDENCE_UPSTREAM_COST")
        .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            other => Err(format!(
                "OXSUM_EVIDENCE_UPSTREAM_COST must be true or false, got {other:?}"
            )),
        })
        .transpose()
        .map(|opt| opt.unwrap_or(false))
}

/// Parses `OXSUM_RATE_LIMIT_BACKEND` (issue #498) — `memory`/`postgres`, absent
/// meaning `memory` so the single-binary story is unchanged.
fn rate_limit_backend_of() -> Result<RateLimitBackend, String> {
    var("OXSUM_RATE_LIMIT_BACKEND")
        .map(|raw| RateLimitBackend::parse(&raw))
        .transpose()
        .map(|opt| opt.unwrap_or_default())
}

/// Parses the provider the `upstream-bill-sync` job reconciles through (issue
/// #283). `OXSUM_UPSTREAM_BILL_SYNC` is the gate — `true`/`1` mounts the OpenAI
/// Costs implementation, `false`/`0`/absent schedules no bill pass — and the
/// whole thing needs `OXSUM_SECRET_KEY`, because `billable_channels` unseals
/// every channel's `bill_key_sealed` under it before the provider is asked.
/// Enabling without a sealing key is a startup error, the same shape
/// `OXSUM_UPSTREAM_PRICE_SYNC` takes: a half-configured pass only fails at the
/// first fetch, inside the worker.
fn bill_provider_of(
    secret: Option<&SecretKey>,
) -> Result<Option<Arc<dyn oxsum_core::UpstreamBillProvider>>, String> {
    let enabled = var("OXSUM_UPSTREAM_BILL_SYNC")
        .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            other => Err(format!(
                "OXSUM_UPSTREAM_BILL_SYNC must be true or false, got {other:?}"
            )),
        })
        .transpose()?
        .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }
    if secret.is_none() {
        return Err(
            "OXSUM_UPSTREAM_BILL_SYNC needs OXSUM_SECRET_KEY — billable_channels \
             unseals every channel's bill_key_sealed under it"
                .into(),
        );
    }
    Ok(Some(Arc::new(
        oxsum_integrations::bill_providers::CompositeBillProvider::new(vec![
            Arc::new(oxsum_integrations::openai_costs::OpenAiBillProvider::new(
                oxsum_integrations::provider_http::ProviderHttp::default(),
            )),
            Arc::new(
                oxsum_integrations::anthropic_costs::AnthropicBillProvider::new(
                    oxsum_integrations::provider_http::ProviderHttp::default(),
                ),
            ),
        ]),
    )))
}

/// Parses the deployment-wide display currency (issue #257):
/// `OXSUM_DISPLAY_CURRENCY` (a three-letter code) plus
/// `OXSUM_DISPLAY_RATE_PER_CREDIT` (a decimal literal — what one credit
/// renders as in that unit) and the optional `OXSUM_DISPLAY_DECIMALS`
/// (default 2). Either both of the first two are set, or neither: a rate
/// without a unit and a unit without a rate are both half-configured.
fn display_currency_of() -> Result<Option<oxsum_core::Display>, String> {
    let code = var("OXSUM_DISPLAY_CURRENCY");
    let rate = var("OXSUM_DISPLAY_RATE_PER_CREDIT");
    let decimals = var("OXSUM_DISPLAY_DECIMALS")
        .map(|raw| {
            raw.trim()
                .parse::<u8>()
                .map_err(|_| format!("OXSUM_DISPLAY_DECIMALS must be a small integer, got {raw:?}"))
        })
        .transpose()?
        .unwrap_or(2);
    oxsum_core::display_currency(code.as_deref(), rate.as_deref(), decimals)
        .map_err(|error| format!("OXSUM_DISPLAY_CURRENCY / OXSUM_DISPLAY_RATE_PER_CREDIT: {error}"))
}

/// Parses the `OXSUM_BUSINESS_NAME` / `OXSUM_BRAND_*` set (issue #614, T23).
/// Each field is independently optional — a deployment dresses the surfaces it
/// cares about and leaves the rest at the compiled-in default. The accent is
/// validated as `#rrggbb` so a typo fails at startup, not as a broken CSS
/// custom property on every page.
fn branding_of() -> Result<Branding, String> {
    let name = var("OXSUM_BUSINESS_NAME").unwrap_or_else(|| "oxsum".to_owned());
    if name.chars().count() > 80 {
        return Err(format!(
            "OXSUM_BUSINESS_NAME must be at most 80 characters, got {name:?}"
        ));
    }
    let logo_url = var("OXSUM_BRAND_LOGO_URL");
    let accent_color = var("OXSUM_BRAND_ACCENT_COLOR")
        .map(|raw| {
            let ok = raw.len() == 7
                && raw.starts_with('#')
                && raw[1..].chars().all(|c| c.is_ascii_hexdigit());
            if ok { Ok(raw) } else { Err(raw) }
        })
        .transpose()
        .map_err(|raw| format!("OXSUM_BRAND_ACCENT_COLOR must be a #rrggbb color, got {raw:?}"))?;
    Branding {
        name,
        logo_url,
        accent_color,
        contact_email: None,
        help_url: None,
    }
    .validated()
    .map_err(|error| error.to_string())
}

/// Parses one `OXSUM_RETENTION_*` variable: a positive day count — `0` would
/// erase history the retention spec calls permanent.
fn retention_days(name: &str) -> Result<Option<i64>, String> {
    var(name)
        .map(|raw| {
            raw.trim()
                .parse::<i64>()
                .ok()
                .filter(|days| *days > 0)
                .ok_or_else(|| format!("{name} must be a positive number of days, got {raw:?}"))
        })
        .transpose()
}

/// Parses `OXSUM_SIGNUP_BONUS_MINOR`: the credits a new organization starts with, in minor
/// units.
///
/// A non-negative integer, and absent or `0` grants nothing — the default keeps registration
/// from touching the ledger at all. A negative or unparsable value refuses to start rather
/// than quietly grant or charge the wrong thing.
fn signup_bonus_of(raw: String) -> Result<i64, String> {
    match raw.trim().parse::<i64>() {
        Ok(minor) if minor >= 0 => Ok(minor),
        _ => Err(format!(
            "OXSUM_SIGNUP_BONUS_MINOR must be a non-negative integer of minor units, got {raw:?}"
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

/// Parses the mailer trio: `OXSUM_SMTP_URL`, `OXSUM_MAIL_FROM` and
/// `OXSUM_PUBLIC_URL`. All three or none — a subset is a startup error, because
/// a half-configured mailer only fails at send time, inside a request, where the
/// operator is not watching.
fn mailer_of() -> Result<Option<Mailer>, String> {
    let smtp_url = var("OXSUM_SMTP_URL");
    let from = var("OXSUM_MAIL_FROM");
    let public_url = var("OXSUM_PUBLIC_URL");
    if smtp_url.is_none() && from.is_none() && public_url.is_none() {
        return Ok(None);
    }
    let (smtp_url, from, public_url) = match (smtp_url, from, public_url) {
        (Some(u), Some(f), Some(p)) => (u, f, p),
        _ => {
            return Err(
                "OXSUM_SMTP_URL, OXSUM_MAIL_FROM and OXSUM_PUBLIC_URL are set together, \
                 or none of them"
                    .into(),
            );
        }
    };
    Mailer::try_new(&smtp_url, &from, &public_url).map(Some)
}

/// Parses the GitHub OAuth configuration: `OXSUM_GITHUB_CLIENT_ID`,
/// `OXSUM_GITHUB_CLIENT_SECRET` and `OXSUM_PUBLIC_URL` — the client pair set
/// together, and the public origin the callback URL is built from required
/// alongside, because a pair without it can only fail inside the callback.
/// `OXSUM_GITHUB_WEB_URL` and `OXSUM_GITHUB_API_URL` override the provider's
/// base URLs; nothing but a test points them anywhere else.
fn github_of() -> Result<Option<GitHub>, String> {
    let client_id = var("OXSUM_GITHUB_CLIENT_ID");
    let client_secret = var("OXSUM_GITHUB_CLIENT_SECRET");
    if client_id.is_none() && client_secret.is_none() {
        return Ok(None);
    }
    let public_url = var("OXSUM_PUBLIC_URL");
    let (client_id, client_secret, public_url) = match (client_id, client_secret, public_url) {
        (Some(i), Some(s), Some(p)) => (i, s, p),
        _ => {
            return Err(
                "OXSUM_GITHUB_CLIENT_ID and OXSUM_GITHUB_CLIENT_SECRET are set together, \
                 and both require OXSUM_PUBLIC_URL"
                    .into(),
            );
        }
    };
    let web_url = var("OXSUM_GITHUB_WEB_URL").unwrap_or_else(|| "https://github.com".to_owned());
    let api_url =
        var("OXSUM_GITHUB_API_URL").unwrap_or_else(|| "https://api.github.com".to_owned());
    Ok(Some(GitHub::new(
        client_id,
        client_secret,
        public_url,
        web_url,
        api_url,
    )))
}

/// Parses `OXSUM_OIDC_PROVIDERS` (issue #504): a JSON array of
/// `{name, issuer, clientId, clientSecret}` — one per enterprise IdP the
/// deployment federates logins to. `OXSUM_PUBLIC_URL` is required alongside
/// (the callback URL each provider builds its redirect on); providers without
/// it can only fail inside the callback, so a set list plus a missing origin is
/// a startup error, not a login-time one. An absent list parses to `[]` — no
/// OIDC login, the `{provider}` routes 404.
fn oidc_providers_of(public_url: Option<&str>) -> Result<Vec<OidcProvider>, String> {
    let Some(raw) = var("OXSUM_OIDC_PROVIDERS") else {
        return Ok(Vec::new());
    };
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ProviderEntry {
        name: String,
        issuer: String,
        client_id: String,
        client_secret: String,
    }
    let entries: Vec<ProviderEntry> = serde_json::from_str(&raw)
        .map_err(|e| format!("OXSUM_OIDC_PROVIDERS is not a JSON array of providers: {e}"))?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let Some(public_url) = public_url else {
        return Err(
            "OXSUM_OIDC_PROVIDERS requires OXSUM_PUBLIC_URL: the callback each provider \
             builds its redirect on"
                .into(),
        );
    };
    entries
        .into_iter()
        .map(|entry| {
            if entry.name.is_empty() || entry.name.contains(':') || entry.name.contains('/') {
                return Err(format!(
                    "OXSUM_OIDC_PROVIDERS provider name {:?} must be non-empty and hold no ':' or '/'",
                    entry.name
                ));
            }
            if entry.issuer.is_empty() || entry.client_id.is_empty() {
                return Err(format!(
                    "OXSUM_OIDC_PROVIDERS provider {:?} needs a non-empty issuer and clientId",
                    entry.name
                ));
            }
            Ok(OidcProvider::new(
                entry.name,
                entry.issuer,
                entry.client_id,
                entry.client_secret,
                public_url.to_owned(),
            ))
        })
        .collect()
}

/// Parses the Stripe pair: `OXSUM_STRIPE_SECRET_KEY` and
/// `OXSUM_STRIPE_WEBHOOK_SECRET` together or not at all — a lone key is a
/// startup error, because a half-configured rail only fails inside a request.
/// `OXSUM_PUBLIC_URL` is required alongside: checkout redirects are built on
/// it. `OXSUM_STRIPE_API_URL` overrides the API origin; nothing but a test
/// points it anywhere else.
fn stripe_of() -> Result<Option<Stripe>, String> {
    let secret_key = var("OXSUM_STRIPE_SECRET_KEY");
    let webhook_secret = var("OXSUM_STRIPE_WEBHOOK_SECRET");
    if secret_key.is_none() && webhook_secret.is_none() {
        return Ok(None);
    }
    let public_url = var("OXSUM_PUBLIC_URL");
    let (Some(secret_key), Some(webhook_secret), Some(public_url)) =
        (secret_key, webhook_secret, public_url)
    else {
        return Err(
            "OXSUM_STRIPE_SECRET_KEY, OXSUM_STRIPE_WEBHOOK_SECRET and OXSUM_PUBLIC_URL \
             are set together, or none of them"
                .into(),
        );
    };
    let api_url =
        var("OXSUM_STRIPE_API_URL").unwrap_or_else(|| "https://api.stripe.com".to_owned());
    Ok(Some(Stripe::new(
        secret_key,
        webhook_secret,
        api_url,
        public_url,
    )))
}

/// Parses the Airwallex rail (issue #285): `OXSUM_AIRWALLEX_CLIENT_ID`,
/// `OXSUM_AIRWALLEX_API_KEY` and `OXSUM_AIRWALLEX_WEBHOOK_SECRET` together or
/// not at all — a subset is a startup error, because a half-configured rail
/// only fails inside a request. `OXSUM_PUBLIC_URL` is required alongside: the
/// intent's `return_url` is built on it. `OXSUM_AIRWALLEX_API_URL` and
/// `OXSUM_AIRWALLEX_CHECKOUT_URL` override the API and hosted-page origins;
/// nothing but a test points them anywhere else.
fn airwallex_of() -> Result<Option<Airwallex>, String> {
    let client_id = var("OXSUM_AIRWALLEX_CLIENT_ID");
    let api_key = var("OXSUM_AIRWALLEX_API_KEY");
    let webhook_secret = var("OXSUM_AIRWALLEX_WEBHOOK_SECRET");
    if client_id.is_none() && api_key.is_none() && webhook_secret.is_none() {
        return Ok(None);
    }
    let public_url = var("OXSUM_PUBLIC_URL");
    let (Some(client_id), Some(api_key), Some(webhook_secret), Some(public_url)) =
        (client_id, api_key, webhook_secret, public_url)
    else {
        return Err("OXSUM_AIRWALLEX_CLIENT_ID, OXSUM_AIRWALLEX_API_KEY, \
             OXSUM_AIRWALLEX_WEBHOOK_SECRET and OXSUM_PUBLIC_URL are set together, \
             or none of them"
            .into());
    };
    let api_url =
        var("OXSUM_AIRWALLEX_API_URL").unwrap_or_else(|| "https://api.airwallex.com".to_owned());
    let checkout_url = var("OXSUM_AIRWALLEX_CHECKOUT_URL")
        .unwrap_or_else(|| "https://checkout.airwallex.com".to_owned());
    // The Airwallex demo endpoint's TLS 1.3 records do not decrypt under
    // rustls (issue #374); the rail's HTTP client is capped at TLS 1.2, the
    // same transport the test exercises. A TLS-1.3-capable caller can pass
    // its own client through `Airwallex::new` if it needs one — the seam is
    // already there.
    let http = reqwest::Client::builder()
        .max_tls_version(reqwest::tls::Version::TLS_1_2)
        .build()
        .map_err(|e| format!("airwallex http client: {e}"))?;
    Ok(Some(Airwallex::new(
        client_id,
        api_key,
        webhook_secret,
        api_url,
        checkout_url,
        public_url,
        http,
    )))
}

/// Parses the Hyperswitch rail (issue #319): `OXSUM_HYPERSWITCH_API_KEY`,
/// `OXSUM_HYPERSWITCH_WEBHOOK_SECRET` and `OXSUM_HYPERSWITCH_PUBLISHABLE_KEY`
/// together or not at all — a subset is a startup error, because a
/// half-configured rail only fails inside a request. `OXSUM_PUBLIC_URL` is
/// required alongside: the payment's `return_url` is built on it.
/// `OXSUM_HYPERSWITCH_BASE_URL` overrides the switch origin (the hosted
/// sandbox by default); only a self-hosted deployment, or a test standing in a
/// stub, points it elsewhere.
fn hyperswitch_of() -> Result<Option<Hyperswitch>, String> {
    let api_key = var("OXSUM_HYPERSWITCH_API_KEY");
    let webhook_secret = var("OXSUM_HYPERSWITCH_WEBHOOK_SECRET");
    let publishable_key = var("OXSUM_HYPERSWITCH_PUBLISHABLE_KEY");
    if api_key.is_none() && webhook_secret.is_none() && publishable_key.is_none() {
        return Ok(None);
    }
    let public_url = var("OXSUM_PUBLIC_URL");
    let (Some(api_key), Some(webhook_secret), Some(publishable_key), Some(public_url)) =
        (api_key, webhook_secret, publishable_key, public_url)
    else {
        return Err(
            "OXSUM_HYPERSWITCH_API_KEY, OXSUM_HYPERSWITCH_WEBHOOK_SECRET, \
             OXSUM_HYPERSWITCH_PUBLISHABLE_KEY and OXSUM_PUBLIC_URL are set together, \
             or none of them"
                .into(),
        );
    };
    let base_url = var("OXSUM_HYPERSWITCH_BASE_URL")
        .unwrap_or_else(|| "https://sandbox.hyperswitch.io".to_owned());
    // The SDK bundle origin is a different service from the API origin: the
    // hosted sandbox serves it on beta.hyperswitch.io; a self-hosted sidecar
    // on its hyperswitch-web port (issue #320).
    let sdk_url = var("OXSUM_HYPERSWITCH_SDK_URL")
        .unwrap_or_else(|| "https://beta.hyperswitch.io/v1".to_owned());
    Ok(Some(Hyperswitch::new(
        api_key,
        webhook_secret,
        base_url,
        publishable_key,
        public_url,
        sdk_url,
    )))
}

/// Parses the x402 rail's verifier (issues #264, #282). Two configurations:
///
/// - **The real rail**: `OXSUM_X402_FACILITATOR_URL` + `OXSUM_X402_NETWORK` +
///   `OXSUM_X402_ASSET` + `OXSUM_X402_PAY_TO` — all four or none, a subset is a
///   startup error like the Stripe trio. Delegates verify/settle to the x402-rs
///   facilitator sidecar.
/// - **The stand-in**: `OXSUM_X402_VERIFYING_KEY` alone, unchanged — a
///   test-scheme Ed25519 check for deployments and suites without a chain.
///
/// Unset both and the rail is absent — `POST /api/v1/topups/x402` answers 503.
/// A facilitator URL beside a stray subset of its set is a startup error; the
/// stand-in key is ignored when the facilitator is configured (the real rail
/// wins, never a silent fallback).
fn x402_of() -> Result<Option<Arc<dyn oxsum_core::RailVerifier>>, String> {
    let url = var("OXSUM_X402_FACILITATOR_URL");
    let network = var("OXSUM_X402_NETWORK");
    let asset = var("OXSUM_X402_ASSET");
    let pay_to = var("OXSUM_X402_PAY_TO");
    let present = [&url, &network, &asset, &pay_to]
        .iter()
        .filter(|v| v.is_some())
        .count();
    if let (Some(url), Some(network), Some(asset), Some(pay_to)) = (url, network, asset, pay_to) {
        let verifier = oxsum_integrations::x402_facilitator::X402FacilitatorVerifier::new(
            oxsum_integrations::facilitator::Facilitator::new(url),
            reqwest::Client::new(),
            network,
            asset,
            pay_to,
        );
        return Ok(Some(Arc::new(verifier)));
    }
    if present > 0 {
        return Err(
            "OXSUM_X402_FACILITATOR_URL, OXSUM_X402_NETWORK, OXSUM_X402_ASSET and \
             OXSUM_X402_PAY_TO are set together, or none"
                .into(),
        );
    }
    let Some(key) = var("OXSUM_X402_VERIFYING_KEY") else {
        return Ok(None);
    };
    oxsum_core::Ed25519RailVerifier::from_hex(&key)
        .map(|v| Some(Arc::new(v) as Arc<dyn oxsum_core::RailVerifier>))
        .map_err(|error| error.to_string())
}

/// What the boot proved about where the rail's `payTo` lives — the custody
/// half of issue #1143. The server holds no spend key; this records whether
/// the receiving address was verified to be the multisig vault it should be.
#[derive(Debug, PartialEq, Eq)]
enum SolanaCustody {
    /// `OXSUM_SOLANA_MULTISIG_ACCOUNT` was set and `payTo` *is* the multisig's
    /// `vault index 0` PDA — the strongest statement the boot can make
    /// without on-chain introspection.
    VerifiedMultisigVault,
    /// `OXSUM_SOLANA_MULTISIG_ACCOUNT` was unset — `payTo` is unverified and
    /// is treated as a hot wallet (allowed on a devnet loop, warned loudly).
    UnverifiedHotWallet,
}

/// Validates `OXSUM_SOLANA_PAY_TO` against the Squads v4 multisig
/// `OXSUM_SOLANA_MULTISIG_ACCOUNT` names (issue #1143). When the multisig is
/// configured, the boot derives its `vault index 0` PDA and requires `payTo`
/// to equal it — a mismatch means the receiving address is not the vault the
/// multisig implies (a pasted hot-wallet, a vault of another index, or a
/// multisig that was re-created), and startup refuses. When the multisig is
/// unset the rail still boots — a plain wallet is a legitimate devnet
/// `payTo` — but the warn marks it as unverified custody, never silently.
///
/// This is a derivation check only, deliberately: it proves `payTo` equals
/// the PDA the multisig account implies, not that the multisig's
/// threshold/members are the intended signers. Reading the multisig's policy
/// on-chain is the residual `docs/self-hosted-solana-multisig.md` records.
fn solana_custody_of(
    multisig_account: Option<&str>,
    pay_to: &str,
) -> Result<SolanaCustody, String> {
    let Some(multisig_account) = multisig_account else {
        tracing::warn!(
            pay_to,
            "OXSUM_SOLANA_MULTISIG_ACCOUNT is unset: the Solana rail's payTo is \
             NOT verified as a multisig vault — it is a hot wallet. A devnet loop \
             may keep this, but a production deployment should receive into a \
             Squads v4 multisig's vault PDA; set OXSUM_SOLANA_MULTISIG_ACCOUNT to \
             the multisig account so the boot can prove payTo is its vault index 0"
        );
        return Ok(SolanaCustody::UnverifiedHotWallet);
    };
    let vault = oxsum_integrations::squads::MultisigVault {
        multisig_account,
        vault_index: 0,
    };
    oxsum_integrations::squads::check_pay_to_is_vault(vault, pay_to).map_err(|e| match e {
        oxsum_integrations::squads::VaultCheckError::PayToIsNotVault {
            derived,
            configured,
        } => {
            format!(
                "OXSUM_SOLANA_PAY_TO ({configured}) is not the vault OXSUM_SOLANA_MULTISIG_ACCOUNT \
                 implies — the multisig's vault index 0 derives to {derived}. Point PAY_TO at that \
                 vault, or unset OXSUM_SOLANA_MULTISIG_ACCOUNT to run an unverified hot wallet"
            )
        }
        other => format!("OXSUM_SOLANA_MULTISIG_ACCOUNT could not be verified: {other}"),
    })?;
    Ok(SolanaCustody::VerifiedMultisigVault)
}

/// Parses the Solana deposit rail (issue #1117). The complete set:
/// `OXSUM_SOLANA_RPC_URL` + `OXSUM_SOLANA_PAY_TO`, plus a rate source — a
/// Pyth oracle for a native-SOL intent (the default, issue #1139) or the
/// static `OXSUM_SOLANA_RATE_MICRO` a devnet deployment pins behind
/// `OXSUM_SOLANA_DEVNET_STATIC_RATE=1`. `OXSUM_SOLANA_SPL_TOKEN_MINT` /
/// `OXSUM_SOLANA_COMMITMENT` / `OXSUM_SOLANA_INTENT_TTL_SECS` optional —
/// the requireds or none, a subset is a startup error like the Stripe trio.
/// Unset and the rail is absent: `POST /api/v1/topups/solana` answers 503 and
/// the `solana-deposits` job schedules nothing.
fn solana_of() -> Result<Option<Arc<oxsum_core::SolanaRailVerifier>>, String> {
    let rpc_url = var("OXSUM_SOLANA_RPC_URL");
    let pay_to = var("OXSUM_SOLANA_PAY_TO");
    let rate_micro = var("OXSUM_SOLANA_RATE_MICRO");
    let spl_token_mint = var("OXSUM_SOLANA_SPL_TOKEN_MINT");
    let devnet_static = var("OXSUM_SOLANA_DEVNET_STATIC_RATE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let present = [&rpc_url, &pay_to, &rate_micro, &spl_token_mint]
        .iter()
        .filter(|v| v.is_some())
        .count();
    // Any PYTH_* knob or the devnet flag also counts as "the operator meant
    // the rail" — a lone `OXSUM_SOLANA_PYTH_*` must not be silently ignored.
    let pyth_touched = [
        "OXSUM_SOLANA_PYTH_URL",
        "OXSUM_SOLANA_PYTH_FEED_ID",
        "OXSUM_SOLANA_PYTH_MAX_AGE_SECS",
        "OXSUM_SOLANA_PYTH_MAX_CONF_RATIO",
    ]
    .iter()
    .any(|k| var(k).is_some());
    if present == 0 && !devnet_static && !pyth_touched {
        return Ok(None);
    }
    let (Some(rpc_url), Some(pay_to)) = (rpc_url, pay_to) else {
        return Err(
            "OXSUM_SOLANA_RPC_URL and OXSUM_SOLANA_PAY_TO are set together, or none".into(),
        );
    };
    let mint_configured = spl_token_mint
        .as_deref()
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    // The rate source (issue #1139): an SPL intent always prices at the fixed
    // static conversion — `RATE_MICRO` is required for it. A native-SOL intent
    // is oracle-priced by default: `RATE_MICRO` is the devnet stand-in and is
    // only read behind the explicit `OXSUM_SOLANA_DEVNET_STATIC_RATE=1` opt-in
    // — set on a SOL deployment without the flag, it is a startup error, never
    // a silent mainnet fallback.
    let rate_micro: i64 = match rate_micro {
        Some(raw) => {
            let v: i64 = raw
                .parse()
                .map_err(|_| "OXSUM_SOLANA_RATE_MICRO must be an integer".to_owned())?;
            if v <= 0 {
                return Err("OXSUM_SOLANA_RATE_MICRO must be positive".into());
            }
            v
        }
        None => 0,
    };
    if mint_configured && rate_micro == 0 {
        return Err(
            "OXSUM_SOLANA_RATE_MICRO must be set when OXSUM_SOLANA_SPL_TOKEN_MINT is — \
             an SPL intent prices at the fixed conversion, no oracle is consulted"
                .into(),
        );
    }
    if !mint_configured && rate_micro != 0 && !devnet_static {
        return Err(
            "OXSUM_SOLANA_RATE_MICRO may only price a native-SOL intent behind \
             OXSUM_SOLANA_DEVNET_STATIC_RATE=1 — a mainnet SOL intent is \
             oracle-priced and this would silently pin a static rate"
                .into(),
        );
    }
    if !mint_configured && devnet_static && rate_micro == 0 {
        return Err(
            "OXSUM_SOLANA_DEVNET_STATIC_RATE=1 pins a native-SOL intent to \
             OXSUM_SOLANA_RATE_MICRO — set it too (a zero rate would open \
             zero-amount intents)"
                .into(),
        );
    }
    // The pricing the verifier resolves at open: static for SPL or the devnet
    // flag, the Pyth oracle otherwise. A SOL deployment needs no extra config
    // to oracle-price — the Hermes endpoint and the SOL/USD feed default.
    let (pricing, oracle) = if mint_configured || devnet_static {
        (oxsum_core::SolanaPricing::Static, None)
    } else {
        let feed_id = var("OXSUM_SOLANA_PYTH_FEED_ID")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| oxsum_integrations::pyth::DEFAULT_SOL_USD_FEED_ID.to_owned());
        let max_quote_age_secs = var("OXSUM_SOLANA_PYTH_MAX_AGE_SECS")
            .map(|v| v.parse::<i64>())
            .transpose()
            .map_err(|_| "OXSUM_SOLANA_PYTH_MAX_AGE_SECS must be an integer".to_owned())?
            .unwrap_or(60);
        let max_conf_ratio = var("OXSUM_SOLANA_PYTH_MAX_CONF_RATIO")
            .map(|v| v.parse::<f64>())
            .transpose()
            .map_err(|_| "OXSUM_SOLANA_PYTH_MAX_CONF_RATIO must be a number".to_owned())?
            .unwrap_or(0.01);
        let pyth_url = var("OXSUM_SOLANA_PYTH_URL")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://hermes.pyth.network".to_owned());
        let pyth = oxsum_integrations::pyth::PythClient::new(pyth_url, crate::jobs::http_client());
        (
            oxsum_core::SolanaPricing::Oracle {
                feed_id,
                max_quote_age_secs,
                max_conf_ratio,
            },
            Some(Arc::new(pyth) as Arc<dyn oxsum_core::PriceOracle>),
        )
    };
    // Default finalized for mainnet-grade finality; a devnet deployment sets
    // `OXSUM_SOLANA_COMMITMENT=confirmed` explicitly for latency. The wrong
    // direction is the dangerous one — a `confirmed` default would silently
    // under-protect a mainnet cut (issue #1129).
    let commitment = var("OXSUM_SOLANA_COMMITMENT").unwrap_or_else(|| "finalized".to_owned());
    if commitment != "confirmed" && commitment != "finalized" {
        return Err("OXSUM_SOLANA_COMMITMENT must be confirmed or finalized".into());
    }
    // When an SPL mint is configured, decimals must be declared explicitly —
    // silently defaulting to SOL's 9 would misprice a 6-decimal mint 1000×.
    // The boot then verifies the declared figure against the mint's on-chain
    // decimals in `main` (SolanaRpcClient::mint_decimals) and refuses to start
    // on a mismatch. Native SOL keeps the 9 default.
    let asset_decimals = match (var("OXSUM_SOLANA_DECIMALS"), mint_configured) {
        (Some(v), _) => v
            .parse::<u32>()
            .map_err(|_| "OXSUM_SOLANA_DECIMALS must be a non-negative integer".to_owned())?,
        (None, true) => {
            return Err(
                "OXSUM_SOLANA_DECIMALS must be set when OXSUM_SOLANA_SPL_TOKEN_MINT is — \
                 it is checked against the mint's on-chain decimals at startup"
                    .into(),
            );
        }
        (None, false) => 9,
    };
    let intent_ttl_secs = var("OXSUM_SOLANA_INTENT_TTL_SECS")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| "OXSUM_SOLANA_INTENT_TTL_SECS must be an integer".to_owned())?
        .unwrap_or(600);
    // The open-intent surface's bounds (issue #1130): per-call amount limits
    // and a per-organization opens-per-minute cap. Defaults keep today's
    // behavior (min 1, unbounded max, unbounded rate) — a deployment tightens
    // them deliberately.
    let min_amount_minor = var("OXSUM_SOLANA_MIN_MINOR")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| "OXSUM_SOLANA_MIN_MINOR must be an integer".to_owned())?
        .unwrap_or(1);
    let max_amount_minor = var("OXSUM_SOLANA_MAX_MINOR")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| "OXSUM_SOLANA_MAX_MINOR must be an integer".to_owned())?
        .unwrap_or(i64::MAX);
    let opens_per_minute = var("OXSUM_SOLANA_OPENS_PER_MINUTE")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| "OXSUM_SOLANA_OPENS_PER_MINUTE must be an integer".to_owned())?
        .unwrap_or(0);
    // Custody check (issue #1143): when `OXSUM_SOLANA_MULTISIG_ACCOUNT` names
    // the Squads v4 multisig the deployment receives into, `PAY_TO` must be
    // that multisig's `vault index 0` PDA — the derivation is run offline and
    // a mismatch is a startup error, so a pasted hot-wallet can never
    // masquerade as the vault. Unset, the rail is unverified: a loud warn
    // marks `payTo` as a hot wallet the operator chose not to pin.
    solana_custody_of(var("OXSUM_SOLANA_MULTISIG_ACCOUNT").as_deref(), &pay_to)?;
    let config = oxsum_integrations::solana::SolanaConfig {
        rpc_url: rpc_url.clone(),
        pay_to: pay_to.clone(),
        spl_token_mint: spl_token_mint.clone().filter(|s| !s.is_empty()),
        commitment,
        asset_decimals,
        rate_micro,
        intent_ttl_secs,
        min_amount_minor,
        max_amount_minor,
        opens_per_minute,
    };
    let client = oxsum_integrations::solana::SolanaRpcClient::new(
        rpc_url,
        crate::jobs::http_client(),
        config,
    );
    let terms = oxsum_core::SolanaTerms {
        pay_to,
        spl_token_mint,
        asset_decimals,
        rate_micro,
        intent_ttl_secs,
        min_amount_minor,
        max_amount_minor,
        opens_per_minute,
    };
    Ok(Some(Arc::new(
        oxsum_core::SolanaRailVerifier::with_pricing(Arc::new(client), terms, pricing, oracle),
    )))
}

/// Cross-checks `OXSUM_RAILS` (issue #619): the comma-separated deposit rails
/// the operator declares — `stripe`, `airwallex`, `hyperswitch`, `x402` — must
/// match the rails the environment actually completes. Naming a rail whose
/// `OXSUM_<RAIL>_*` set is absent or partial fails the boot with the rail
/// named, and *configuring* a rail the list omits also fails: a rail that is
/// on without the operator meaning it is the worse half of the same
/// contradiction. `None` means no declaration — every configured rail stands,
/// the single-binary default.
fn rails_intent_of(
    raw: Option<&str>,
    stripe: Option<&Stripe>,
    airwallex: Option<&Airwallex>,
    hyperswitch: Option<&Hyperswitch>,
    x402: Option<&Arc<dyn oxsum_core::RailVerifier>>,
    solana: Option<&Arc<oxsum_core::SolanaRailVerifier>>,
) -> Result<(), String> {
    let Some(raw) = raw else {
        return Ok(());
    };
    let configured: [(&str, bool); 5] = [
        ("stripe", stripe.is_some()),
        ("airwallex", airwallex.is_some()),
        ("hyperswitch", hyperswitch.is_some()),
        ("x402", x402.is_some()),
        ("solana", solana.is_some()),
    ];
    let declared: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    for rail in &declared {
        let Some((_, on)) = configured.iter().find(|(name, _)| name == rail) else {
            return Err(format!(
                "OXSUM_RAILS entry {rail:?} is not a deposit rail: \
                 expected stripe, airwallex, hyperswitch, x402 or solana"
            ));
        };
        if !on {
            return Err(format!(
                "OXSUM_RAILS declares {rail:?} but its OXSUM_{} set is not complete",
                rail.to_uppercase()
            ));
        }
    }
    for (name, on) in configured {
        if on && !declared.contains(&name) {
            return Err(format!(
                "OXSUM_{} is configured but OXSUM_RAILS does not declare {name:?} — \
                 add it to the list or remove the credentials",
                name.to_uppercase()
            ));
        }
    }
    Ok(())
}

/// Cross-checks `OXSUM_CUSTOMER_SELF_REGISTER` (issue #619): whether the
/// deployment intends customers to register themselves once the business
/// enables open registration. `true`/`1` requires the mailer — a registration
/// that cannot send its verification mail only fails inside the request, the
/// failure shape this check exists to move to boot. `false`/`0` and absence
/// declare nothing and check nothing.
fn self_register_intent_of(raw: Option<&str>, mailer: Option<&Mailer>) -> Result<(), String> {
    let Some(raw) = raw else {
        return Ok(());
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" if mailer.is_none() => Err(
            "OXSUM_CUSTOMER_SELF_REGISTER=true needs the mailer — OXSUM_SMTP_URL, \
             OXSUM_MAIL_FROM and OXSUM_PUBLIC_URL — a self-registering customer \
             must receive the verification mail"
                .into(),
        ),
        "true" | "1" | "false" | "0" => Ok(()),
        other => Err(format!(
            "OXSUM_CUSTOMER_SELF_REGISTER must be true or false, got {other:?}"
        )),
    }
}

/// Parses the Turnstile pair: `OXSUM_TURNSTILE_SITE_KEY` and
/// `OXSUM_TURNSTILE_SECRET_KEY` together or not at all — a lone key is a
/// startup error, because a half-configured check only fails inside a
/// registration. `OXSUM_TURNSTILE_VERIFY_URL` overrides the siteverify
/// endpoint; nothing but a test points it anywhere else.
fn turnstile_of() -> Result<Option<Turnstile>, String> {
    let site_key = var("OXSUM_TURNSTILE_SITE_KEY");
    let secret_key = var("OXSUM_TURNSTILE_SECRET_KEY");
    if site_key.is_none() && secret_key.is_none() {
        return Ok(None);
    }
    let (Some(site_key), Some(secret_key)) = (site_key, secret_key) else {
        return Err(
            "OXSUM_TURNSTILE_SITE_KEY and OXSUM_TURNSTILE_SECRET_KEY are set together, \
             or neither"
                .into(),
        );
    };
    let verify_url = var("OXSUM_TURNSTILE_VERIFY_URL")
        .unwrap_or_else(|| "https://challenges.cloudflare.com/turnstile/v0/siteverify".to_owned());
    Ok(Some(Turnstile::new(site_key, secret_key, verify_url)))
}

/// Environment credentials need explicit merchant identity and mode before
/// they can start new payments. This is a one-time bootstrap, not an override.
fn merchant_bootstraps_of(
    stripe: Option<&Stripe>,
    airwallex: Option<&Airwallex>,
    hyperswitch: Option<&Hyperswitch>,
) -> Result<Vec<oxsum_core::MerchantBootstrap>, String> {
    let mut result = Vec::new();
    for (prefix, configured) in [
        ("OXSUM_STRIPE", stripe.is_some()),
        ("OXSUM_AIRWALLEX", airwallex.is_some()),
        ("OXSUM_HYPERSWITCH", hyperswitch.is_some()),
    ] {
        let account = var(&format!("{prefix}_MERCHANT_ACCOUNT_ID"));
        let mode = var(&format!("{prefix}_MERCHANT_MODE"));
        if account.is_none() && mode.is_none() {
            continue;
        }
        if !configured {
            return Err(format!(
                "{prefix} merchant bootstrap requires rail credentials"
            ));
        }
        let (Some(account), Some(mode)) = (account, mode) else {
            return Err(format!(
                "{prefix}_MERCHANT_ACCOUNT_ID and {prefix}_MERCHANT_MODE are set together"
            ));
        };
        let mode = match mode.as_str() {
            "test" => oxsum_core::MerchantMode::Test,
            "live" => oxsum_core::MerchantMode::Live,
            _ => return Err(format!("{prefix}_MERCHANT_MODE must be test or live")),
        };
        let policy = var(&format!("{prefix}_MERCHANT_DESTINATION_POLICY"))
            .map(|v| {
                serde_json::from_str::<oxsum_core::ProviderDestinationPolicy>(&v)
                    .map_err(|_| format!("{prefix}_MERCHANT_DESTINATION_POLICY is invalid"))
            })
            .transpose()?
            .unwrap_or_default();
        let bootstrap = match prefix {
            "OXSUM_STRIPE" => stripe.map(|s| s.merchant_bootstrap(account, mode, policy)),
            "OXSUM_AIRWALLEX" => airwallex.map(|s| s.merchant_bootstrap(account, mode, policy)),
            _ => hyperswitch.map(|s| s.merchant_bootstrap(account, mode, policy)),
        };
        if let Some(bootstrap) = bootstrap {
            result.push(bootstrap);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::str::FromStr;

    use super::{Config, Gateway, RateLimitBackend, Signup};
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
    fn the_head_signing_seed_is_32_base64_bytes() {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
        assert_eq!(
            super::head_signing_seed_of(encoded),
            Ok([9u8; 32]),
            "whitespace is trimmed like the other secrets"
        );
        let short = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
        let err = super::head_signing_seed_of(short).unwrap_err();
        assert!(err.contains("OXSUM_HEAD_SIGNING_KEY"), "{err}");
        let err = super::head_signing_seed_of("not base64!!".into()).unwrap_err();
        assert!(err.contains("OXSUM_HEAD_SIGNING_KEY"), "{err}");
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
            .map(|token| Config::new(Signup::Invite, None).with_admin_token(&token))
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
    fn the_rails_declaration_must_match_the_configured_rails() {
        // Undeclared intent: any configured set is fine — the default.
        assert!(super::rails_intent_of(None, None, None, None, None, None).is_ok());

        // A declared rail without credentials names the rail.
        let err = super::rails_intent_of(Some("stripe"), None, None, None, None, None).unwrap_err();
        assert!(err.contains("stripe"), "{err}");
        assert!(err.contains("OXSUM_STRIPE"), "{err}");

        // A rail that is not a rail is a typo, not a config.
        assert!(
            super::rails_intent_of(Some("paypal"), None, None, None, None, None)
                .unwrap_err()
                .contains("not a deposit rail")
        );

        // A configured rail the list omits is the other half of the same
        // contradiction — covered at the binary level by the --check-config
        // test in tests/install_ops.rs (the rails need real structs here).
    }

    #[test]
    fn self_registration_intent_requires_the_mailer() {
        let err = super::self_register_intent_of(Some("true"), None).unwrap_err();
        assert!(err.contains("OXSUM_CUSTOMER_SELF_REGISTER"), "{err}");
        assert!(err.contains("OXSUM_SMTP_URL"), "{err}");
        assert!(super::self_register_intent_of(Some("false"), None).is_ok());
        // `1` is `true` — it fails on the mailer, not on the parse.
        let err = super::self_register_intent_of(Some("1"), None).unwrap_err();
        assert!(err.contains("needs the mailer"), "{err}");
        // A non-boolean value is the parse failure instead.
        let err = super::self_register_intent_of(Some("yes"), None).unwrap_err();
        assert!(err.contains("must be true or false"), "{err}");
        assert!(super::self_register_intent_of(None, None).is_ok());
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

    /// The Squads v4 multisig/vault pair `getVaultPda` pins (issue #1143):
    /// multisig `C46NEXuBLRJjJhfAsKBUxUYdxSBQUPc3SXQFRBTXfupC` derives vault
    /// index 0 = `65kaU3KWZJCEz11vrCQfUZMc5AWSTzkFybwKD5BckLH8`.
    const TEST_MULTISIG: &str = "C46NEXuBLRJjJhfAsKBUxUYdxSBQUPc3SXQFRBTXfupC";
    const TEST_VAULT_0: &str = "65kaU3KWZJCEz11vrCQfUZMc5AWSTzkFybwKD5BckLH8";

    #[test]
    fn solana_custody_accepts_a_matching_vault() {
        assert_eq!(
            super::solana_custody_of(Some(TEST_MULTISIG), TEST_VAULT_0).unwrap(),
            super::SolanaCustody::VerifiedMultisigVault
        );
    }

    #[test]
    fn solana_custody_refuses_a_mismatched_pay_to() {
        // A payTo that is not the derived vault is a startup error naming
        // both halves — a pasted hot-wallet can never masquerade as the vault.
        let err = super::solana_custody_of(Some(TEST_MULTISIG), TEST_MULTISIG).unwrap_err();
        assert!(err.contains("OXSUM_SOLANA_PAY_TO"), "{err}");
        assert!(err.contains(TEST_VAULT_0), "{err}");
    }

    #[test]
    fn solana_custody_marks_an_unset_multisig_as_unverified() {
        // Unset multisig is not an error — a devnet hot wallet is allowed —
        // but the answer records the unverified branch so it is never silent.
        assert_eq!(
            super::solana_custody_of(None, TEST_VAULT_0).unwrap(),
            super::SolanaCustody::UnverifiedHotWallet
        );
    }

    #[test]
    fn solana_custody_refuses_a_malformed_multisig() {
        let err = super::solana_custody_of(Some("not-base58!!!"), TEST_VAULT_0).unwrap_err();
        assert!(err.contains("OXSUM_SOLANA_MULTISIG_ACCOUNT"), "{err}");
    }

    #[test]
    fn from_str_and_parse_agree() {
        for raw in ["invite", "open", " OPEN "] {
            assert_eq!(
                Signup::from_str(raw).ok(),
                Signup::parse(raw).ok(),
                "{raw:?}"
            );
        }
        assert!(Signup::from_str("other").is_err());
        assert!(Signup::parse("other").is_err());
        for raw in ["memory", "postgres", " Postgres "] {
            assert_eq!(
                RateLimitBackend::from_str(raw).ok(),
                RateLimitBackend::parse(raw).ok(),
                "{raw:?}"
            );
        }
        assert!(RateLimitBackend::from_str("other").is_err());
        assert!(RateLimitBackend::parse("other").is_err());
    }
}
