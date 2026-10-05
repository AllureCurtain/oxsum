//! The billing-correctness suite (roadmap P1-7, issue #114).
//!
//! Unit tests check the cases somebody thought of. This file pins what billing
//! promises regardless of case:
//!
//! - **Golden fixtures** — canonical settlement descriptions down to the byte.
//!   A wire-format drift (a renamed field, a reordered line, a dropped `v`)
//!   fails loudly here rather than silently breaking `verify_charge` for every
//!   old bill.
//! - **Identities** — proptest drives arbitrary valid `UsageRecord` × `Price`
//!   pairs through `itemize`, `total_minor` and `upstream_cost` and asserts
//!   what must hold of all of them: the lines account for the whole usage, the
//!   charge never exceeds the freeze, subsets never price twice, and arithmetic
//!   overflows refuse rather than wrap.
//! - **Proof round-trips** — hold → settle → `receipt_proof` → `verify_bundle`
//!   (inclusion) → `verify_charge` (recompute) for every settlement kind, on a
//!   real wallet over PostgreSQL.
//! - **The reconciler's surface** — a usage row names the settlement entry it
//!   describes, and its `charged`/`freeze`/`kind` agree with the record the
//!   entry's description parses back into. That agreement is what a future
//!   reconciler (P4-1) compares upstream bills against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    BillLine, BillingMode, ChargeCheck, Db, Price, PriceRule, RuleMatch, Settlement,
    SettlementKind, SettlementRecord, UpstreamPrices, UsageRecord, UsageRow, Wallet, entry_id_for,
    settlement_key_for, verify_bundle, verify_charge,
};
use proptest::prelude::*;
use sqlx::PgPool;
use sqlx::Row;
use time::macros::date;

const D: time::Date = date!(2026 - 10 - 01);

// ── golden fixtures ──────────────────────────────────────────────────────────

/// The price every golden turn prices under: input at 1e6, output at 2e6, the
/// cached part at 1e5, reasoning at 8e6, a 100-minor flat fee.
fn golden_price() -> Price {
    serde_json::from_value(serde_json::json!({
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 2_000_000,
        "maxOutputTokens": 8_000,
        "cacheReadPricePerMillion": 100_000,
        "reasoningPricePerMillion": 8_000_000,
        "costPerRequest": 100,
    }))
    .expect("the fixture's own price parses")
}

/// The settlement description a turn writes, with the charge computed the way
/// the relay computes it: itemize, sum, cap at the freeze.
fn golden(
    request: &str,
    kind: SettlementKind,
    usage: &UsageRecord,
    matched_rule: Option<&RuleMatch>,
    freeze: i64,
) -> String {
    let billable = !matches!(
        kind,
        SettlementKind::UpstreamError | SettlementKind::UpstreamUnreachable
    );
    let itemized = golden_price().itemize(usage, billable).unwrap();
    let cost = itemized.total_minor().unwrap();
    let charged = match kind {
        SettlementKind::Capped | SettlementKind::Unpriced => cost.min(freeze),
        _ => cost,
    };
    Settlement {
        request,
        channel: "deepseek",
        model: "ds-chat",
        price_version: 7,
        kind,
        usage,
        lines: &itemized.lines,
        matched_rule,
        charged,
        freeze,
    }
    .description()
    .expect("the fixture serializes")
}

