//! The metering API's core (issue #172): service credentials, and the
//! hold/settle/release/one-shot orchestration over a real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! Every test names its channel, billable code and service credential with a
//! random suffix: the database is shared between tests, and a name another
//! test wrote would change what this one means.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    BillingMode, ChargeCheck, Db, MeteredEvent, NewUser, Price, SecretKey, SettlementKind,
    SettlementRecord, Tenants, UsageRecord, WalletError, verify_charge,
};
use sqlx::PgPool;
use time::macros::date;
use uuid::Uuid;

const D: time::Date = date!(2026 - 10 - 01);
const ONE: i64 = 1_000_000;

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

/// Each test uses its own channel, code and credential names, so tests never
/// interfere and reruns never collide.
fn fresh(name: &str) -> String {
    format!("{name}-{}", &Uuid::new_v4().simple().to_string()[..8])
}

/// An `event`-mode price: input metered, no output side, nothing forwarded.
fn event_price(input_per_million: i64) -> Price {
    Price {
        input_price_per_million: input_per_million,
        output_price_per_million: 0,
        max_output_tokens: 0,
        cache_read_price_per_million: None,
        cache_write_5m_price_per_million: None,
        cache_write_1h_price_per_million: None,
        reasoning_price_per_million: None,
        cost_per_request: None,
        upstream: None,
        mode: BillingMode::Event,
        rules: vec![],
    }
}

/// The usage a metered event declares: `units` in the code's denomination —
/// input tokens here — and the event's kind.
fn event_usage(units: i64) -> UsageRecord {
    UsageRecord {
        input_tokens: units,
        event_type: Some("mail.send".to_owned()),
        ..UsageRecord::default()
    }
}

/// A migrated database, a funded organization, and a service credential that
/// reports for it — the world every metering call needs.
struct World {
    db: Db,
    tenants: Tenants,
    organization_id: Uuid,
    tenant_id: String,
    service: oxsum_core::ActingService,
    channel: String,
    code: String,
}

