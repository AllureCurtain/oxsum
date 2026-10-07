//! Channels and their prices: what a gateway request relays to, and what it is charged by.
//!
//! A channel is an OpenAI-compatible upstream plus the models it serves. Every model carries a price
//! per million tokens and an output ceiling, and every price change appends a version instead of
//! overwriting one — the table is append-only, and a trigger enforces it. A request resolves the
//! version in force *when it starts* and carries it to the settlement, so a price change lands on
//! later requests only: a turn already in flight and a bill already written keep the version they
//! were priced by (docs/product.md, "Channels and prices").
//!
//! The upstream credential is stored sealed with AES-256-GCM under `OXSUM_SECRET_KEY`
//! ([`SecretKey`]): the database holds a nonce and a ciphertext, never the key, and a dump of
//! `oxsum.channels` cannot call upstream.

use std::collections::BTreeMap;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::RngExt as _;
use serde::Serialize;
use sqlx::Row;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::billing::{Price, PriceBook};
use crate::db::Db;
use crate::error::WalletError;

/// The longest a channel name may be; it appears in the admin URL path.
const MAX_NAME: usize = 40;
/// The longest a model name may be.
const MAX_MODEL: usize = 200;
/// AES-256-GCM: a 32-byte key, a 12-byte nonce, and a 16-byte tag inside the ciphertext.
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
/// How much of an upstream key stays readable, so a list can recognize one. product.md,
/// "Channels and prices".
const LAST4: usize = 4;

/// Serialises the one-time seeding of a channel across processes, so two servers starting against an
/// empty database do not both insert the bootstrap channel.
const SEED_LOCK: i64 = i64::from_be_bytes(*b"oxsumchn");

/// The key that seals upstream credentials at rest.
///
/// It is deployment configuration, not data: the database holds what this key sealed, and a
/// deployment that loses it cannot open its own channels. Losing it is therefore a hard failure at
/// startup rather than a request-time surprise (`Db::check_sealed_keys`).
#[derive(Clone)]
pub struct SecretKey([u8; KEY_BYTES]);

impl SecretKey {
    /// Reads `OXSUM_SECRET_KEY`: 32 bytes, base64.
    ///
    /// # Errors
    ///
    /// Names the variable and what is wrong with it, so a deployment that mistyped it can fix it
    /// without reading the source.
    pub fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        let bytes = BASE64
            .decode(trimmed)
            .map_err(|error| format!("OXSUM_SECRET_KEY is not base64: {error}"))?;
        let key: [u8; KEY_BYTES] = bytes.try_into().map_err(|bytes: Vec<u8>| {
            format!(
                "OXSUM_SECRET_KEY must decode to {KEY_BYTES} bytes, got {}",
                bytes.len()
            )
        })?;
        Ok(Self(key))
    }

    /// A key from raw bytes: what a test builds, and what a deployment that would rather generate
    /// its own can pass.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// Seals a secret into the stored form: base64 of `nonce || ciphertext`.
    ///
    /// A fresh random nonce per value, so sealing the same credential twice produces two different
    /// records — the nonce is not a secret, but reusing one under one key is how AES-GCM breaks.
    /// `pub` because webhook signing secrets get the same protection.
    pub fn seal(&self, plaintext: &str) -> Result<String, WalletError> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let mut nonce_bytes = [0u8; NONCE_BYTES];
        rand::rng().fill(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let sealed = cipher.encrypt(nonce, plaintext.as_bytes()).map_err(|_| {
            WalletError::Misconfigured("sealing an upstream credential failed".into())
        })?;
        let mut record = nonce_bytes.to_vec();
        record.extend_from_slice(&sealed);
        Ok(BASE64.encode(record))
    }

    /// Opens what [`seal`](Self::seal) wrote.
    ///
    /// AES-GCM authenticates as well as it encrypts, so a record sealed under another key, or
    /// changed since it was written, fails here rather than turning into a plausible wrong
    /// credential.
    pub fn open(&self, sealed: &str) -> Result<String, WalletError> {
        let record = BASE64.decode(sealed.trim()).map_err(|error| {
            WalletError::Misconfigured(format!(
                "a stored upstream credential is not base64: {error}"
            ))
        })?;
        if record.len() <= NONCE_BYTES {
            return Err(WalletError::Misconfigured(
                "a stored upstream credential is too short to hold a nonce".into(),
            ));
        }
        let (nonce_bytes, ciphertext) = record.split_at(NONCE_BYTES);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let opened = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| {
                WalletError::Misconfigured(
                    "a stored upstream credential does not open with OXSUM_SECRET_KEY".into(),
                )
            })?;
        String::from_utf8(opened).map_err(|_| {
            WalletError::Misconfigured("a stored upstream credential is not text".into())
        })
    }
}