/// The v3 wire shape is pinned down to the byte: a field renamed, reordered or
/// spelled differently fails this, and so would every old bill's recompute.
#[test]
fn settlement_descriptions_are_byte_stable() {
    let cases: &[(SettlementKind, UsageRecord, &str)] = &[
        // An ordinary turn: sparse usage, two lines, the flat fee priced in.
        (
            SettlementKind::Usage,
            UsageRecord::tokens(10, 2).unwrap(),
            r#"{"v":3,"request":"req-golden-usage","channel":"deepseek","model":"ds-chat","priceVersion":7,"kind":"usage","usage":{"inputTokens":10,"outputTokens":2},"lines":[["input",10,1000000],["output",2,2000000],["request",1,100000000]],"charged":114,"freeze":900000}"#,
        ),
        // An unpriced turn: the audio tokens sit in `usage` (they are metered
        // fact) but reach no line — nothing may claim them.
        (
            SettlementKind::Unpriced,
            UsageRecord {
                audio_input_tokens: 300,
                ..UsageRecord::tokens(10, 2).unwrap()
            },
            r#"{"v":3,"request":"req-golden-unpriced","channel":"deepseek","model":"ds-chat","priceVersion":7,"kind":"unpriced","usage":{"inputTokens":10,"outputTokens":2,"audioInputTokens":300},"lines":[["input",10,1000000],["output",2,2000000],["request",1,100000000]],"charged":114,"freeze":900000}"#,
        ),
    ];
    for (kind, usage, expected) in cases {
        let request = serde_json::from_str::<serde_json::Value>(expected).unwrap()["request"]
            .as_str()
            .unwrap()
            .to_owned();
        let description = golden(&request, *kind, usage, None, 900_000);
        assert_eq!(&description, expected, "kind {kind:?}");
        // Every canonical description still parses and recomputes: the pin is
        // on the writer's bytes, the reader and the verifier agree with them.
        let record = SettlementRecord::parse(&description).expect("the fixture parses");
        assert_eq!(record.kind, *kind);
        assert_eq!(verify_charge(&description), ChargeCheck::Recomputed);
    }

    // A swept turn's description is the sweeper's own shape — two zero lines at
    // the version's base rates, no request line at all: it ran nothing and owes
    // nothing, and a retried sweep must reproduce exactly these bytes.
    let swept = Settlement {
        request: "req-golden-swept",
        channel: "deepseek",
        model: "ds-chat",
        price_version: 7,
        kind: SettlementKind::Swept,
        usage: &UsageRecord::default(),
        lines: &[
            BillLine {
                item: "input".into(),
                units: 0,
                price_per_m: 1_000_000,
            },
            BillLine {
                item: "output".into(),
                units: 0,
                price_per_m: 2_000_000,
            },
        ],
        matched_rule: None,
        charged: 0,
        freeze: 900_000,
    }
    .description()
    .expect("the fixture serializes");
    assert_eq!(
        swept,
        r#"{"v":3,"request":"req-golden-swept","channel":"deepseek","model":"ds-chat","priceVersion":7,"kind":"swept","usage":{},"lines":[["input",0,1000000],["output",0,2000000]],"charged":0,"freeze":900000}"#
    );
    assert_eq!(verify_charge(&swept), ChargeCheck::Recomputed);
}

/// A rule-priced turn names its match inside the hashed bytes — the audit
/// answer to "which rule priced this" is part of the credential, not a side
/// note.
#[test]
fn a_rule_priced_description_names_its_match() {
    let usage = UsageRecord {
        cached_tokens: 40,
        reasoning_tokens: 5,
        service_tier: Some("priority".into()),
        ..UsageRecord::tokens(100, 30).unwrap()
    };
    let rule = RuleMatch {
        service_tier: Some("priority".into()),
        ..RuleMatch::default()
    };
    // The priority set: input 5e5, output 2.5e5, cache 1e5, reasoning 8e6,
    // 100 flat — written directly as lines, the shape itemize emits.
    let lines = [
        BillLine {
            item: "input".into(),
            units: 60,
            price_per_m: 500_000,
        },
        BillLine {
            item: "cache_read".into(),
            units: 40,
            price_per_m: 100_000,
        },
        BillLine {
            item: "output".into(),
            units: 25,
            price_per_m: 250_000,
        },
        BillLine {
            item: "reasoning".into(),
            units: 5,
            price_per_m: 8_000_000,
        },
        BillLine {
            item: "request".into(),
            units: 1,
            price_per_m: 100_000_000,
        },
    ];
    // ceil(60·5e5 + 40·1e5 + 25·2.5e5 + 5·8e6 + 1e8)/1e6 = ceil(180.25) = 181.
    let description = Settlement {
        request: "req-golden-rule",
        channel: "deepseek",
        model: "ds-chat",
        price_version: 7,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &lines,
        matched_rule: Some(&rule),
        charged: 181,
        freeze: 900_000,
    }
    .description()
    .expect("the fixture serializes");
    assert_eq!(
        description,
        r#"{"v":3,"request":"req-golden-rule","channel":"deepseek","model":"ds-chat","priceVersion":7,"kind":"usage","usage":{"inputTokens":100,"outputTokens":30,"cachedTokens":40,"reasoningTokens":5,"serviceTier":"priority"},"lines":[["input",60,500000],["cache_read",40,100000],["output",25,250000],["reasoning",5,8000000],["request",1,100000000]],"matchedRule":{"serviceTier":"priority"},"charged":181,"freeze":900000}"#
    );
    // The description fits the ledger's 512-character limit even itemized.
    assert!(description.len() <= 512, "{}", description.len());
    assert_eq!(verify_charge(&description), ChargeCheck::Recomputed);
    let record = SettlementRecord::parse(&description).expect("the fixture parses");
    assert_eq!(
        record.matched_rule.unwrap().service_tier.as_deref(),
        Some("priority")
    );
}