async fn world(url: &str, top_up: i64) -> World {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let registration = db
        .register(NewUser {
            email: format!(
                "metering_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    let organization_id = registration.organization.id;
    let tenant_id = registration.organization.tenant_id.clone();
    let tenants = Tenants::new(pool.clone());
    let wallet = tenants
        .get(&tenant_id)
        .await
        .expect("the organization's wallet opens");
    if top_up > 0 {
        wallet.top_up("topup", top_up, D).await.unwrap();
    }
    // A channel and an `event` price for the billable code — the price book
    // entry every metering call resolves. Metering never calls the upstream,
    // so the address only has to be a URL.
    let channel = fresh("metered");
    let code = fresh("mail.send");
    db.set_channel(
        &channel,
        "http://127.0.0.1:9/unused",
        "upstream-secret",
        "openai",
        &SecretKey::from_bytes([7; 32]),
    )
    .await
    .expect("the channel registers");
    db.append_price(&channel, &code, event_price(ONE), 100)
        .await
        .expect("the event price appends");
    let minted = db
        .create_service_credential(Some(fresh("mailer")))
        .await
        .expect("the credential mints");
    let service = db
        .authenticate_service(&minted.secret)
        .await
        .expect("authenticates")
        .expect("the minted credential resolves");
    World {
        db,
        tenants,
        organization_id,
        tenant_id,
        service,
        channel,
        code,
    }
}

impl World {
    fn event(&self, units: i64) -> MeteredEvent {
        MeteredEvent {
            channel: self.channel.clone(),
            billable_code: self.code.clone(),
            event_id: None,
            usage: event_usage(units),
        }
    }
}

// ── the credential ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_service_credential_mints_authenticates_and_revokes() {
    let url = db_or_skip!();
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");

    let name = fresh("mailer");
    let minted = db
        .create_service_credential(Some(name.clone()))
        .await
        .unwrap();
    // The secret's shape is the documented one, and it is answered once.
    assert!(minted.secret.starts_with("oxs-svc-"));
    assert_eq!(minted.secret.len(), "oxs-svc-".len() + 64);
    assert_eq!(minted.credential.name.as_deref(), Some(name.as_str()));
    assert_eq!(
        minted.credential.prefix,
        minted.secret.chars().take(16).collect::<String>()
    );

    // The secret resolves to the credential; a wrong one resolves to nothing.
    let acting = db.authenticate_service(&minted.secret).await.unwrap();
    assert_eq!(
        acting
            .expect("the minted secret authenticates")
            .credential_id,
        minted.credential.id
    );
    assert!(
        db.authenticate_service(
            "oxs-svc-0000000000000000000000000000000000000000000000000000000000000000"
        )
        .await
        .unwrap()
        .is_none()
    );

    // The list carries the credential's metadata, never the secret or hash.
    let listed = db.list_service_credentials().await.unwrap();
    let entry = listed
        .iter()
        .find(|c| c.id == minted.credential.id)
        .expect("the minted credential lists");
    assert!(entry.revoked_at.is_none());
    assert!(
        !entry
            .prefix
            .contains(&minted.secret["oxs-svc-".len() + 8..])
    );

    // Revocation ends authentication, and a second revoke is not an answer.
    let revoked = db
        .revoke_service_credential(minted.credential.id)
        .await
        .unwrap()
        .expect("the live credential revokes");
    assert!(revoked.revoked_at.is_some());
    assert!(
        db.authenticate_service(&minted.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.revoke_service_credential(minted.credential.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.revoke_service_credential(Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );

    // A name over the bound is refused.
    assert!(matches!(
        db.create_service_credential(Some("x".repeat(81))).await,
        Err(WalletError::InvalidInput(_))
    ));
}

// ── holds, settle, release ───────────────────────────────────────────────────

#[tokio::test]
async fn a_hold_freezes_the_declared_bound_and_settles_at_actual() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;

    // 100 units at 1 credit per million is the bound; the hold freezes it.
    let hold =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "hold-1",
            &w.event(100),
            D,
        )
        .await
        .unwrap();
    assert_eq!(hold.freeze_minor, 100);
    assert_eq!(hold.price_version, 1);
    assert_eq!(hold.hold_key, "metering:hold-1:hold");

    // The settled usage prices inside the freeze: usage kind, charged 40.
    let outcome =
        w.db.metering_settle(
            &w.tenants,
            &w.service,
            w.organization_id,
            &hold.hold_key,
            None,
            &event_usage(40),
            D,
        )
        .await
        .unwrap();
    assert_eq!(outcome.charged_minor, 40);
    assert_eq!(outcome.kind, SettlementKind::Usage);
    assert_eq!(outcome.price_version, 1);

    // The watch row is cleared and the usage row records the event.
    assert!(w.db.open_hold(&hold.hold_key).await.unwrap().is_none());
    let (charged, kind): (i64, String) =
        sqlx::query_as("SELECT charged_minor, kind FROM oxsum.usage_records WHERE request_id = $1")
            .bind(&hold.hold_key)
            .fetch_one(w.db.pool())
            .await
            .expect("the usage row is there");
    assert_eq!(charged, 40);
    assert_eq!(kind, "usage");
}

#[tokio::test]
async fn a_replayed_hold_answers_the_same_freeze() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;

    let first =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "h-re",
            &w.event(50),
            D,
        )
        .await
        .unwrap();
    let replay =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "h-re",
            &w.event(50),
            D,
        )
        .await
        .unwrap();
    assert_eq!(first.hold_key, replay.hold_key);
    assert_eq!(first.freeze_minor, replay.freeze_minor);
    assert_eq!(first.price_version, replay.price_version);

    // The same key under a different declaration is a conflict, and the first
    // hold's watch row survives it.
    let other =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "h-re",
            &w.event(60),
            D,
        )
        .await;
    assert!(matches!(other, Err(WalletError::Conflict(_))));
    assert!(w.db.open_hold(&first.hold_key).await.unwrap().is_some());
}

#[tokio::test]
async fn a_settle_above_the_freeze_charges_the_freeze() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    let hold =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "cap",
            &w.event(50),
            D,
        )
        .await
        .unwrap();
    let outcome =
        w.db.metering_settle(
            &w.tenants,
            &w.service,
            w.organization_id,
            &hold.hold_key,
            None,
            &event_usage(80),
            D,
        )
        .await
        .unwrap();
    assert_eq!(outcome.kind, SettlementKind::Capped);
    assert_eq!(outcome.charged_minor, 50);
}

#[tokio::test]
async fn a_release_lets_the_hold_go_at_zero() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    let hold =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "rel",
            &w.event(50),
            D,
        )
        .await
        .unwrap();
    let outcome =
        w.db.metering_release(&w.tenants, &w.service, w.organization_id, &hold.hold_key, D)
            .await
            .unwrap();
    assert_eq!(outcome.kind, SettlementKind::Released);
    assert_eq!(outcome.charged_minor, 0);
    assert!(w.db.open_hold(&hold.hold_key).await.unwrap().is_none());

    // The release replays; a settle over the released hold is a conflict.
    let replay =
        w.db.metering_release(&w.tenants, &w.service, w.organization_id, &hold.hold_key, D)
            .await
            .unwrap();
    assert_eq!(replay.kind, SettlementKind::Released);
    let settled =
        w.db.metering_settle(
            &w.tenants,
            &w.service,
            w.organization_id,
            &hold.hold_key,
            None,
            &event_usage(10),
            D,
        )
        .await;
    assert!(matches!(settled, Err(WalletError::Conflict(_))));
}