impl std::fmt::Debug for SecretKey {
    /// Never the key itself, only that there is one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(…)")
    }
}

/// A channel as the admin API presents it: the connection, no credential, and what it serves now.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub name: String,
    pub base_url: String,
    /// The upstream protocol the channel speaks: the name the usage-adapter registry
    /// resolves (issue #104).
    pub protocol: String,
    /// The last four characters of the upstream key, and nothing more of it.
    pub api_key_last4: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// One entry per model, at its current version.
    pub models: Vec<ModelPrice>,
}

/// One model's price, and the version it came from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    pub model: String,
    pub version: i64,
    /// The whole price this version carries, flattened into the record: the
    /// base set plus the mode and any conditional rules.
    #[serde(flatten)]
    pub price: Price,
    /// When this version was written, which is the closest thing a price has to a creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl ModelPrice {
    /// The price this record carries.
    #[must_use]
    pub fn price(&self) -> Price {
        self.price.clone()
    }
}

/// What one request relays to and is charged by, resolved once when the request starts.
#[derive(Debug, Clone)]
pub struct Serving {
    pub channel: String,
    pub base_url: String,
    /// The upstream protocol, resolved into a usage adapter when the turn starts.
    pub protocol: String,
    /// The upstream credential, opened. Held for this request only.
    pub api_key: String,
    /// The price version this request is billed at, which the settlement records.
    pub version: i64,
    pub price: Price,
}

