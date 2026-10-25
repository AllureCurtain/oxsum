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
    mailer: Option<crate::mail::Mailer>,
    github: Option<crate::oauth::GitHub>,
    turnstile: Option<crate::antibot::Turnstile>,
    stripe: Option<crate::stripe::Stripe>,
    /// The Airwallex deposit rail (issue #285): the credential pair plus
    /// webhook secret when the `OXSUM_AIRWALLEX_*` set is complete, `None` —
    /// the default — when the deployment takes no Airwallex payments.
    airwallex: Option<crate::airwallex::Airwallex>,
    /// The Hyperswitch deposit rail (issue #319): the API key, the business
    /// profile's webhook hash key and the browser publishable key when the
    /// `OXSUM_HYPERSWITCH_*` set is complete, `None` — the default — when the
    /// deployment takes no switch payments.
    hyperswitch: Option<crate::hyperswitch::Hyperswitch>,
    /// The deployment's public origin, `OXSUM_PUBLIC_URL` — required by the
    /// mailer and the OAuth client, optional on its own: the device grant
    /// builds `verificationUri` from it when set (issue #156).
    public_url: Option<String>,
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
    x402: Option<std::sync::Arc<dyn oxsum_core::RailVerifier>>,
    /// The provider the `upstream-bill-sync` job asks for each channel's daily
    /// billed cost (issue #283): the OpenAI Costs impl when
    /// `OXSUM_UPSTREAM_BILL_SYNC` is enabled (and `OXSUM_SECRET_KEY` present to
    /// open `bill_key_sealed`), a test's stub otherwise. `None` — the default —
    /// schedules no `upstream-bill-sync` pass.
    bill_provider: Option<std::sync::Arc<dyn oxsum_core::UpstreamBillProvider>>,
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
            signup_bonus_minor: 0,
            mailer: None,
            github: None,
            turnstile: None,
            stripe: None,
            airwallex: None,
            hyperswitch: None,
            public_url: None,
            retention: oxsum_core::Retention::default(),
            upstream_price_sync: false,
            otlp_endpoint: None,
            witness_urls: Vec::new(),
            witness_keys: Vec::new(),
            witness_serve: None,
            display_currency: None,
            x402: None,
            bill_provider: None,
        }
    }

    /// Sets the anti-bot check. Tests use this; the server reads
    /// `OXSUM_TURNSTILE_SITE_KEY` and `OXSUM_TURNSTILE_SECRET_KEY` together —
    /// a lone key is a startup error, because a half-configured check only
    /// fails inside a registration.
    #[must_use]
    pub fn with_turnstile(mut self, turnstile: crate::antibot::Turnstile) -> Self {
        self.turnstile = Some(turnstile);
        self
    }

    /// Sets the GitHub OAuth client. Tests use this; the server reads
    /// `OXSUM_GITHUB_CLIENT_ID`, `OXSUM_GITHUB_CLIENT_SECRET` and
    /// `OXSUM_PUBLIC_URL` together — a subset is a startup error, because a
    /// half-configured provider only fails inside a callback.
    #[must_use]
    pub fn with_github(mut self, github: crate::oauth::GitHub) -> Self {
        self.github = Some(github);
        self
    }

    /// Sets the mailer. Tests use this; the server reads `OXSUM_SMTP_URL`,
    /// `OXSUM_MAIL_FROM` and `OXSUM_PUBLIC_URL` together — any subset is a
    /// startup error, because a half-configured mailer only fails at send time.
    #[must_use]
    pub fn with_mailer(mut self, mailer: crate::mail::Mailer) -> Self {
        self.mailer = Some(mailer);
        self
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
        let github = github_of()?;
        let turnstile = turnstile_of()?;
        let stripe = stripe_of()?;
        let airwallex = airwallex_of()?;
        let hyperswitch = hyperswitch_of()?;
        // Optional on its own: the mailer and the OAuth client each require it,
        // and the device grant only prefers it for a stable `verificationUri`.
        let public_url = var("OXSUM_PUBLIC_URL");
        let retention = oxsum_core::Retention {
            provider_raw_days: retention_days("OXSUM_RETENTION_PROVIDER_RAW_DAYS")?.unwrap_or(7),
            deliveries_days: retention_days("OXSUM_RETENTION_DELIVERIES_DAYS")?.unwrap_or(30),
            jobs_days: retention_days("OXSUM_RETENTION_JOBS_DAYS")?.unwrap_or(90),
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
            turnstile,
            stripe,
            airwallex,
            hyperswitch,
            public_url,
            retention,
            upstream_price_sync,
            otlp_endpoint,
            witness_urls,
            witness_keys,
            witness_serve,
            display_currency,
            x402: x402_of()?,
            bill_provider,
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
    pub fn secret(&self) -> Option<&SecretKey> {
        self.secret.as_ref()
    }

    /// The mailer, or `None` when the deployment sends no email: every email
    /// surface then behaves as documented — registration sends nothing, forgot
    /// is indistinguishable, and `verify/request` alone reports the absence.
    #[must_use]
    pub fn mailer(&self) -> Option<&crate::mail::Mailer> {
        self.mailer.as_ref()
    }

    /// The GitHub OAuth client, or `None` when the provider pair is not
    /// configured — in which case the start endpoint answers 404 and the
    /// callback redirects home, and the login page draws no GitHub button.
    #[must_use]
    pub fn github(&self) -> Option<&crate::oauth::GitHub> {
        self.github.as_ref()
    }

    /// The anti-bot check, or `None` when the pair is not configured — the
    /// two registration surfaces then run unchecked, exactly as before the
    /// check existed.
    #[must_use]
    pub fn turnstile(&self) -> Option<&crate::antibot::Turnstile> {
        self.turnstile.as_ref()
    }

    /// The Stripe rail, or `None` when `OXSUM_STRIPE_SECRET_KEY` and
    /// `OXSUM_STRIPE_WEBHOOK_SECRET` are not both set — in which case
    /// `POST /api/v1/topups/stripe` answers 503 and `POST
    /// /api/v1/webhooks/stripe` answers 404.
    #[must_use]
    pub fn stripe(&self) -> Option<&crate::stripe::Stripe> {
        self.stripe.as_ref()
    }

    /// Sets the Stripe rail. Tests use this; the server reads
    /// `OXSUM_STRIPE_SECRET_KEY`, `OXSUM_STRIPE_WEBHOOK_SECRET` and
    /// `OXSUM_PUBLIC_URL` together — a subset is a startup error.
    #[must_use]
    pub fn with_stripe(mut self, stripe: crate::stripe::Stripe) -> Self {
        self.stripe = Some(stripe);
        self
    }

    /// The Airwallex rail, or `None` when `OXSUM_AIRWALLEX_CLIENT_ID`,
    /// `OXSUM_AIRWALLEX_API_KEY`, `OXSUM_AIRWALLEX_WEBHOOK_SECRET` and
    /// `OXSUM_PUBLIC_URL` are not all set — in which case
    /// `POST /api/v1/topups/airwallex` answers 503 and
    /// `POST /api/v1/webhooks/airwallex` answers 404.
    #[must_use]
    pub fn airwallex(&self) -> Option<&crate::airwallex::Airwallex> {
        self.airwallex.as_ref()
    }

    /// Sets the Airwallex rail. Tests use this; the server reads the
    /// `OXSUM_AIRWALLEX_*` set plus `OXSUM_PUBLIC_URL` together — a subset is
    /// a startup error.
    #[must_use]
    pub fn with_airwallex(mut self, airwallex: crate::airwallex::Airwallex) -> Self {
        self.airwallex = Some(airwallex);
        self
    }

    /// The Hyperswitch rail, or `None` when `OXSUM_HYPERSWITCH_API_KEY`,
    /// `OXSUM_HYPERSWITCH_WEBHOOK_SECRET`, `OXSUM_HYPERSWITCH_PUBLISHABLE_KEY`
    /// and `OXSUM_PUBLIC_URL` are not all set — in which case
    /// `POST /api/v1/topups/hyperswitch` answers 503 and
    /// `POST /api/v1/webhooks/hyperswitch` answers 404.
    #[must_use]
    pub fn hyperswitch(&self) -> Option<&crate::hyperswitch::Hyperswitch> {
        self.hyperswitch.as_ref()
    }

    /// Sets the Hyperswitch rail. Tests use this; the server reads the
    /// `OXSUM_HYPERSWITCH_*` set plus `OXSUM_PUBLIC_URL` together — a subset is
    /// a startup error.
    #[must_use]
    pub fn with_hyperswitch(mut self, hyperswitch: crate::hyperswitch::Hyperswitch) -> Self {
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
        self.x402 = Some(std::sync::Arc::new(verifier));
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
    /// `OXSUM_RETENTION_PROVIDER_RAW_DAYS`, `OXSUM_RETENTION_DELIVERIES_DAYS`
    /// and `OXSUM_RETENTION_JOBS_DAYS`.
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
    pub fn bill_provider(&self) -> Option<&std::sync::Arc<dyn oxsum_core::UpstreamBillProvider>> {
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
    /// Returns the owned [`Display`]; the conversion is a cheap struct, so a
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
        self.bill_provider = Some(std::sync::Arc::new(provider));
        self
    }

    /// `OXSUM_OTLP_ENDPOINT` when set — the collector base URL trace export
    /// points at (issue #224). `None` keeps the subscriber fmt-only.
    #[must_use]
    pub fn otlp_endpoint(&self) -> Option<&str> {
        self.otlp_endpoint.as_deref()
    }

    /// Sets the public origin. Tests use this; the server reads
    /// `OXSUM_PUBLIC_URL`.
    #[must_use]
    pub fn with_public_url(mut self, url: impl Into<String>) -> Self {
        self.public_url = Some(url.into());
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
) -> Result<Option<std::sync::Arc<dyn oxsum_core::UpstreamBillProvider>>, String> {
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
    Ok(Some(std::sync::Arc::new(
        oxsum_integrations::bill_providers::CompositeBillProvider::new(vec![
            std::sync::Arc::new(oxsum_integrations::openai_costs::OpenAiBillProvider::new(
                reqwest::Client::new(),
            )),
            std::sync::Arc::new(
                oxsum_integrations::anthropic_costs::AnthropicBillProvider::new(
                    reqwest::Client::new(),
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
fn mailer_of() -> Result<Option<crate::mail::Mailer>, String> {
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
    crate::mail::Mailer::new(&smtp_url, &from, &public_url).map(Some)
}

/// Parses the GitHub OAuth configuration: `OXSUM_GITHUB_CLIENT_ID`,
/// `OXSUM_GITHUB_CLIENT_SECRET` and `OXSUM_PUBLIC_URL` — the client pair set
/// together, and the public origin the callback URL is built from required
/// alongside, because a pair without it can only fail inside the callback.
/// `OXSUM_GITHUB_WEB_URL` and `OXSUM_GITHUB_API_URL` override the provider's
/// base URLs; nothing but a test points them anywhere else.
fn github_of() -> Result<Option<crate::oauth::GitHub>, String> {
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
    Ok(Some(crate::oauth::GitHub::new(
        client_id,
        client_secret,
        public_url,
        web_url,
        api_url,
    )))
}

/// Parses the Stripe pair: `OXSUM_STRIPE_SECRET_KEY` and
/// `OXSUM_STRIPE_WEBHOOK_SECRET` together or not at all — a lone key is a
/// startup error, because a half-configured rail only fails inside a request.
/// `OXSUM_PUBLIC_URL` is required alongside: checkout redirects are built on
/// it. `OXSUM_STRIPE_API_URL` overrides the API origin; nothing but a test
/// points it anywhere else.
fn stripe_of() -> Result<Option<crate::stripe::Stripe>, String> {
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
    Ok(Some(crate::stripe::Stripe::new(
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
fn airwallex_of() -> Result<Option<crate::airwallex::Airwallex>, String> {
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
    Ok(Some(crate::airwallex::Airwallex::new(
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
fn hyperswitch_of() -> Result<Option<crate::hyperswitch::Hyperswitch>, String> {
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
    Ok(Some(crate::hyperswitch::Hyperswitch::new(
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
fn x402_of() -> Result<Option<std::sync::Arc<dyn oxsum_core::RailVerifier>>, String> {
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
        return Ok(Some(std::sync::Arc::new(verifier)));
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
        .map(|v| Some(std::sync::Arc::new(v) as std::sync::Arc<dyn oxsum_core::RailVerifier>))
        .map_err(|error| error.to_string())
}

/// Parses the Turnstile pair: `OXSUM_TURNSTILE_SITE_KEY` and
/// `OXSUM_TURNSTILE_SECRET_KEY` together or not at all — a lone key is a
/// startup error, because a half-configured check only fails inside a
/// registration. `OXSUM_TURNSTILE_VERIFY_URL` overrides the siteverify
/// endpoint; nothing but a test points it anywhere else.
fn turnstile_of() -> Result<Option<crate::antibot::Turnstile>, String> {
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
    Ok(Some(crate::antibot::Turnstile::new(
        site_key, secret_key, verify_url,
    )))
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
