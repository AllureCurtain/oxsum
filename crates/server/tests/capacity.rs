//! Reproducible D09 load on a real listener and scratch PostgreSQL (#685).
//! Explicitly ignored: run via scripts/soak.sh, never on every PR.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use oxsum_core::{Db, KeyConstraints, NewUser, Price, SecretKey, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::Row;
use tokio::task::JoinSet;

const FUNDING: i64 = 1_000_000;
const CHARGE: i64 = 7;

fn knob(name: &str, default: usize, maximum: usize) -> usize {
    let value = std::env::var(name).map_or(default, |v| v.parse().expect("integer load parameter"));
    assert!(
        (1..=maximum).contains(&value),
        "{name} outside 1..={maximum}"
    );
    value
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    assert!(!sorted.is_empty() && (1..=100).contains(&percent));
    sorted[(sorted.len() * percent).div_ceil(100) - 1]
}

#[test]
fn latency_percentiles_use_nearest_rank() {
    let values: Vec<_> = (1..=100).collect();
    assert_eq!(percentile(&values, 50), 50);
    assert_eq!(percentile(&values, 95), 95);
    assert_eq!(percentile(&values, 99), 99);
    assert_eq!(percentile(&[7], 99), 7);
    assert_eq!(percentile(&[2, 4, 9], 50), 4);
}