impl Db {
    /// Creates a channel, or replaces the connection of one that already exists.
    ///
    /// The prices are not touched: a connection moves, a price history stays where it is.
    ///
    /// # Errors
    ///
    /// Refuses a name, an address or a credential that could not be used, and a name another
    /// channel already has with a different address only in the sense that this *is* that channel.
    pub async fn set_channel(
        &self,
        name: &str,
        base_url: &str,
        api_key: &str,
        protocol: &str,
        key: &SecretKey,
    ) -> Result<(), WalletError> {
        let name = channel_name(name)?;
        let base_url = base_url_of(base_url)?;
        let api_key = api_key_of(api_key)?;
        crate::adapters::known_protocol(protocol)?;
        let sealed = key.seal(api_key)?;
        let last4 = last_chars(api_key, LAST4);
        sqlx::query(
            "INSERT INTO oxsum.channels \
             (channel_id, name, base_url, api_key_sealed, api_key_last4, protocol) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (name) DO UPDATE SET \
                 base_url = EXCLUDED.base_url, \
                 api_key_sealed = EXCLUDED.api_key_sealed, \
                 api_key_last4 = EXCLUDED.api_key_last4, \
                 protocol = EXCLUDED.protocol, \
                 updated_at = now()",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(base_url)
        .bind(sealed)
        .bind(last4)
        .bind(protocol)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Appends a price version for one of a channel's models, and returns the version it wrote.
    ///
    /// Versions are allocated per `(channel, model)` under the channel's own row lock, so two
    /// concurrent price changes produce two versions rather than one collision.
    ///
    /// # Errors
    ///
    /// Refuses an unknown channel, an unusable model name or price, and a model another channel
    /// already serves — in v1 one model belongs to exactly one channel (docs/product.md), so the
    /// gateway never has to choose between two upstreams for one request.
    pub async fn append_price(
        &self,
        channel: &str,
        model: &str,
        price: Price,
    ) -> Result<i64, WalletError> {
        let model = model_name(model)?;
        price.validate().map_err(WalletError::InvalidInput)?;
        let mut tx = self.pool().begin().await?;
        let channel_id: Uuid =
            sqlx::query_scalar("SELECT channel_id FROM oxsum.channels WHERE name = $1 FOR UPDATE")
                .bind(channel)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| {
                    WalletError::InvalidInput(format!("no channel named {channel:?}"))
                })?;
        let elsewhere: Option<String> = sqlx::query_scalar(
            "SELECT c.name FROM oxsum.channel_prices p JOIN oxsum.channels c USING (channel_id) \
             WHERE p.model = $1 AND p.channel_id <> $2 LIMIT 1",
        )
        .bind(model)
        .bind(channel_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(other) = elsewhere {
            return Err(WalletError::Conflict(format!(
                "{model:?} is already served by channel {other:?}: one model belongs to one channel"
            )));
        }
        let version: i32 = sqlx::query_scalar(
            "INSERT INTO oxsum.channel_prices \
                 (channel_id, model, version, input_price_per_million, output_price_per_million, \
                  max_output_tokens, cache_read_price_per_million, \
                  cache_write_5m_price_per_million, cache_write_1h_price_per_million, \
                  reasoning_price_per_million, cost_per_request, mode, upstream_prices, rules) \
             SELECT $1, $2, COALESCE(max(version), 0) + 1, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
                    $12, $13 \
             FROM oxsum.channel_prices WHERE channel_id = $1 AND model = $2 \
             RETURNING version",
        )
        .bind(channel_id)
        .bind(model)
        .bind(price.input_price_per_million)
        .bind(price.output_price_per_million)
        .bind(price.max_output_tokens)
        .bind(price.cache_read_price_per_million)
        .bind(price.cache_write_5m_price_per_million)
        .bind(price.cache_write_1h_price_per_million)
        .bind(price.reasoning_price_per_million)
        .bind(price.cost_per_request)
        .bind(price.mode.as_str())
        .bind(
            price
                .upstream
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| WalletError::InvalidInput(format!("upstream prices: {error}")))?,
        )
        .bind(if price.rules.is_empty() {
            None
        } else {
            Some(
                serde_json::to_value(&price.rules)
                    .map_err(|error| WalletError::InvalidInput(format!("price rules: {error}")))?,
            )
        })
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(i64::from(version))
    }

    /// Every channel with the current version of each of its models, oldest first.
    pub async fn channels(&self) -> Result<Vec<Channel>, WalletError> {
        let rows = sqlx::query(
            "SELECT channel_id, name, base_url, api_key_last4, created_at, protocol \
             FROM oxsum.channels ORDER BY created_at, name",
        )
        .fetch_all(self.pool())
        .await?;
        let mut channels = Vec::with_capacity(rows.len());
        for row in &rows {
            let channel_id: Uuid = row.try_get("channel_id")?;
            channels.push(Channel {
                name: row.try_get("name")?,
                base_url: row.try_get("base_url")?,
                protocol: row.try_get("protocol")?,
                api_key_last4: row.try_get("api_key_last4")?,
                created_at: row.try_get("created_at")?,
                models: current_prices(self, channel_id).await?,
            });
        }
        Ok(channels)
    }

    /// Every version of every model of one channel, newest first. `None`: no such channel.
    ///
    /// This is what makes the previous version readable rather than merely retained.
    pub async fn channel_prices(
        &self,
        channel: &str,
    ) -> Result<Option<Vec<ModelPrice>>, WalletError> {
        let Some(channel_id): Option<Uuid> =
            sqlx::query_scalar("SELECT channel_id FROM oxsum.channels WHERE name = $1")
                .bind(channel)
                .fetch_optional(self.pool())
                .await?
        else {
            return Ok(None);
        };
        Ok(Some(prices_of(self, channel_id, None).await?))
    }

    /// What a request for this model relays to and is charged by, or `None` when nothing serves it.
    ///
    /// The newest version of the model wins, and the credential is opened here: the caller holds the
    /// plaintext for the length of one request and the database never holds it at all.
    ///
    /// # Errors
    ///
    /// [`WalletError::Misconfigured`] when the stored credential does not open under `key`, which is
    /// a deployment that lost the key it wrote with rather than a caller's mistake.
    pub async fn serving(
        &self,
        model: &str,
        key: &SecretKey,
    ) -> Result<Option<Serving>, WalletError> {
        let row = sqlx::query(
            "SELECT c.name, c.base_url, c.api_key_sealed, c.protocol, p.version, \
                    p.input_price_per_million, p.output_price_per_million, p.max_output_tokens, \
                    p.cache_read_price_per_million, p.cache_write_5m_price_per_million, \
                    p.cache_write_1h_price_per_million, p.reasoning_price_per_million, \
                    p.cost_per_request, p.mode, p.upstream_prices, p.rules \
             FROM oxsum.channel_prices p JOIN oxsum.channels c USING (channel_id) \
             WHERE p.model = $1 \
             ORDER BY p.version DESC, c.name \
             LIMIT 1",
        )
        .bind(model)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let sealed: String = row.try_get("api_key_sealed")?;
        Ok(Some(Serving {
            channel: row.try_get("name")?,
            base_url: row.try_get("base_url")?,
            protocol: row.try_get("protocol")?,
            api_key: key.open(&sealed)?,
            version: i64::from(row.try_get::<i32, _>("version")?),
            price: price_from_row(&row)?,
        }))
    }

    /// The models with a current price, for `/v1/models`.
    pub async fn served_models(&self) -> Result<Vec<String>, WalletError> {
        let rows =
            sqlx::query_scalar("SELECT DISTINCT model FROM oxsum.channel_prices ORDER BY model")
                .fetch_all(self.pool())
                .await?;
        Ok(rows)
    }

    /// Whether any channel is configured.
    pub async fn has_channels(&self) -> Result<bool, WalletError> {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oxsum.channels)")
            .fetch_one(self.pool())
            .await?;
        Ok(exists)
    }

