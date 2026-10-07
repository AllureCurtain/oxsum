//! Channels and their prices, against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of failing, so
//! a bare `cargo test` still passes.
//!
//! The point of these tests is the promise the prices carry: a change appends a version, the previous
//! version stays readable, and nothing — not the admin API, not a psql session — can rewrite one. The
//! HTTP surface over this store is tested in crates/server/tests/admin.rs.
//!
//! Every test names its channel *and* its model with a random suffix: the database is shared between
//! tests, and a model belongs to one channel, so a shared model name would make the tests mean
//! different things depending on which one ran first.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, Price, PriceBook, SecretKey, WalletError};
use sqlx::PgPool;
use std::collections::BTreeMap;

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! db_or_skip {
    () => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// A database handle with oxsum's own tables applied.
async fn db(url: &str) -> Db {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    db
}

/// Each test uses its own channel and model names, so tests never interfere and reruns never collide.
fn fresh(name: &str) -> String {
    format!("{name}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

fn key() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

fn price(input: i64, output: i64, max: i64) -> Price {
    Price {
        input_price_per_million: input,
        output_price_per_million: output,
        max_output_tokens: max,
        cache_read_price_per_million: None,
        cache_write_5m_price_per_million: None,
        cache_write_1h_price_per_million: None,
        reasoning_price_per_million: None,
        cost_per_request: None,
        upstream: None,
        mode: Default::default(),
        rules: Vec::new(),
    }
}

#[tokio::test]
async fn a_price_change_appends_a_version_and_leaves_the_old_one_readable() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let channel = fresh("priced");
    let model = fresh("m");

    db.set_channel(
        &channel,
        "https://upstream.example/v1",
        "sk-secret-1234",
        "openai",
        &key(),
    )
    .await
    .unwrap();
    let first = db
        .append_price(&channel, &model, price(10, 20, 100))
        .await
        .unwrap();
    let second = db
        .append_price(&channel, &model, price(30, 40, 200))
        .await
        .unwrap();
    assert_eq!((first, second), (1, 2), "versions count up from one");

    // What a request starts on is the newest version…
    let serving = db.serving(&model, &key()).await.unwrap().unwrap();
    assert_eq!(serving.channel, channel);
    assert_eq!(serving.protocol, "openai");
    assert_eq!(serving.version, second);
    assert_eq!(serving.price, price(30, 40, 200));
    assert_eq!(serving.api_key, "sk-secret-1234");
    assert_eq!(serving.base_url, "https://upstream.example/v1");

    // …and the first version is still there, which is what makes an old bill checkable.
    let versions = db.channel_prices(&channel).await.unwrap().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version, second);
    assert_eq!(versions[1].version, first);
    assert_eq!(versions[1].price(), price(10, 20, 100));

    // The admin list shows the current version only.
    let listed = db
        .channels()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.name == channel)
        .unwrap();
    assert_eq!(listed.models.len(), 1);
    assert_eq!(listed.models[0].version, second);
    assert_eq!(listed.protocol, "openai");
    assert_eq!(listed.api_key_last4, "1234", "only the tail is readable");
    assert!(!format!("{listed:?}").contains("sk-secret-1234"));

    // Two models on one channel are two independent histories.
    assert_eq!(
        db.served_models()
            .await
            .unwrap()
            .iter()
            .filter(|m| **m == model)
            .count(),
        1
    );
    assert!(db.channel_prices(&fresh("absent")).await.unwrap().is_none());
    assert!(
        db.serving(&fresh("absent"), &key())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_price_table_refuses_to_be_rewritten() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let channel = fresh("append-only");
    let model = fresh("m");
    db.set_channel(
        &channel,
        "https://upstream.example/v1",
        "sk-1",
        "openai",
        &key(),
    )
    .await
    .unwrap();
    let version = db
        .append_price(&channel, &model, price(10, 20, 100))
        .await
        .unwrap();

    // Straight at the table, as psql would. The trigger, not the API, is what makes this a rule.
    let update =
        sqlx::query("UPDATE oxsum.channel_prices SET input_price_per_million = 0 WHERE model = $1")
            .bind(&model)
            .execute(db.pool())
            .await;
    assert!(update.is_err(), "an update must be refused");
    let delete = sqlx::query("DELETE FROM oxsum.channel_prices WHERE model = $1")
        .bind(&model)
        .execute(db.pool())
        .await;
    assert!(delete.is_err(), "a delete must be refused");

    let versions = db.channel_prices(&channel).await.unwrap().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version, version);
    assert_eq!(versions[0].price(), price(10, 20, 100));
}