// ── identities ───────────────────────────────────────────────────────────────

/// An arbitrary *valid* usage record: the subsets always fit their totals,
/// which is the invariant `validate` checks and `itemize` relies on.
fn arb_usage() -> impl Strategy<Value = UsageRecord> {
    (0i64..=100_000, 0i64..=100_000).prop_flat_map(|(input, output)| {
        (Just(input), Just(output), 0..=input, 0..=output)
            .prop_flat_map(|(input, output, cached, reasoning)| {
                (
                    Just(input),
                    Just(output),
                    Just(cached),
                    Just(reasoning),
                    0..=(input - cached),
                    0i64..=4,
                    prop::option::of(0i64..=1_000),
                    prop::option::of(Just("priority".to_owned())),
                )
            })
            .prop_map(
                |(input, output, cached, reasoning, cw5m, tool_calls, audio, tier)| UsageRecord {
                    input_tokens: input,
                    output_tokens: output,
                    cached_tokens: cached,
                    cache_write_5m_tokens: cw5m,
                    reasoning_tokens: reasoning,
                    tool_calls,
                    audio_input_tokens: audio.unwrap_or(0),
                    service_tier: tier,
                    ..UsageRecord::default()
                },
            )
    })
}

/// An arbitrary price: base rates, optional dimensions, an optional upstream
/// block, and zero or one service-tier rule — one rule is never ambiguous.
fn arb_price() -> impl Strategy<Value = Price> {
    (
        0i64..=4_000_000,
        0i64..=8_000_000,
        1i64..=8_000,
        prop::option::of(0i64..=4_000_000),
        prop::option::of(0i64..=32_000_000),
        prop::option::of(0i64..=5_000),
        prop::bool::ANY,
        prop::option::of(0i64..=4_000_000),
    )
        .prop_map(
            |(input, output, max_out, cache, reasoning, flat, has_upstream, rule_input)| Price {
                input_price_per_million: input,
                output_price_per_million: output,
                max_output_tokens: max_out,
                cache_read_price_per_million: cache,
                reasoning_price_per_million: reasoning,
                cost_per_request: flat,
                upstream: has_upstream.then(|| UpstreamPrices {
                    input_price_per_million: Some(input / 2),
                    output_price_per_million: Some(output / 2),
                    ..UpstreamPrices::default()
                }),
                mode: BillingMode::Chat,
                rules: rule_input
                    .map(|rule_input| {
                        vec![PriceRule {
                            cond: RuleMatch {
                                service_tier: Some("priority".into()),
                                ..RuleMatch::default()
                            },
                            price: oxsum_core::PriceSet {
                                input_price_per_million: rule_input,
                                output_price_per_million: output,
                                max_output_tokens: max_out,
                                cache_read_price_per_million: cache,
                                cache_write_5m_price_per_million: None,
                                cache_write_1h_price_per_million: None,
                                reasoning_price_per_million: reasoning,
                                cost_per_request: flat,
                                upstream: None,
                            },
                        }]
                    })
                    .unwrap_or_default(),
                cache_write_5m_price_per_million: None,
                cache_write_1h_price_per_million: None,
            },
        )
}

/// The item names that live on the input side of a decomposition: their units
/// must sum back to `input_tokens`, or something was billed twice or not at all.
const INPUT_ITEMS: &[&str] = &["input", "cache_read", "cache_write_5m", "cache_write_1h"];
const OUTPUT_ITEMS: &[&str] = &["output", "reasoning"];