#[derive(Clone)]
struct Upstream {
    delay: Duration,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

async fn stream(
    State(state): State<Upstream>,
    Json(_body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.peak.fetch_max(active, Ordering::SeqCst);
    let body = Body::from_stream(async_stream::stream! {
        yield Ok::<_, std::convert::Infallible>(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\n"));
        tokio::time::sleep(state.delay).await;
        state.active.fetch_sub(1, Ordering::SeqCst);
        yield Ok(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":6,\"completion_tokens\":1,\"total_tokens\":7}}\n\ndata: [DONE]\n\n"));
    });
    ([("content-type", "text/event-stream")], body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit sustained capacity measurement; scripts/soak.sh provisions an isolated database"]
async fn gateway_capacity_and_accounting() {
    let customers = knob("SOAK_CUSTOMERS", 10, 10_000);
    let keys_each = knob("SOAK_KEYS_PER_CUSTOMER", 2, 100);
    let concurrency = knob("SOAK_CONCURRENCY", 5, 1_000);
    let seconds = knob("SOAK_SECONDS", 10, 3_600);
    let rate = knob("SOAK_RATE", 10, 10_000);
    let stream_ms = knob("SOAK_STREAM_MS", 100, 30_000);
    let pool_size = knob("SOAK_POOL", 30, 80);
    let url = oxsum_testkit::suite_db_url("capacity")
        .expect("DATABASE_URL is required; never skip a requested measurement");
    let db = Db::connect(&url, pool_size as u32).await.unwrap();
    db.migrate().await.unwrap();
    let tenants = Tenants::new(db.pool().clone());
    let secret = SecretKey::from_bytes([37; 32]);
    let upstream = Upstream {
        delay: Duration::from_millis(stream_ms as u64),
        active: Arc::default(),
        peak: Arc::default(),
    };
    let stub = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_url = format!("http://{}/v1", stub.local_addr().unwrap());
    let stub_app = Router::new()
        .route("/v1/chat/completions", post(stream))
        .with_state(upstream.clone());
    let stub_task = tokio::spawn(async move { axum::serve(stub, stub_app).await.unwrap() });
    db.set_channel_pool_policy(
        "capacity",
        &stub_url,
        Some(&["isolated-stub-key".into()]),
        "openai",
        &secret,
        Some(&oxsum_core::ProviderDestinationPolicy {
            network: oxsum_core::ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }),
    )
    .await
    .unwrap();
    let price: Price = serde_json::from_value(json!({"inputPricePerMillion":1_000_000,"outputPricePerMillion":1_000_000,"maxOutputTokens":8})).unwrap();
    db.append_price("capacity", "capacity-chat", price, 100)
        .await
        .unwrap();
    let config = Config::new(Signup::Invite, None).with_secret(secret);
    oxsum_server::prepare(&db, &config).await.unwrap();

    // One fixture identity owns independent customer billing accounts. This
    // avoids measuring password hashing during the load; every organization,
    // key, ledger and opening credit is created through production domain APIs.
    let owner = db
        .register(NewUser {
            email: "capacity-owner@example.test".into(),
            password: "isolated capacity password".into(),
            organization_name: Some("Capacity 0".into()),
        })
        .await
        .unwrap();
    let mut accounts = Vec::with_capacity(customers);
    accounts.push(owner.organization.clone());
    for i in 1..customers {
        accounts.push(
            db.create_team_organization(owner.user.id, &format!("Capacity {i}"))
                .await
                .unwrap(),
        );
    }
    let preparation = Instant::now();
    let mut provisioning = JoinSet::new();
    let mut credentials = Vec::with_capacity(customers * keys_each);
    let mut wallets = HashMap::new();
    for (index, org) in accounts.into_iter().enumerate() {
        let db = db.clone();
        let tenants = tenants.clone();
        let first = if index == 0 {
            Some(owner.api_key.secret.clone())
        } else {
            None
        };
        provisioning.spawn(async move {
            let wallet = tenants.get(&org.tenant_id).await.unwrap();
            wallet
                .top_up(
                    "capacity-opening",
                    FUNDING,
                    time::OffsetDateTime::now_utc().date(),
                )
                .await
                .unwrap();
            let mut keys = Vec::new();
            if let Some(first) = first {
                keys.push(first);
            }
            while keys.len() < keys_each {
                keys.push(
                    db.create_key(
                        org.id,
                        Some(format!("capacity-{}", keys.len())),
                        None,
                        None,
                        KeyConstraints::default(),
                    )
                    .await
                    .unwrap()
                    .secret,
                );
            }
            (org, wallet, keys)
        });
        if provisioning.len() >= 4 || index + 1 == customers {
            while let Some(result) = provisioning.join_next().await {
                let (org, wallet, keys) = result.unwrap();
                credentials.extend(keys.into_iter().map(|key| (org.id, key)));
                wallets.insert(org.id, wallet);
            }
        }
        if (index + 1) % 100 == 0 {
            eprintln!("provisioned {}/{customers} customers", index + 1);
        }
    }
    assert_eq!(credentials.len(), customers * keys_each);
    let schema_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'ledger_%'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(schema_count, customers as i64);
    let key_count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.api_keys")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(key_count, (customers * keys_each) as i64);
    let provisioning_seconds = preparation.elapsed().as_secs_f64();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let app = oxsum_server::app(db.clone(), config);
    let server_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap();
    let credentials = Arc::new(credentials);
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    let mut samples = Vec::new();
    let mut expected = HashMap::<uuid::Uuid, usize>::new();
    let mut submitted = 0usize;
    let mut tick = tokio::time::interval(Duration::from_nanos(1_000_000_000 / rate as u64));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while started.elapsed() < Duration::from_secs(seconds as u64) {
        tick.tick().await;
        if started.elapsed() >= Duration::from_secs(seconds as u64) {
            break;
        }
        if tasks.len() >= concurrency {
            samples.push(tasks.join_next().await.unwrap().unwrap());
        }
        let index = submitted;
        let (org, key) =
            credentials[(index % customers) * keys_each + (index / customers) % keys_each].clone();
        *expected.entry(org).or_default() += 1;
        submitted += 1;
        let endpoint = endpoint.clone();
        let client = client.clone();
        tasks.spawn(async move {
            let start = Instant::now();
            let mut response = client.post(endpoint).bearer_auth(key)
                .header("idempotency-key", format!("capacity-{index}"))
                .json(&json!({"model":"capacity-chat","messages":[{"role":"user","content":format!("Say OK {index}")}],"max_tokens":8,"stream":true}))
                .send().await.unwrap();
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            let mut first = None;
            while let Some(chunk) = response.chunk().await.unwrap() {
                first.get_or_insert(start.elapsed().as_micros() as u64);
                bytes.extend_from_slice(&chunk);
            }
            (status, first.unwrap_or(0), start.elapsed().as_micros() as u64, bytes.ends_with(b"data: [DONE]\n\n"))
        });
    }
    let admitted_seconds = started.elapsed().as_secs_f64();
    while let Some(result) = tasks.join_next().await {
        samples.push(result.unwrap());
    }
    let elapsed_seconds = started.elapsed().as_secs_f64();
    let failures = samples
        .iter()
        .filter(|(status, _, _, done)| *status != 200 || !done)
        .count();
    // Wait only for the gateway's final settlement task; do not run a sweeper
    // that could conceal a leaked hold by forcibly settling it at zero.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let open: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.request_operations WHERE state NOT IN ('succeeded','failed','cancelled','timed_out')").fetch_one(db.pool()).await.unwrap();
        if open == 0 || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let operations = sqlx::query("SELECT count(*) AS n, count(*) FILTER (WHERE state='succeeded' AND charged_minor=7) AS correct, COALESCE(sum(charged_minor),0)::bigint AS charged FROM oxsum.request_operations").fetch_one(db.pool()).await.unwrap();
    let usage = sqlx::query("SELECT count(*) AS n, COALESCE(sum(charged_minor),0)::bigint AS charged FROM oxsum.usage_records").fetch_one(db.pool()).await.unwrap();
    let holds: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.open_holds")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut balance_drift = 0;
    let mut reserved = 0;
    for (org, wallet) in wallets {
        let spent = *expected.get(&org).unwrap_or(&0) as i64 * CHARGE;
        balance_drift += (wallet.available().await.unwrap() - (FUNDING - spent)).abs();
        reserved += wallet.reserved().await.unwrap();
    }
    let mut first: Vec<_> = samples.iter().map(|s| s.1).collect();
    first.sort_unstable();
    let mut total: Vec<_> = samples.iter().map(|s| s.2).collect();
    total.sort_unstable();
    let mut overhead: Vec<_> = total
        .iter()
        .map(|v| v.saturating_sub(stream_ms as u64 * 1_000))
        .collect();
    overhead.sort_unstable();
    let report = json!({"customer_accounts":customers,"keys":key_count,"ledger_schemas":schema_count,
        "concurrency_limit":concurrency,"peak_upstream_streams":upstream.peak.load(Ordering::SeqCst),
        "pool_connections":pool_size,"configured_rate":rate,"configured_seconds":seconds,"stub_stream_ms":stream_ms,
        "wallet_and_key_provisioning_seconds":provisioning_seconds,
        "admission_seconds":admitted_seconds,"elapsed_seconds_including_drain":elapsed_seconds,
        "requests":submitted,"failures":failures,"admitted_per_second":submitted as f64 / admitted_seconds,
        "completed_per_second":submitted as f64 / elapsed_seconds,
        "latency_us":{"ttft_p50":percentile(&first,50),"ttft_p95":percentile(&first,95),"ttft_p99":percentile(&first,99),
            "total_p50":percentile(&total,50),"total_p95":percentile(&total,95),"total_p99":percentile(&total,99),
            "overhead_upper_bound_p95":percentile(&overhead,95)},
        "accounting":{"operations":operations.get::<i64,_>("n"),"correct_operations":operations.get::<i64,_>("correct"),
            "charged_minor":operations.get::<i64,_>("charged"),"usage_rows":usage.get::<i64,_>("n"),"usage_charged_minor":usage.get::<i64,_>("charged"),
            "expected_minor":submitted as i64 * CHARGE,"open_holds":holds,"reserved_minor":reserved,"absolute_balance_drift_minor":balance_drift}});
    println!(
        "CAPACITY_REPORT {}",
        serde_json::to_string(&report).unwrap()
    );
    if let Ok(path) = std::env::var("SOAK_REPORT") {
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    server_task.abort();
    stub_task.abort();
    assert_eq!(failures, 0, "unexpected response or incomplete stream");
    assert_eq!(operations.get::<i64, _>("n"), submitted as i64);
    assert_eq!(operations.get::<i64, _>("correct"), submitted as i64);
    assert_eq!(usage.get::<i64, _>("n"), submitted as i64);
    assert_eq!(
        operations.get::<i64, _>("charged"),
        submitted as i64 * CHARGE
    );
    assert_eq!(usage.get::<i64, _>("charged"), submitted as i64 * CHARGE);
    assert_eq!((holds, reserved, balance_drift), (0, 0, 0));
}