#[tokio::test]
async fn one_model_belongs_to_one_channel() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let first = fresh("owner");
    let second = fresh("rival");
    let model = fresh("shared");
    db.set_channel(&first, "https://a.example/v1", "sk-a", "openai", &key())
        .await
        .unwrap();
    db.set_channel(&second, "https://b.example/v1", "sk-b", "openai", &key())
        .await
        .unwrap();
    db.append_price(&first, &model, price(1, 2, 3))
        .await
        .unwrap();

    // A second channel pricing the same model would make the gateway choose between two upstreams
    // for one request, which v1 does not do (docs/product.md).
    let refused = db
        .append_price(&second, &model, price(1, 2, 3))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, WalletError::Conflict(_)),
        "got {refused:?}"
    );
    // The same channel may of course change its own price.
    db.append_price(&first, &model, price(5, 6, 7))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_credential_that_does_not_open_is_a_deployment_failure_not_a_caller_error() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let channel = fresh("sealed");
    let model = fresh("m");
    db.set_channel(
        &channel,
        "https://upstream.example/v1",
        "sk-secret",
        "openai",
        &key(),
    )
    .await
    .unwrap();

    // The startup check: every stored credential must open, and the failure names a channel. (Any
    // channel: the database is shared, so the first one checked is whichever comes first.)
    db.check_sealed_keys(&key()).await.unwrap();
    let wrong = SecretKey::from_bytes([9; 32]);
    let error = db.check_sealed_keys(&wrong).await.unwrap_err();
    assert!(
        matches!(error, WalletError::Misconfigured(_)),
        "got {error:?}"
    );
    assert!(
        error.to_string().starts_with("misconfigured: channel "),
        "names the channel: {error}"
    );

    // A record changed after it was written fails the same way, rather than relaying with a
    // plausible wrong credential: AES-GCM authenticates as well as it encrypts.
    db.append_price(&channel, &model, price(1, 2, 3))
        .await
        .unwrap();
    assert_eq!(
        db.serving(&model, &key()).await.unwrap().unwrap().api_key,
        "sk-secret"
    );
    sqlx::query("UPDATE oxsum.channels SET api_key_sealed = 'AAAA' WHERE name = $1")
        .bind(&channel)
        .execute(db.pool())
        .await
        .unwrap();
    let error = db.serving(&model, &key()).await.unwrap_err();
    assert!(
        matches!(error, WalletError::Misconfigured(_)),
        "got {error:?}"
    );

    // Replacing the connection re-seals the credential and the channel serves again.
    db.set_channel(
        &channel,
        "https://other.example/v1",
        "sk-rotated",
        "openai",
        &key(),
    )
    .await
    .unwrap();
    let serving = db.serving(&model, &key()).await.unwrap().unwrap();
    assert_eq!(serving.api_key, "sk-rotated");
    assert_eq!(serving.base_url, "https://other.example/v1");
}

#[tokio::test]
async fn the_environment_seeds_an_empty_database_and_leaves_a_populated_one_alone() {
    let url = db_or_skip!();
    let db = db(&url).await;

    // The database is shared with the other tests, so this asserts the rule "seed an empty database,
    // leave a populated one alone" without requiring this to be the first channel in it.
    let mut models = BTreeMap::new();
    models.insert("m".to_owned(), price(10, 20, 100));
    let channel = fresh("seeded");
    let book = PriceBook::new(channel.clone(), models);

    let seeded = db
        .seed_channel(&book, "https://seeded.example/v1/", "sk-seed", &key())
        .await
        .unwrap();
    let again = db
        .seed_channel(&book, "https://other.example/v1", "sk-other", &key())
        .await
        .unwrap();
    assert!(!again, "a database with channels is left alone");

    if seeded {
        // It only wins when it was the first channel in this database.
        let serving = db.serving("m", &key()).await.unwrap().unwrap();
        assert_eq!(serving.channel, channel);
        assert_eq!(
            serving.api_key, "sk-seed",
            "the bootstrap credential is the one in force"
        );
        assert_eq!(
            serving.base_url, "https://seeded.example/v1",
            "no trailing slash stored"
        );
        assert_eq!(serving.version, 1);
    } else {
        assert!(
            db.channels()
                .await
                .unwrap()
                .iter()
                .all(|c| c.name != channel),
            "nothing was written"
        );
    }
}

#[tokio::test]
async fn names_addresses_and_prices_that_could_not_be_used_are_refused() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let channel = fresh("validated");
    db.set_channel(
        &channel,
        "https://upstream.example/v1",
        "sk-secret",
        "openai",
        &key(),
    )
    .await
    .unwrap();

    assert!(
        db.set_channel("bad name", "https://x.example", "sk", "openai", &key())
            .await
            .is_err()
    );
    assert!(
        db.set_channel("", "https://x.example", "sk", "openai", &key())
            .await
            .is_err()
    );
    assert!(
        db.set_channel("ok", "ftp://x.example", "sk", "openai", &key())
            .await
            .is_err()
    );
    assert!(
        db.set_channel("ok", "not a url", "sk", "openai", &key())
            .await
            .is_err()
    );
    assert!(
        db.set_channel("ok", "https://x.example", "  ", "openai", &key())
            .await
            .is_err()
    );
    assert!(
        db.set_channel("ok", "https://x.example", "sk", "gemini", &key())
            .await
            .is_err(),
        "a protocol with no adapter is refused: the channel could never normalize usage"
    );
    assert!(
        db.append_price(&fresh("missing"), "m", price(1, 2, 3))
            .await
            .is_err(),
        "an unknown channel is refused"
    );
    assert!(
        db.append_price(&channel, "m", price(-1, 0, 1))
            .await
            .is_err(),
        "a negative price is refused"
    );
    assert!(
        db.append_price(&channel, "m", price(1, 0, 0))
            .await
            .is_err(),
        "a price with no output ceiling is refused"
    );
    assert!(
        db.append_price(&channel, " ", price(1, 0, 1))
            .await
            .is_err(),
        "an empty model is refused"
    );

    // A channel may not exceed what the admin URL path can carry.
    let long = "c".repeat(41);
    assert!(
        db.set_channel(&long, "https://x.example", "sk", "openai", &key())
            .await
            .is_err()
    );

    // The refused calls wrote nothing.
    let listed = db
        .channels()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.name == channel)
        .unwrap();
    assert!(listed.models.is_empty());
}
