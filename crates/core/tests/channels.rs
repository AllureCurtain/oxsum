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
//! tests, and though a model may be served by several channels, a shared model name would make
//! the tests mean different things depending on which one ran first.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, MAX_WEIGHT, Price, PriceBook, SecretKey, WalletError};
use sqlx::PgPool;
use std::collections::BTreeMap;

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("channels")
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
        .append_price(&channel, &model, price(10, 20, 100), 100)
        .await
        .unwrap();
    let second = db
        .append_price(&channel, &model, price(30, 40, 200), 100)
        .await
        .unwrap();
    assert_eq!((first, second), (1, 2), "versions count up from one");

    // What a request starts on is the newest version…
    let serving = db
        .servings(&model, &key())
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(serving.channel, channel);
    assert_eq!(serving.protocol, "openai");
    assert_eq!(serving.version, second);
    assert_eq!(serving.price, price(30, 40, 200));
    assert_eq!(serving.credentials.len(), 1);
    assert_eq!(serving.credentials[0].api_key, "sk-secret-1234");
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
    assert_eq!(listed.credentials.len(), 1);
    assert_eq!(
        listed.credentials[0].api_key_last4, "1234",
        "only the tail is readable"
    );
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
        db.servings(&fresh("absent"), &key())
            .await
            .unwrap()
            .is_empty()
    );
}

/// The catalog reads the same current versions `serving` resolves, without the
/// credential — `priced` for one model, `catalog` for all of them.
#[tokio::test]
async fn the_catalog_shows_the_current_price_and_no_credential() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let channel = fresh("cataloged");
    let model = fresh("m");

    db.set_channel(
        &channel,
        "https://upstream.example/v1",
        "sk-hidden-9999",
        "anthropic",
        &key(),
    )
    .await
    .unwrap();
    db.append_price(&channel, &model, price(10, 20, 100), 100)
        .await
        .unwrap();
    let latest = db
        .append_price(&channel, &model, price(30, 40, 200), 100)
        .await
        .unwrap();

    // `priced` resolves one model to its newest version, with channel and
    // protocol and without opening the sealed credential.
    let priced = db.routes(&model).await.unwrap().into_iter().next().unwrap();
    assert_eq!(priced.channel, channel);
    assert_eq!(priced.protocol, "anthropic");
    assert_eq!(priced.version, latest);
    assert_eq!(priced.price, price(30, 40, 200));
    assert!(db.routes(&fresh("absent")).await.unwrap().is_empty());

    // `catalog` is the same row for every model.
    let catalog = db.catalog().await.unwrap();
    let row = catalog.iter().find(|row| row.model == model).unwrap();
    assert_eq!(row.channel, channel);
    assert_eq!(row.version, latest);
    // Every model appears once: the rollup picks the current version, not a row
    // per version.
    assert_eq!(catalog.iter().filter(|row| row.model == model).count(), 1);
    // Nothing credential-shaped is on the row, and the payload serializes
    // without it.
    let json = serde_json::to_value(row).unwrap();
    assert!(!json.to_string().contains("sk-hidden-9999"));
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
        .append_price(&channel, &model, price(10, 20, 100), 100)
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

/// Failover (issue #168): a model may be priced on several channels, each
/// route carrying its own version lineage and weight. `servings` answers every
/// candidate; `route_order` leads with a weighted pick and follows in
/// descending weight.
#[tokio::test]
async fn one_model_may_be_served_by_several_channels() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let first = fresh("primary");
    let second = fresh("backup");
    let model = fresh("shared");
    db.set_channel(&first, "https://a.example/v1", "sk-a", "openai", &key())
        .await
        .unwrap();
    db.set_channel(&second, "https://b.example/v1", "sk-b", "openai", &key())
        .await
        .unwrap();
    db.append_price(&first, &model, price(1, 2, 3), 100)
        .await
        .unwrap();
    // A second channel pricing the same model is a route, not a conflict.
    db.append_price(&second, &model, price(2, 4, 6), 10)
        .await
        .unwrap();

    // Every route answers with its own version and weight.
    let servings = db.servings(&model, &key()).await.unwrap();
    assert_eq!(servings.len(), 2);
    let routes = db.routes(&model).await.unwrap();
    assert_eq!(routes.len(), 2, "routes lists one row per channel");
    assert_eq!(routes[0].weight, 100, "weight desc first");
    assert_eq!(routes[1].weight, 10);

    // The catalog lists one row per route.
    let catalog = db.catalog().await.unwrap();
    assert_eq!(catalog.iter().filter(|row| row.model == model).count(), 2);

    // The weighted pick walks the name-sorted candidates — backup (10) before
    // primary (100) — so a roll under 10/110 leads with the backup, anything
    // at or above leads with the primary, and the loser follows.
    let ordered = oxsum_core::route_order(servings.clone(), 0.5);
    assert_eq!(ordered[0].channel, first);
    assert_eq!(ordered[1].channel, second);
    let ordered = oxsum_core::route_order(servings, 0.05);
    assert_eq!(ordered[0].channel, second);
    assert_eq!(ordered[1].channel, first);

    // The same channel may of course change its own price — and its weight,
    // which versions with it.
    db.append_price(&first, &model, price(5, 6, 7), 500)
        .await
        .unwrap();
    let routes = db.routes(&model).await.unwrap();
    let primary = routes.iter().find(|row| row.channel == first).unwrap();
    assert_eq!(primary.version, 2);
    assert_eq!(primary.weight, 500);
}