    /// Opens every stored credential once, so a deployment whose key does not match its channels
    /// says so at startup instead of answering a request with a 500.
    ///
    /// # Errors
    ///
    /// [`WalletError::Misconfigured`], naming the channel whose credential would not open.
    pub async fn check_sealed_keys(&self, key: &SecretKey) -> Result<(), WalletError> {
        let rows = sqlx::query("SELECT name, api_key_sealed FROM oxsum.channels")
            .fetch_all(self.pool())
            .await?;
        for row in &rows {
            let name: String = row.try_get("name")?;
            let sealed: String = row.try_get("api_key_sealed")?;
            key.open(&sealed).map_err(|error| {
                WalletError::Misconfigured(format!("channel {name:?}: {error}"))
            })?;
        }
        Ok(())
    }

    /// Seeds one channel and its prices from a [`PriceBook`], once, if the database has no channels.
    ///
    /// Returns whether it seeded anything. This is the deployment's *first* channel: the environment
    /// describes it so that a fresh database has something to serve, and a database that already has
    /// channels is left alone — configuration outlives the environment it started in.
    ///
    /// # Errors
    ///
    /// As [`Db::set_channel`] and [`Db::append_price`].
    pub async fn seed_channel(
        &self,
        book: &PriceBook,
        base_url: &str,
        api_key: &str,
        key: &SecretKey,
    ) -> Result<bool, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SEED_LOCK)
            .execute(&mut *tx)
            .await?;
        let existing: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oxsum.channels)")
            .fetch_one(&mut *tx)
            .await?;
        if existing {
            tx.rollback().await?;
            return Ok(false);
        }
        let channel_id = Uuid::new_v4();
        let name = channel_name(book.channel())?;
        let base_url = base_url_of(base_url)?;
        let api_key = api_key_of(api_key)?;
        sqlx::query(
            "INSERT INTO oxsum.channels \
             (channel_id, name, base_url, api_key_sealed, api_key_last4, protocol) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(channel_id)
        .bind(name)
        .bind(base_url)
        .bind(key.seal(api_key)?)
        .bind(last_chars(api_key, LAST4))
        .bind(crate::adapters::OPENAI)
        .execute(&mut *tx)
        .await?;
        let models: BTreeMap<&str, &Price> = book.models().collect();
        for (index, (model, price)) in models.iter().enumerate() {
            price.validate().map_err(WalletError::InvalidInput)?;
            sqlx::query(
                "INSERT INTO oxsum.channel_prices \
                     (channel_id, model, version, input_price_per_million, output_price_per_million, \
                      max_output_tokens, cache_read_price_per_million, \
                      cache_write_5m_price_per_million, cache_write_1h_price_per_million, \
                      reasoning_price_per_million, cost_per_request, mode, upstream_prices, rules) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
            )
            .bind(channel_id)
            .bind(model_name(model)?)
            .bind(i32::try_from(index).unwrap_or(i32::MAX) + 1)
            .bind(price.input_price_per_million)
            .bind(price.output_price_per_million)
            .bind(price.max_output_tokens)
            .bind(price.cache_read_price_per_million)
            .bind(price.cache_write_5m_price_per_million)
            .bind(price.cache_write_1h_price_per_million)
            .bind(price.reasoning_price_per_million)
            .bind(price.cost_per_request)
            .bind(price.mode.as_str())
            .bind(
                price
                    .upstream
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|error| {
                        WalletError::InvalidInput(format!("upstream prices: {error}"))
                    })?,
            )
            .bind(if price.rules.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&price.rules).map_err(|error| {
                    WalletError::InvalidInput(format!("price rules: {error}"))
                })?)
            })
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}