#[tokio::test]
async fn a_settled_hold_replays_its_outcome() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    let hold =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "rep",
            &w.event(50),
            D,
        )
        .await
        .unwrap();
    w.db.metering_settle(
        &w.tenants,
        &w.service,
        w.organization_id,
        &hold.hold_key,
        None,
        &event_usage(30),
        D,
    )
    .await
    .unwrap();

    // A settle retry answers the committed record; a release is a different
    // operation and conflicts.
    let replay =
        w.db.metering_settle(
            &w.tenants,
            &w.service,
            w.organization_id,
            &hold.hold_key,
            None,
            &event_usage(30),
            D,
        )
        .await
        .unwrap();
    assert_eq!(replay.charged_minor, 30);
    assert_eq!(replay.kind, SettlementKind::Usage);
    let released =
        w.db.metering_release(&w.tenants, &w.service, w.organization_id, &hold.hold_key, D)
            .await;
    assert!(matches!(released, Err(WalletError::Conflict(_))));

    // A hold nobody ever took is a plain 404 on either path.
    for call in ["settle", "release"] {
        let result = match call {
            "settle" => {
                w.db.metering_settle(
                    &w.tenants,
                    &w.service,
                    w.organization_id,
                    "metering:never:hold",
                    None,
                    &event_usage(1),
                    D,
                )
                .await
                .map(|_| ())
            }
            _ => {
                w.db.metering_release(
                    &w.tenants,
                    &w.service,
                    w.organization_id,
                    "metering:never:hold",
                    D,
                )
                .await
                .map(|_| ())
            }
        };
        assert!(matches!(result, Err(WalletError::NotFound(_))), "{call}");
    }
}

#[tokio::test]
async fn a_one_shot_settlement_charges_and_replays() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    let outcome =
        w.db.metering_settle_once(
            &w.tenants,
            &w.service,
            w.organization_id,
            "once-1",
            &w.event(70),
            D,
        )
        .await
        .unwrap();
    assert_eq!(outcome.kind, SettlementKind::Usage);
    assert_eq!(outcome.charged_minor, 70);

    // The replay answers the committed settlement — no second charge.
    let replay =
        w.db.metering_settle_once(
            &w.tenants,
            &w.service,
            w.organization_id,
            "once-1",
            &w.event(70),
            D,
        )
        .await
        .unwrap();
    assert_eq!(replay.charged_minor, 70);
    assert_eq!(replay.settlement_id, outcome.settlement_id);

    // And the wallet moved once, by the charge.
    let wallet = w.tenants.get(&w.tenant_id).await.unwrap();
    assert_eq!(wallet.settled().await.unwrap(), 100 * ONE - 70);
}

#[tokio::test]
async fn the_description_of_a_metered_settlement_is_a_v6_record() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    let outcome =
        w.db.metering_settle_once(
            &w.tenants,
            &w.service,
            w.organization_id,
            "v6-1",
            &MeteredEvent {
                channel: w.channel.clone(),
                billable_code: w.code.clone(),
                event_id: Some("evt-42".to_owned()),
                usage: event_usage(25),
            },
            D,
        )
        .await
        .unwrap();
    assert_eq!(outcome.charged_minor, 25);
    let wallet = w.tenants.get(&w.tenant_id).await.unwrap();
    let record = wallet
        .settled_record("metering:v6-1:hold")
        .await
        .unwrap()
        .expect("the settlement committed");
    assert_eq!(record.kind, SettlementKind::Usage);
    assert_eq!(record.charged, 25);
    assert_eq!(record.price_version, 1);
    assert_eq!(record.upstream_attempts, 0);
    // The reporting credential's name is on the record — who wrote this charge.
    assert_eq!(record.service.as_deref(), w.service.name.as_deref());
    assert_eq!(record.request, "evt-42");
    assert_eq!(record.model, w.code);
}

// ── the refusals ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn usage_without_an_event_type_is_refused() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    let mut event = w.event(10);
    event.usage.event_type = None;
    let refused =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "no-et",
            &event,
            D,
        )
        .await;
    assert!(matches!(refused, Err(WalletError::InvalidInput(_))));
}