/// Cost-aware routing (issue #438): two routes the weight rule prefers equally
/// are ordered by input price — the cheaper leads whatever the roll, and the
/// dearer falls back to failover position. Weight stays the dominant term: a
/// lower-weight route never outranks a higher one for being cheaper.
#[tokio::test]
async fn equally_weighted_routes_lead_with_the_cheapest() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let dear = fresh("dear");
    let cheap = fresh("cheap");
    let model = fresh("shared-cost");
    db.set_channel(&dear, "https://dear.example/v1", "sk-d", "openai", &key())
        .await
        .unwrap();
    db.set_channel(&cheap, "https://cheap.example/v1", "sk-c", "openai", &key())
        .await
        .unwrap();
    // Identical weights, different input prices.
    db.append_price(&dear, &model, price(40, 4, 100), 100)
        .await
        .unwrap();
    db.append_price(&cheap, &model, price(10, 4, 100), 100)
        .await
        .unwrap();

    let servings = db.servings(&model, &key()).await.unwrap();
    assert_eq!(servings.len(), 2);
    // Every roll in [0, 1) lands on the shared weight tier, and the tier is
    // always represented by its cheapest route.
    for roll in [0.0, 0.25, 0.5, 0.9, 0.999] {
        let ordered = oxsum_core::route_order(servings.clone(), roll);
        assert_eq!(
            ordered[0].channel, cheap,
            "roll {roll}: the cheaper route of an equal-weight tier leads"
        );
        assert_eq!(ordered[1].channel, dear);
    }

    // Weight still dominates: a third route that is *cheapest* of all but
    // carries a tenth of the others' weight never leads — the roll that lands
    // on its tier leads with it, but every roll in the two heavy tiers leads
    // with the cheap one of those.
    let third = fresh("mid");
    db.set_channel(&third, "https://mid.example/v1", "sk-m", "openai", &key())
        .await
        .unwrap();
    db.append_price(&third, &model, price(1, 4, 100), 10)
        .await
        .unwrap();
    let servings = db.servings(&model, &key()).await.unwrap();
    // Name order is cheap-, dear-, mid-: weights 100, 100, 10 out of 210, so a
    // roll below 100/210 lands on the cheap route's tier, [100,200)/210 the
    // dear one's — which the cheap route still represents — and only the last
    // sliver lands on the light route.
    for roll in [0.0, 0.4, 0.5, 0.9] {
        let ordered = oxsum_core::route_order(servings.clone(), roll);
        assert_eq!(
            ordered[0].channel, cheap,
            "roll {roll}: heavy tier, cheap leads"
        );
    }
    let ordered = oxsum_core::route_order(servings, 0.98);
    assert_eq!(
        ordered[0].channel, third,
        "the light tier's own roll leads with it"
    );
    // And within the tail, weight descends then price ascends.
    assert_eq!(ordered[1].channel, cheap);
    assert_eq!(ordered[2].channel, dear);
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
    db.append_price(&channel, &model, price(1, 2, 3), 100)
        .await
        .unwrap();
    assert_eq!(
        db.servings(&model, &key())
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .credentials[0]
            .api_key,
        "sk-secret"
    );
    sqlx::query(
        "UPDATE oxsum.channel_credentials SET api_key_sealed = 'AAAA' \
         WHERE channel_id = (SELECT channel_id FROM oxsum.channels WHERE name = $1)",
    )
    .bind(&channel)
    .execute(db.pool())
    .await
    .unwrap();
    let error = db.servings(&model, &key()).await.unwrap_err();
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
    let serving = db
        .servings(&model, &key())
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(serving.credentials[0].api_key, "sk-rotated");
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
        // It only wins when it was the first channel in this database. Other
        // tests may serve a bare "m" too — find this channel's route among them.
        let serving = db
            .servings("m", &key())
            .await
            .unwrap()
            .into_iter()
            .find(|serving| serving.channel == channel)
            .unwrap();
        assert_eq!(serving.channel, channel);
        assert_eq!(
            serving.credentials[0].api_key, "sk-seed",
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
        db.set_channel("ok", "https://x.example", "sk", "grok", &key())
            .await
            .is_err(),
        "a protocol with no adapter is refused: the channel could never normalize usage"
    );
    assert!(
        db.append_price(&fresh("missing"), "m", price(1, 2, 3), 100)
            .await
            .is_err(),
        "an unknown channel is refused"
    );
    assert!(
        db.append_price(&channel, "m", price(-1, 0, 1), 100)
            .await
            .is_err(),
        "a negative price is refused"
    );
    assert!(
        db.append_price(&channel, "m", price(1, 0, 0), 100)
            .await
            .is_err(),
        "a price with no output ceiling is refused"
    );
    assert!(
        db.append_price(&channel, " ", price(1, 0, 1), 100)
            .await
            .is_err(),
        "an empty model is refused"
    );
    assert!(
        db.append_price(&channel, "m", price(1, 0, 1), 0)
            .await
            .is_err(),
        "a zero weight is refused"
    );
    assert!(
        db.append_price(&channel, "m", price(1, 0, 1), MAX_WEIGHT + 1)
            .await
            .is_err(),
        "a weight above the bound is refused"
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

/// A model alias renames what a channel relays (issue #286): the name resolves
/// to the claiming channel alone, priced by the alias's own row when the
/// channel wrote one and by the upstream model's otherwise — and the
/// one-name-one-route-set rule refuses a second claim, whether it comes as an
/// alias on another channel or as a name another channel serves directly.
#[tokio::test]
async fn an_alias_resolves_to_its_channel_and_prices_by_its_own_row() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let channel = fresh("alias");
    let other = fresh("other");
    let upstream = fresh("real");
    let alias = fresh("public");
    db.set_channel(&channel, "https://a.example/v1", "sk-a", "openai", &key())
        .await
        .unwrap();
    db.set_channel(&other, "https://b.example/v1", "sk-b", "openai", &key())
        .await
        .unwrap();
    db.append_price(&channel, &upstream, price(10, 20, 100), 100)
        .await
        .unwrap();

    // The claim: write, read back on the channel's list, resolve.
    let written = db.create_alias(&channel, &alias, &upstream).await.unwrap();
    assert_eq!(written.alias, alias);
    assert_eq!(written.channel, channel);
    assert_eq!(written.upstream_model, upstream);
    let listed = db.list_aliases(&channel).await.unwrap().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].alias, alias);
    assert!(db.list_aliases(&fresh("absent")).await.unwrap().is_none());
    assert_eq!(
        db.resolve_alias(&alias).await.unwrap(),
        Some((channel.clone(), upstream.clone()))
    );
    assert!(db.resolve_alias(&upstream).await.unwrap().is_none());

    // No alias-named price yet: the route is the channel's, priced by the
    // upstream model's row, and the upstream name rides along.
    let (servings, resolved) = db.servings_named(&alias, &key()).await.unwrap();
    assert_eq!(resolved.as_deref(), Some(upstream.as_str()));
    let serving = servings.into_iter().next().unwrap();
    assert_eq!(serving.channel, channel);
    assert_eq!(serving.price, price(10, 20, 100));

    // The alias's own price wins once the channel writes one.
    db.append_price(&channel, &alias, price(99, 88, 77), 100)
        .await
        .unwrap();
    let (servings, _) = db.servings_named(&alias, &key()).await.unwrap();
    let serving = servings.into_iter().next().unwrap();
    assert_eq!(serving.price, price(99, 88, 77));
    assert_eq!(serving.version, 1, "the alias's own version lineage");

    // A name that is no alias resolves like before — every channel serving it,
    // and no upstream name follows.
    let (servings, resolved) = db.servings_named(&upstream, &key()).await.unwrap();
    assert_eq!(resolved, None);
    assert_eq!(servings.len(), 1);

    // One name, one route set: the alias on another channel conflicts, and so
    // does a name another channel serves directly — while the same channel may
    // freely alias a name it serves itself.
    let conflict = db
        .create_alias(&other, &alias, &upstream)
        .await
        .unwrap_err();
    assert!(matches!(conflict, WalletError::Conflict(_)), "{conflict:?}");
    let own = fresh("own");
    db.append_price(&other, &own, price(1, 2, 3), 100)
        .await
        .unwrap();
    let conflict = db
        .create_alias(&channel, &own, &upstream)
        .await
        .unwrap_err();
    assert!(matches!(conflict, WalletError::Conflict(_)), "{conflict:?}");

    // Validation: empty names are refused; an unknown channel is not found.
    assert!(matches!(
        db.create_alias(&channel, "", &upstream).await.unwrap_err(),
        WalletError::InvalidInput(_)
    ));
    assert!(matches!(
        db.create_alias(&channel, &fresh("x"), "")
            .await
            .unwrap_err(),
        WalletError::InvalidInput(_)
    ));
    assert!(matches!(
        db.create_alias(&fresh("missing"), &fresh("x"), &upstream)
            .await
            .unwrap_err(),
        WalletError::InvalidInput(_)
    ));

    // The delete is this channel's alone, and hard.
    assert!(db.delete_alias(&channel, &alias).await.unwrap());
    assert!(!db.delete_alias(&channel, &alias).await.unwrap());
    assert!(db.resolve_alias(&alias).await.unwrap().is_none());
    // Deleting against the channel that never claimed it is false, not an error.
    let again = fresh("public");
    db.create_alias(&channel, &again, &upstream).await.unwrap();
    assert!(!db.delete_alias(&other, &again).await.unwrap());
    assert!(db.resolve_alias(&again).await.unwrap().is_some());
}

