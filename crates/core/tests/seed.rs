//! The demo seed (issue #164), against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of
//! failing, so a bare `cargo test` still passes.
//!
//! `seed_demo` writes the world a first `docker compose up` should show: the demo
//! user and a second member under a budget, keys with and without constraints,
//! deposits and top-ups, a month of settled turns the verifier can read back,
//! one hold still in flight, and a finalized statement for the month that just
//! ended. The demo email is the whole idempotency check — a second run writes
//! nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{DEMO_EMAIL, Db, MEMBER_EMAIL, Price, SecretKey, SettlementKind, Tenants};
use sqlx::PgPool;
use uuid::Uuid;

/// Both tests share the database — a cleanup racing a seed would see the other
/// run's freshly-registered demo email as a conflict, so they run in sequence.
static SEED_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// A migrated database with the previous seed's world removed, so the suite is
/// rerunnable on a database that was seeded before. The fixed emails are the
/// feature under test, so they are cleared rather than suffixed. Every FK off
/// `users`/`organizations` is RESTRICT — the teardown walks the graph in
/// dependency order and drops the tenant ledger schema afterwards.
async fn db(url: &str) -> Db {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    clear_seed(&db).await;
    db
}

async fn clear_seed(db: &Db) {
    let orgs: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT o.organization_id, o.tenant_id \
         FROM oxsum.users u \
         JOIN oxsum.memberships m ON m.user_id = u.user_id \
         JOIN oxsum.organizations o ON o.organization_id = m.organization_id \
         WHERE u.email_normalized IN ($1, $2)",
    )
    .bind(DEMO_EMAIL)
    .bind(MEMBER_EMAIL)
    .fetch_all(db.pool())
    .await
    .expect("finds a previous seed's organizations");

    for (organization, tenant) in &orgs {
        // Tenant-keyed history first: usage_records and usage_daily reference
        // the ledger tenant, not the organization row.
        for table in ["usage_records", "usage_daily", "open_holds"] {
            sqlx::query(&format!("DELETE FROM oxsum.{table} WHERE tenant_id = $1"))
                .bind(tenant)
                .execute(db.pool())
                .await
                .expect("clears the seeded tenant's usage history");
        }
        // statement_lines cascades off statements; webhook and idempotency
        // tables cascade off the organization itself.
        for table in [
            "statements",
            "deposits",
            "sessions",
            "invitations",
            "api_keys",
            "memberships",
            "pricing_discounts",
            "device_codes",
        ] {
            sqlx::query(&format!(
                "DELETE FROM oxsum.{table} WHERE organization_id = $1"
            ))
            .bind(organization)
            .execute(db.pool())
            .await
            .expect("clears the seeded organization's rows");
        }
        sqlx::query("DELETE FROM oxsum.organizations WHERE organization_id = $1")
            .bind(organization)
            .execute(db.pool())
            .await
            .expect("clears the seeded organization");
        let schema = format!("ledger_{}", tenant.replace('"', "\"\""));
        sqlx::query(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"))
            .execute(db.pool())
            .await
            .expect("drops the seeded tenant ledger");
    }

    // Seed-shaped residue outlives a half-torn-down world — the seeded request
    // and hold keys are globally unique, so rows whose organization is already
    // gone would still collide with the next run.
    sqlx::query("DELETE FROM oxsum.open_holds WHERE hold_key LIKE 'req-seed-%'")
        .execute(db.pool())
        .await
        .expect("clears stale seeded holds");
    sqlx::query("DELETE FROM oxsum.usage_records WHERE request_id LIKE 'seed-%'")
        .execute(db.pool())
        .await
        .expect("clears stale seeded usage");
    sqlx::query(
        "DELETE FROM oxsum.sessions WHERE user_id IN \
         (SELECT user_id FROM oxsum.users WHERE email_normalized IN ($1, $2))",
    )
    .bind(DEMO_EMAIL)
    .bind(MEMBER_EMAIL)
    .execute(db.pool())
    .await
    .expect("clears the seeded users' sessions");
    sqlx::query("DELETE FROM oxsum.users WHERE email_normalized IN ($1, $2)")
        .bind(DEMO_EMAIL)
        .bind(MEMBER_EMAIL)
        .execute(db.pool())
        .await
        .expect("clears the seeded users");
}