proptest! {
    /// Whatever the usage and whatever the price, the lines account for the
    /// whole usage exactly once: the input side's units sum to `input_tokens`,
    /// the output side's to `output_tokens`, and the flat fee counts at most
    /// once. Double counting a dimension is what this refuses.
    #[test]
    fn the_lines_account_for_the_whole_usage(
        usage in arb_usage(),
        price in arb_price(),
        billable in any::<bool>(),
    ) {
        price.validate().expect("the generated price validates");
        let itemized = price.itemize(&usage, billable).expect("the usage validates");
        let side = |items: &[&str]| -> i64 {
            itemized
                .lines
                .iter()
                .filter(|line| items.contains(&line.item.as_str()))
                .map(|line| line.units)
                .sum()
        };
        prop_assert_eq!(side(INPUT_ITEMS), usage.input_tokens);
        prop_assert_eq!(side(OUTPUT_ITEMS), usage.output_tokens);
        prop_assert!(itemized
            .lines
            .iter()
            .all(|line| INPUT_ITEMS.contains(&line.item.as_str())
                || OUTPUT_ITEMS.contains(&line.item.as_str())
                || line.item == "request"));
        if let Some(request) = itemized.lines.iter().find(|line| line.item == "request") {
            // The fee joins as units 1 when the turn ran, 0 when it did not.
            prop_assert_eq!(request.units, i64::from(billable));
        }
    }

    /// The charge the lines sum to is never negative, never panics, and the
    /// relay's rule — `min(cost, freeze)` — never charges above the freeze.
    /// Upstream cost is `Some` exactly when the resolved set tracks it, and is
    /// itself never negative and never above the customer total by fiat.
    #[test]
    fn the_charge_stays_under_the_freeze_and_upstream_stays_optional(
        usage in arb_usage(),
        price in arb_price(),
        freeze in 1i64..=10_000_000,
    ) {
        let itemized = price.itemize(&usage, true).expect("the usage validates");
        let cost = itemized.total_minor().expect("the sums fit, rates are bounded");
        prop_assert!(cost >= 0);
        let charged = cost.min(freeze);
        prop_assert!(charged <= freeze);
        prop_assert!(charged == cost || charged == freeze);

        match price.upstream_cost(&usage, true) {
            Ok(Some(upstream)) => prop_assert!(upstream >= 0),
            Ok(None) => {
                // `None` is only honest when the set the turn priced under
                // carries no `upstream` block.
                let rule_matched = usage.service_tier.as_deref() == Some("priority")
                    && !price.rules.is_empty();
                prop_assert!(!rule_matched || price.rules[0].price.upstream.is_none());
                if !rule_matched {
                    prop_assert!(price.upstream.is_none());
                }
            }
            Err(error) => prop_assert!(false, "bounded rates cannot overflow: {error}"),
        }
    }

    /// `unpriced_dimensions` is the fail-closed list: it names exactly the
    /// metered dimensions no set can bill, and the computable part still prices.
    #[test]
    fn unpriced_dimensions_are_exactly_the_dimensions_the_book_misses(
        usage in arb_usage(),
        price in arb_price(),
    ) {
        let unpriced = price.unpriced_dimensions(&usage);
        let mut expected = Vec::new();
        if usage.tool_calls > 0 {
            expected.push("toolCalls");
        }
        if usage.audio_input_tokens > 0 {
            expected.push("audioInputTokens");
        }
        prop_assert_eq!(unpriced, expected);
    }
}

// ── proof round-trips and the reconciler's surface ──────────────────────────

fn url() -> Option<String> {
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

async fn pool(url: &str) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL")
}