/// Credential pools (issue #444): a channel carries several upstream keys, the
/// pick filters out the cooled and disabled, and `credential_order` ranks the
/// rest — a conversation's sticky hit leads, then least-recently-used, then
/// `position`.
#[tokio::test]
async fn a_channel_serves_from_its_credential_pool() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let channel = fresh("pooled");
    let model = fresh("m");
    db.set_channel(
        &channel,
        "https://pool.example/v1",
        "sk-first",
        "openai",
        &key(),
    )
    .await
    .unwrap();
    db.append_price(&channel, &model, price(1, 2, 3), 100)
        .await
        .unwrap();

    // The seeded credential is the pool's first; a second joins at the end.
    let listed = db.channel_credentials(&channel).await.unwrap().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].api_key_last4, "irst");
    assert_eq!(listed[0].position, 1);
    assert_eq!(listed[0].status, "active");
    let second = db
        .add_credential(&channel, "sk-second", &key())
        .await
        .unwrap();
    assert_eq!(second.position, 2);
    let third = db
        .add_credential(&channel, "sk-third", &key())
        .await
        .unwrap();
    assert_eq!(third.position, 3);
    assert!(
        db.channel_credentials(&fresh("absent"))
            .await
            .unwrap()
            .is_none(),
        "an unknown channel has no pool to list"
    );

    // The serving carries the whole healthy pool, opened — still one serving
    // per channel, so `route_order` sees one route either way.
    let servings = db.servings(&model, &key()).await.unwrap();
    let serving = servings
        .iter()
        .find(|serving| serving.channel == channel)
        .unwrap();
    assert_eq!(serving.credentials.len(), 3);
    let opened: Vec<&str> = serving
        .credentials
        .iter()
        .map(|credential| credential.api_key.as_str())
        .collect();
    assert_eq!(opened, ["sk-first", "sk-second", "sk-third"]);

    // The pick's health filter: cool the first credential and it drops out of
    // the serving's pool — `add_credential` appends, `cool_credential` rests.
    db.cool_credential(listed[0].credential_id, "upstream answered 429")
        .await
        .unwrap();
    let servings = db.servings(&model, &key()).await.unwrap();
    let serving = servings
        .iter()
        .find(|serving| serving.channel == channel)
        .unwrap();
    let opened: Vec<&str> = serving
        .credentials
        .iter()
        .map(|credential| credential.api_key.as_str())
        .collect();
    assert_eq!(
        opened,
        ["sk-second", "sk-third"],
        "a cooling credential is skipped, not retried"
    );
    // …and the admin list shows why it sits out.
    let listed = db.channel_credentials(&channel).await.unwrap().unwrap();
    assert!(listed[0].cooldown_until.is_some());
    assert_eq!(
        listed[0].last_error.as_deref(),
        Some("upstream answered 429")
    );

    // Disabled is the operator's switch: kept on record, never picked.
    db.set_credential_status(&channel, second.credential_id, "disabled")
        .await
        .unwrap();
    let servings = db.servings(&model, &key()).await.unwrap();
    let serving = servings
        .iter()
        .find(|serving| serving.channel == channel)
        .unwrap();
    assert_eq!(serving.credentials.len(), 1);
    assert_eq!(serving.credentials[0].api_key, "sk-third");
    db.set_credential_status(&channel, second.credential_id, "active")
        .await
        .unwrap();
    assert!(
        db.set_credential_status(&channel, second.credential_id, "paused")
            .await
            .is_err(),
        "an unknown status is refused"
    );

    // The pool's last *pickable* credential cannot be dropped: a cooling
    // `active` row still counts as pickable because its window ends on its
    // own, so `third` — the only other active row — drops fine. What cannot
    // drop is a credential whose loss leaves nothing `active`.
    let dropped = db
        .drop_credential(&channel, third.credential_id)
        .await
        .unwrap();
    assert!(dropped);
    assert!(
        !db.drop_credential(&channel, third.credential_id)
            .await
            .unwrap(),
        "the second drop is false, not an error"
    );
    // A credential is only its own channel's to drop: against an unknown
    // channel the same id is false, not an error.
    assert!(
        !db.drop_credential(&fresh("absent"), listed[0].credential_id)
            .await
            .unwrap()
    );

    // `set_channel` with keys re-credentials wholesale; without keys the pool
    // stands. One key in, the two old ones out.
    db.set_channel(
        &channel,
        "https://pool.example/v1",
        "sk-only",
        "openai",
        &key(),
    )
    .await
    .unwrap();
    let servings = db.servings(&model, &key()).await.unwrap();
    let serving = servings
        .iter()
        .find(|serving| serving.channel == channel)
        .unwrap();
    assert_eq!(serving.credentials.len(), 1);
    assert_eq!(serving.credentials[0].api_key, "sk-only");
}