/// The current version of every model of one channel.
async fn current_prices(db: &Db, channel_id: Uuid) -> Result<Vec<ModelPrice>, WalletError> {
    prices_of(db, channel_id, Some(1)).await
}

/// One version per model (`latest`), or all of them.
async fn prices_of(
    db: &Db,
    channel_id: Uuid,
    latest: Option<i32>,
) -> Result<Vec<ModelPrice>, WalletError> {
    let rows = sqlx::query(
        "SELECT model, version, input_price_per_million, output_price_per_million, \
                max_output_tokens, cache_read_price_per_million, \
                cache_write_5m_price_per_million, cache_write_1h_price_per_million, \
                reasoning_price_per_million, cost_per_request, mode, upstream_prices, rules, \
                created_at \
         FROM (SELECT p.*, row_number() OVER (PARTITION BY model ORDER BY version DESC) AS rank \
               FROM oxsum.channel_prices p WHERE p.channel_id = $1) ranked \
         WHERE $2::int IS NULL OR rank <= $2 \
         ORDER BY model, version DESC",
    )
    .bind(channel_id)
    .bind(latest)
    .fetch_all(db.pool())
    .await?;
    rows.iter()
        .map(|row| {
            Ok(ModelPrice {
                model: row.try_get("model")?,
                version: i64::from(row.try_get::<i32, _>("version")?),
                price: price_from_row(row)?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

/// The price a `channel_prices` row carries, columns and JSONB slots together.
///
/// A stored `mode` this build cannot name, or rules that do not deserialize,
/// are configuration the gateway cannot price — [`WalletError::Misconfigured`]
/// rather than a bill computed on a guess.
fn price_from_row(row: &sqlx::postgres::PgRow) -> Result<Price, WalletError> {
    let mode: String = row.try_get("mode")?;
    let rules: Option<serde_json::Value> = row.try_get("rules")?;
    let upstream: Option<serde_json::Value> = row.try_get("upstream_prices")?;
    let price = Price {
        input_price_per_million: row.try_get("input_price_per_million")?,
        output_price_per_million: row.try_get("output_price_per_million")?,
        max_output_tokens: row.try_get("max_output_tokens")?,
        cache_read_price_per_million: row.try_get("cache_read_price_per_million")?,
        cache_write_5m_price_per_million: row.try_get("cache_write_5m_price_per_million")?,
        cache_write_1h_price_per_million: row.try_get("cache_write_1h_price_per_million")?,
        reasoning_price_per_million: row.try_get("reasoning_price_per_million")?,
        cost_per_request: row.try_get("cost_per_request")?,
        upstream: upstream
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                WalletError::Misconfigured(format!("stored upstream prices: {error}"))
            })?,
        mode: crate::billing::BillingMode::parse(&mode).ok_or_else(|| {
            WalletError::Misconfigured(format!("a stored price has unknown mode {mode:?}"))
        })?,
        rules: rules
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| WalletError::Misconfigured(format!("stored price rules: {error}")))?
            .unwrap_or_default(),
    };
    Ok(price)
}

/// A channel name: what the admin URL path can carry.
fn channel_name(name: &str) -> Result<&str, WalletError> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(WalletError::InvalidInput(format!(
            "name must be 1 to {MAX_NAME} characters"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(WalletError::InvalidInput(
            "name may use letters, digits, dashes, underscores and dots".into(),
        ));
    }
    Ok(name)
}

/// A model name, which upstream has to accept back.
fn model_name(model: &str) -> Result<&str, WalletError> {
    let model = model.trim();
    if model.is_empty() || model.len() > MAX_MODEL {
        return Err(WalletError::InvalidInput(format!(
            "model must be 1 to {MAX_MODEL} characters"
        )));
    }
    if model.chars().any(|c| c.is_control()) {
        return Err(WalletError::InvalidInput(
            "model may not contain control characters".into(),
        ));
    }
    Ok(model)
}

/// The channel's address, without a trailing slash: the gateway appends `/chat/completions`.
fn base_url_of(base_url: &str) -> Result<String, WalletError> {
    let url = Url::parse(base_url.trim())
        .map_err(|error| WalletError::InvalidInput(format!("baseUrl is not a URL: {error}")))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(WalletError::InvalidInput(format!(
            "baseUrl must be http or https, got {:?}",
            url.scheme()
        )));
    }
    if url.host_str().is_none() {
        return Err(WalletError::InvalidInput("baseUrl must name a host".into()));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// The upstream credential: present, and worth sealing.
fn api_key_of(api_key: &str) -> Result<&str, WalletError> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(WalletError::InvalidInput("apiKey must not be empty".into()));
    }
    Ok(api_key)
}

/// The last `count` characters of a secret, or all of it when it is shorter.
fn last_chars(secret: &str, count: usize) -> String {
    let chars: Vec<char> = secret.chars().collect();
    let start = chars.len().saturating_sub(count);
    chars[start..].iter().collect()
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{SecretKey, last_chars};

    fn key(byte: u8) -> SecretKey {
        SecretKey::from_bytes([byte; 32])
    }

    #[test]
    fn a_secret_round_trips_and_a_wrong_key_does_not() {
        let sealed = key(7).seal("sk-upstream-secret").unwrap();
        assert_eq!(key(7).open(&sealed).unwrap(), "sk-upstream-secret");
        // Another key, and a record changed after it was written: AES-GCM authenticates, so both
        // are refusals rather than plausible wrong credentials.
        assert!(key(8).open(&sealed).is_err());
        let mut tampered = sealed.clone();
        tampered.pop();
        tampered.push(if sealed.ends_with('A') { 'B' } else { 'A' });
        assert!(key(7).open(&tampered).is_err());
    }

    #[test]
    fn sealing_twice_produces_two_records() {
        let first = key(7).seal("sk-upstream-secret").unwrap();
        let second = key(7).seal("sk-upstream-secret").unwrap();
        assert_ne!(first, second, "a fresh nonce per value");
    }

    #[test]
    fn a_key_parses_from_base64_and_nothing_else() {
        let text = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [3u8; 32]);
        assert_eq!(
            SecretKey::parse(&text)
                .unwrap()
                .open(&key(3).seal("x").unwrap())
                .unwrap(),
            "x"
        );
        assert!(SecretKey::parse("not base64!").is_err());
        assert!(SecretKey::parse("c2hvcnQ=").is_err());
    }

    #[test]
    fn only_the_tail_of_a_secret_is_kept_readable() {
        assert_eq!(last_chars("sk-12345678", 4), "5678");
        assert_eq!(last_chars("ab", 4), "ab");
        assert_eq!(last_chars("", 4), "");
    }

    #[test]
    fn the_debug_form_never_carries_the_key() {
        assert_eq!(format!("{:?}", key(9)), "SecretKey(…)");
    }
}