fn fresh(name: &str) -> String {
    format!("{name}_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// Every way a turn can end writes a settlement whose proof includes the entry
/// and whose description recomputes — the whole loop a user's verifier runs.
/// On top of that, the usage row written beside the entry agrees with the
/// record the description parses back into, field for field: that agreement is
/// what the margin view sums and what a reconciler compares.
#[tokio::test]
async fn every_settlement_kind_proves_and_recomputes() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let tenant = fresh("billing");
    let wallet = Wallet::open(pool.clone(), &tenant).await.unwrap();
    wallet.top_up("topup", 10_000_000, D).await.unwrap();

    // The price the turns settle under — flat fee and an `upstream` block, so
    // every tracked row carries both numbers the margin view sums.
    let price: Price = serde_json::from_value(serde_json::json!({
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 2_000_000,
        "maxOutputTokens": 8_000,
        "costPerRequest": 100,
        "upstream": {
            "inputPricePerMillion": 500_000,
            "outputPricePerMillion": 1_000_000,
        },
    }))
    .expect("the test's own price parses");

    // One case per kind the ledger knows, with the usage the kind settles on:
    // a priced turn, a local estimate, a cancelled stream, a turn priced over
    // the freeze, a swept hold, an unpriced turn, and the two upstream
    // failures that charge nothing and owe no flat fee.
    let cases: &[(SettlementKind, UsageRecord, i64)] = &[
        (
            SettlementKind::Usage,
            UsageRecord::tokens(10, 2).unwrap(),
            900_000,
        ),
        (
            SettlementKind::Estimated,
            UsageRecord::tokens(23, 11).unwrap(),
            900_000,
        ),
        (
            SettlementKind::ClientCancelled,
            UsageRecord::tokens(4, 2).unwrap(),
            900_000,
        ),
        (
            SettlementKind::Capped,
            UsageRecord::tokens(10_000, 10_000).unwrap(),
            100,
        ),
        (
            SettlementKind::Swept,
            UsageRecord::tokens(0, 0).unwrap(),
            900_000,
        ),
        (
            SettlementKind::Unpriced,
            UsageRecord {
                audio_input_tokens: 300,
                ..UsageRecord::tokens(10, 2).unwrap()
            },
            900_000,
        ),
        (
            SettlementKind::UpstreamError,
            UsageRecord::tokens(0, 0).unwrap(),
            900_000,
        ),
        (
            SettlementKind::UpstreamUnreachable,
            UsageRecord::tokens(0, 0).unwrap(),
            900_000,
        ),
    ];

    for (kind, usage, freeze) in cases {
        let hold_key = format!("hold-{}", fresh("kind"));
        let request = hold_key.clone();
        wallet.hold(&hold_key, "hold", *freeze, D).await.unwrap();

        // The charge the settling path would book for this turn. A swept turn is
        // the sweeper's own shape — two zero lines at the version's base rates,
        // no request line: it ran nothing.
        let billable = !matches!(
            kind,
            SettlementKind::UpstreamError
                | SettlementKind::UpstreamUnreachable
                | SettlementKind::Swept
        );
        let itemized = price.itemize(usage, billable).unwrap();
        let swept_lines;
        let (lines, charged) = if *kind == SettlementKind::Swept {
            swept_lines = [
                BillLine {
                    item: "input".into(),
                    units: 0,
                    price_per_m: price.input_price_per_million,
                },
                BillLine {
                    item: "output".into(),
                    units: 0,
                    price_per_m: price.output_price_per_million,
                },
            ];
            (&swept_lines[..], 0)
        } else {
            let cost = itemized.total_minor().unwrap();
            let charged = match kind {
                SettlementKind::Capped | SettlementKind::Unpriced => cost.min(*freeze),
                _ => cost,
            };
            (&itemized.lines[..], charged)
        };
        let description = Settlement {
            request: &request,
            channel: "chan-correctness",
            model: "model-correctness",
            price_version: 3,
            kind: *kind,
            usage,
            lines,
            matched_rule: None,
            charged,
            freeze: *freeze,
        }
        .description()
        .expect("the settlement serializes");
        let receipt = wallet
            .settle(&hold_key, &description, charged, D)
            .await
            .unwrap_or_else(|error| panic!("the {kind:?} turn settles: {error}"));

        // Round-trip: the entry is provable, and what it records recomputes.
        let entry_id = entry_id_for(&settlement_key_for(&hold_key));
        let bundle = wallet
            .receipt_proof(entry_id)
            .await
            .expect("the proof builds")
            .expect("the settlement entry exists");
        let json = serde_json::to_string(&bundle).expect("the bundle serializes");
        assert!(
            matches!(verify_bundle(&json, &receipt.content_hash), Ok(true)),
            "the {kind:?} bundle verifies"
        );
        assert_eq!(
            verify_charge(&description),
            ChargeCheck::Recomputed,
            "the {kind:?} description recomputes"
        );

        // The reconciler's surface: the usage row written beside the entry
        // names it, and the two agree on kind, charge and freeze.
        let record = SettlementRecord::parse(&description).expect("the description parses");
        let upstream_cost = price.upstream_cost(usage, billable).unwrap();
        db.record_usage(&UsageRow {
            request_id: request.clone(),
            tenant_id: tenant.clone(),
            key_id: None,
            model: record.model.clone(),
            channel: record.channel.clone(),
            price_version: record.price_version,
            kind: *kind,
            entry_id: *entry_id.as_uuid(),
            usage: usage.clone(),
            charged_minor: charged,
            freeze_minor: *freeze,
            upstream_cost_minor: upstream_cost,
        })
        .await
        .expect("the usage row is written");
        let row = sqlx::query(
            "SELECT kind, charged_minor, freeze_minor, entry_id, upstream_cost_minor \
             FROM oxsum.usage_records WHERE request_id = $1",
        )
        .bind(&request)
        .fetch_one(&pool)
        .await
        .expect("the usage row exists");
        assert_eq!(row.get::<String, _>("kind"), record.kind.as_str());
        assert_eq!(row.get::<i64, _>("charged_minor"), record.charged);
        assert_eq!(row.get::<i64, _>("freeze_minor"), record.freeze);
        assert_eq!(row.get::<uuid::Uuid, _>("entry_id"), *entry_id.as_uuid());
        assert_eq!(
            row.get::<Option<i64>, _>("upstream_cost_minor"),
            upstream_cost
        );
    }
}