/// A channel and two priced models for the seeded turns to bill under — the
/// test deployment serves what `oxsum seed` finds in the catalog.
async fn serve_models(db: &Db) {
    let suffix = &Uuid::new_v4().simple().to_string()[..8];
    let channel = format!("seed-{suffix}");
    db.set_channel(
        &channel,
        "https://upstream.example/v1",
        "sk-seed-channel",
        "openai",
        &SecretKey::from_bytes([7; 32]),
    )
    .await
    .unwrap();
    // Model names ride the channel suffix — a model belongs to exactly one
    // channel, so fixed names would collide with an earlier run's leftovers.
    for (model, input, output) in [
        (format!("seed-mini-{suffix}"), 500i64, 2_000i64),
        (format!("seed-pro-{suffix}"), 5_000i64, 15_000i64),
    ] {
        let model = model.as_str();
        db.append_price(
            &channel,
            model,
            Price {
                input_price_per_million: input,
                output_price_per_million: output,
                max_output_tokens: 8_192,
                cache_read_price_per_million: Some(input / 10),
                cache_write_5m_price_per_million: None,
                cache_write_1h_price_per_million: None,
                reasoning_price_per_million: Some(output * 3),
                cost_per_request: Some(10),
                upstream: Some(oxsum_core::UpstreamPrices {
                    input_price_per_million: Some(input / 2),
                    output_price_per_million: Some(output / 2),
                    cache_read_price_per_million: None,
                    cache_write_5m_price_per_million: None,
                    cache_write_1h_price_per_million: None,
                    reasoning_price_per_million: None,
                    cost_per_request: None,
                }),
                mode: Default::default(),
                rules: Vec::new(),
            },
            100,
        )
        .await
        .unwrap();
    }
}

/// The demo world's own organization, for assertions that name it.
async fn demo_org(db: &Db) -> (Uuid, String) {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT o.organization_id, o.tenant_id FROM oxsum.organizations o \
         JOIN oxsum.memberships m USING (organization_id) \
         JOIN oxsum.users u USING (user_id) \
         WHERE u.email_normalized = $1",
    )
    .bind(DEMO_EMAIL)
    .fetch_one(db.pool())
    .await
    .expect("the demo organization exists")
}

#[tokio::test]
async fn the_seed_populates_a_believable_world() {
    let _guard = SEED_LOCK.lock().await;
    let url = db_or_skip!();
    let db = db(&url).await;
    serve_models(&db).await;

    let report = db.seed_demo().await.unwrap();
    assert!(report.created);
    assert_eq!(report.email, DEMO_EMAIL);
    assert_eq!(report.member_email, MEMBER_EMAIL);
    assert!(report.api_secret.is_some(), "the first key prints once");
    assert!(report.turns > 0);
    assert!(report.topped_up_minor > report.spent_minor);

    // The printed secret authenticates — the report's promise is real.
    let secret = report.api_secret.as_deref().unwrap();
    let (organization, _) = db.authenticate(secret).await.unwrap().unwrap();
    assert_eq!(organization.id, demo_org(&db).await.0);

    // Members: the owner, plus the seeded member under a budget.
    let members = db.members(organization.id).await.unwrap();
    assert_eq!(members.len(), 2);
    let member = members
        .iter()
        .find(|m| m.email == MEMBER_EMAIL)
        .expect("the member is on the organization");
    assert!(member.budget_limit_minor.is_some());

    // The wallet shows the deposits minus the settled spend, and the in-flight
    // hold keeps part of the balance reserved.
    let wallet = Tenants::new(db.pool().clone())
        .get(&organization.tenant_id)
        .await
        .unwrap();
    assert_eq!(
        wallet.settled().await.unwrap(),
        report.topped_up_minor - report.spent_minor,
        "the settled balance is what the top-ups left after the seeded spend"
    );
    assert!(wallet.available().await.unwrap() > 0);
    assert!(
        wallet.reserved().await.unwrap() > 0,
        "the open hold freezes"
    );

    // The requests page reads settled turns back — every seeded kind present.
    let requests = wallet.recent_requests(200).await.unwrap();
    assert!(requests.len() as i64 >= report.turns);
    let kinds: Vec<SettlementKind> = requests.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&SettlementKind::Usage));
    assert!(kinds.contains(&SettlementKind::Capped));
    assert!(kinds.contains(&SettlementKind::Unpriced));

    // The usage rollup feeds the usage page over the seeded days.
    let today = time::OffsetDateTime::now_utc().date();
    let days = db
        .usage_daily(
            &organization.tenant_id,
            today - time::Duration::days(40),
            today,
        )
        .await
        .unwrap();
    assert!(days.len() > 10, "the rollup spans the seeded history");
    assert!(days.iter().map(|d| d.turns).sum::<i64>() >= report.turns);

    // Last month's statement stands finalized.
    assert!(report.statement.is_some());
    let statements = db.organization_statements(organization.id).await.unwrap();
    assert!(
        statements
            .iter()
            .any(|s| Some(s.period.as_str()) == report.statement.as_deref())
    );
}

#[tokio::test]
async fn a_second_seed_writes_nothing() {
    let _guard = SEED_LOCK.lock().await;
    let url = db_or_skip!();
    let db = db(&url).await;

    let first = db.seed_demo().await.unwrap();
    assert!(first.created, "the first run populates");
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.usage_records")
        .fetch_one(db.pool())
        .await
        .unwrap();

    let second = db.seed_demo().await.unwrap();
    assert!(!second.created, "the demo email makes the run a no-op");
    assert!(second.api_secret.is_none());
    assert_eq!(second.turns, 0);

    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.usage_records")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after, "nothing new lands on a rerun");
}