/// The pick orders a pool's credentials: the conversation's sticky hit leads
/// while it is still healthy, then least-recently-used, `position` as the
/// operator's tiebreak.
#[test]
fn the_pool_pick_sticks_then_rotates_by_use() {
    let one = oxsum_core::PoolCredential {
        credential_id: uuid::Uuid::new_v4(),
        api_key: "sk-one".to_owned(),
        position: 1,
        last_used_at: Some(time::OffsetDateTime::now_utc()),
    };
    let two = oxsum_core::PoolCredential {
        credential_id: uuid::Uuid::new_v4(),
        api_key: "sk-two".to_owned(),
        position: 2,
        last_used_at: None,
    };
    let three = oxsum_core::PoolCredential {
        credential_id: uuid::Uuid::new_v4(),
        api_key: "sk-three".to_owned(),
        position: 3,
        last_used_at: None,
    };
    let pool = vec![one.clone(), two.clone(), three.clone()];

    // No sticky: never-used first (position order among them), the used one last.
    let ordered = oxsum_core::credential_order(&pool, None);
    let keys: Vec<&str> = ordered.iter().map(|c| c.api_key.as_str()).collect();
    assert_eq!(keys, ["sk-two", "sk-three", "sk-one"]);

    // A sticky hit leads whatever the use order says.
    let ordered = oxsum_core::credential_order(&pool, Some(one.credential_id));
    assert_eq!(ordered[0].api_key, "sk-one");
    // The rest still order least-used first.
    let rest: Vec<&str> = ordered[1..].iter().map(|c| c.api_key.as_str()).collect();
    assert_eq!(rest, ["sk-two", "sk-three"]);
}