#[tokio::test]
async fn a_dimension_the_price_cannot_bill_is_refused() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    // Tool calls are a dimension no price set carries: declared usage that the
    // price cannot bill is refused outright — the service declares the usage,
    // so it can fix the request; it is not billed at zero.
    let mut event = w.event(10);
    event.usage.tool_calls = 1;
    let refused =
        w.db.metering_hold(&w.tenants, &w.service, w.organization_id, "unp", &event, D)
            .await;
    assert!(matches!(refused, Err(WalletError::InvalidInput(_))));

    // A zero rate is a rate, though: this event price's output side is free,
    // so declared output prices at zero rather than refusing — the same
    // semantics a chat price's free output carries.
    let mut free_output = w.event(10);
    free_output.usage.output_tokens = 5;
    let hold =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "fout",
            &free_output,
            D,
        )
        .await
        .unwrap();
    assert_eq!(hold.freeze_minor, 10);

    // And a free event prices to zero — a hold over nothing is refused.
    let free =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "zero",
            &w.event(0),
            D,
        )
        .await;
    assert!(matches!(free, Err(WalletError::InvalidInput(_))));
}

#[tokio::test]
async fn an_unknown_code_or_channel_is_refused() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    for (channel, code) in [
        (w.channel.clone(), fresh("nope")),
        (fresh("nochan"), w.code.clone()),
    ] {
        let event = MeteredEvent {
            channel,
            billable_code: code,
            event_id: None,
            usage: event_usage(1),
        };
        let refused =
            w.db.metering_hold(&w.tenants, &w.service, w.organization_id, "unk", &event, D)
                .await;
        assert!(matches!(refused, Err(WalletError::InvalidInput(_))));
    }
}

#[tokio::test]
async fn a_non_event_price_does_not_resolve() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    // The same code priced as `chat` serves the gateway, not metering.
    let chat = Price {
        input_price_per_million: ONE,
        output_price_per_million: ONE,
        max_output_tokens: 4096,
        cache_read_price_per_million: None,
        cache_write_5m_price_per_million: None,
        cache_write_1h_price_per_million: None,
        reasoning_price_per_million: None,
        cost_per_request: None,
        upstream: None,
        mode: BillingMode::Chat,
        rules: vec![],
    };
    let chat_code = fresh("chat.only");
    w.db.append_price(&w.channel, &chat_code, chat, 100)
        .await
        .unwrap();
    let event = MeteredEvent {
        channel: w.channel.clone(),
        billable_code: chat_code,
        event_id: None,
        usage: event_usage(10),
    };
    let refused =
        w.db.metering_hold(&w.tenants, &w.service, w.organization_id, "chat", &event, D)
            .await;
    assert!(matches!(refused, Err(WalletError::InvalidInput(_))));
}

#[tokio::test]
async fn an_insolvent_organization_is_refused() {
    let url = db_or_skip!();
    let w = world(&url, 0).await;
    let refused =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "poor",
            &w.event(10),
            D,
        )
        .await;
    assert!(matches!(refused, Err(WalletError::InsufficientFunds)));
}

#[tokio::test]
async fn a_suspended_organization_cannot_hold() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    w.db.set_suspended(w.organization_id, true)
        .await
        .expect("suspends");
    let refused =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            w.organization_id,
            "sus",
            &w.event(10),
            D,
        )
        .await;
    assert!(matches!(refused, Err(WalletError::Forbidden(_))));
    // The refused hold left nothing for the sweeper to watch.
    assert!(w.db.open_hold("metering:sus:hold").await.unwrap().is_none());
}

#[tokio::test]
async fn an_unknown_organization_is_not_found() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    let refused =
        w.db.metering_hold(
            &w.tenants,
            &w.service,
            Uuid::new_v4(),
            "ghost",
            &w.event(10),
            D,
        )
        .await;
    assert!(matches!(
        refused,
        Err(WalletError::NotFound(_)) | Err(WalletError::InvalidInput(_))
    ));
}

// ── the recorded description verifies ────────────────────────────────────────

#[tokio::test]
async fn the_v6_description_recomputes_and_a_tampered_one_does_not() {
    let url = db_or_skip!();
    let w = world(&url, 100 * ONE).await;
    w.db.metering_settle_once(
        &w.tenants,
        &w.service,
        w.organization_id,
        "ver-1",
        &w.event(25),
        D,
    )
    .await
    .unwrap();
    // The settlement entry's description as the ledger stored it — the
    // derivable entry id names it directly.
    let description: String = sqlx::query_scalar(&format!(
        "SELECT description FROM ledger_{}.entries WHERE entry_id = $1",
        w.tenant_id
    ))
    .bind(
        *oxsum_core::entry_id_for(&oxsum_core::settlement_key_for("metering:ver-1:hold")).as_uuid(),
    )
    .fetch_one(w.db.pool())
    .await
    .expect("the settlement entry is there");
    // The description parses as a settlement and its charge recomputes.
    assert!(SettlementRecord::parse(&description).is_some());
    assert_eq!(verify_charge(&description), ChargeCheck::Recomputed);
    // A tampered charge fails the recompute.
    let tampered = description.replacen("\"charged\":25", "\"charged\":26", 1);
    assert_eq!(verify_charge(&tampered), ChargeCheck::Mismatch);
}
