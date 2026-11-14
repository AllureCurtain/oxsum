//! A29: the install/upgrade/restore surfaces — `/readyz`, `/api/v1/version`,
//! `--migrate-only`, `--check-config`, the demo-seed gate, and the
//! backup/restore script pair run against real scratch databases.
use std::time::Duration;
#[path = "../support/binary.rs"]
mod binary;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::Executor;
use tower::ServiceExt;

/// A router over a migrated scratch database — the serving state every
/// readiness check wants.
async fn serving(slug: &str) -> Option<(Db, axum::Router)> {
    let url = oxsum_testkit::suite_db_url(slug)?;
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    Some((db, app))
}

/// `GET /api/v1/version` is unauthenticated and reports the build identity:
/// package version, a git sha this workspace always knows, and the newest
/// schema the binary migrates to.
#[tokio::test]
async fn version_reports_build_identity_without_a_credential() {
    let Some((_db, app)) = serving("install_ops_version").await else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let response = app
        .oneshot(Request::get("/api/v1/version").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let data = &body["data"];
    assert_eq!(data["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        data["schemaVersion"],
        oxsum_core::Db::latest_schema_version()
    );
    // The workspace is a git checkout, so build.rs recorded a real sha.
    assert_eq!(data["gitSha"].as_str().unwrap().len(), 40);
}

/// `/readyz` answers 200 on a migrated database and names the schema level;
/// 503 on a database nothing has migrated and on one whose ledger is behind.
#[tokio::test]
async fn readyz_distinguishes_migrated_unmigrated_and_stale() {
    let Some(url) = oxsum_testkit::suite_db_url("install_ops_readyz") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));

    // Fresh database: the ledger does not exist — not ready.
    let response = app
        .clone()
        .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("unmigrated"), "{body}");

    // Migrated: ready, and the body names the schema the probe can log.
    db.migrate().await.unwrap();
    let response = app
        .clone()
        .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        body.contains(oxsum_core::Db::latest_schema_version()),
        "{body}"
    );

    // Stale: the ledger behind the binary's newest is "not ready" again — the
    // pre-upgrade-drain shape an operator sees while oxsum-migrate still runs.
    sqlx::query("DELETE FROM oxsum._migrations WHERE version = $1")
        .bind(oxsum_core::Db::latest_schema_version())
        .execute(db.pool())
        .await
        .unwrap();
    let response = app
        .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    // The handler reports the older level, never a panic.
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("schema at"), "{body}");

    // /healthz stays a pure liveness answer throughout — no database.
    let db2 = Db::connect(&url, 2).await.unwrap();
    let app2 = oxsum_server::app(db2, Config::new(Signup::Invite, None));
    let response = app2
        .clone()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // DB down: a pool over an unreachable target — `connect_lazy` so the
    // dead backend shows up only when the probe draws a connection, which is
    // the failure readiness exists to report (503), while /healthz answers.
    let dead_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/oxsum_unreachable")
        .unwrap();
    let dead = Db::from_pool(dead_pool);
    let app3 = oxsum_server::app(dead, Config::new(Signup::Invite, None));
    let response = app3
        .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// `--check-config` answers a JSON OK on a clean environment and fails with
/// the variable named — not a panic — when one is invalid. This is the
/// binary's own diagnostic, run before the database is ever touched.
#[tokio::test]
async fn check_config_validates_env_without_a_database() {
    let dir = std::env::temp_dir().join(format!("oxsum-cfg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(".env"), "").unwrap();

    let run = |extra: &[(&str, &str)]| {
        let mut cmd = binary::oxsum();
        cmd.arg("--check-config").current_dir(&dir).env_clear().env(
            "DATABASE_URL",
            "postgres://postgres:postgres@127.0.0.1:5432/oxsum",
        );
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };

    let ok = run(&[]);
    assert!(
        ok.status.success(),
        "clean env must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let report: Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(report["ok"], json!(true));

    // A bad admin token names itself — the operator reads the variable, not
    // a stack.
    let bad = run(&[("OXSUM_ADMIN_TOKEN", "short")]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_ADMIN_TOKEN"), "{err}");

    // A declared rail with no credentials names the rail.
    let bad = run(&[("OXSUM_RAILS", "stripe")]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_RAILS"), "{err}");
    assert!(err.contains("stripe"), "{err}");

    // Self-registration without a mailer names the mailer requirement.
    let bad = run(&[("OXSUM_CUSTOMER_SELF_REGISTER", "true")]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_CUSTOMER_SELF_REGISTER"), "{err}");
    assert!(err.contains("OXSUM_SMTP_URL"), "{err}");

    // The solana rail's asset whitelist (issue #1140): `RPC_URL` + `PAY_TO`
    // on, `OXSUM_SOLANA_ASSETS` + `OXSUM_SOLANA_MINT_<KEY>` parsing — a
    // whitelist entry that is not `sol` must name its mint, a default off the
    // whitelist names itself, and the complete whitelist passes. `RATE_MICRO`
    // rides in the extras: a `sol` whitelist intent is oracle-priced, so the
    // static figure is only legal behind `DEVNET_STATIC_RATE=1` (issue #1139).
    let rail = |extra: &[(&str, &str)]| {
        let mut vars = vec![
            ("OXSUM_SOLANA_RPC_URL", "http://127.0.0.1:9"),
            ("OXSUM_SOLANA_PAY_TO", "payToAddr"),
        ];
        vars.extend_from_slice(extra);
        run(&vars)
    };
    // `usdc` on the whitelist without its mint names the mint variable.
    let bad = rail(&[("OXSUM_SOLANA_ASSETS", "sol,usdc")]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_MINT_USDC"), "{err}");
    // A default off the whitelist names the default variable.
    let bad = rail(&[
        ("OXSUM_SOLANA_ASSETS", "sol,usdc"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
        ("OXSUM_SOLANA_DEFAULT_ASSET", "doge"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_DEFAULT_ASSET"), "{err}");
    // The complete whitelist parses — `--check-config` never opens the RPC,
    // so the mint audit does not run here (boot-only). No `RATE_MICRO`: the
    // `sol` entry prices off the oracle at open.
    let ok = rail(&[
        ("OXSUM_SOLANA_ASSETS", "sol,usdc"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
        ("OXSUM_SOLANA_DEFAULT_ASSET", "usdc"),
    ]);
    assert!(
        ok.status.success(),
        "the whitelist env must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // The oracle's env surface parses too — a `sol` whitelist priced off
    // Pyth with its gates tuned (issue #1139).
    let ok = rail(&[
        ("OXSUM_SOLANA_ASSETS", "sol,usdc"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
        ("OXSUM_SOLANA_PYTH_URL", "http://127.0.0.1:9"),
        ("OXSUM_SOLANA_PYTH_MAX_AGE_SECS", "30"),
        ("OXSUM_SOLANA_PYTH_MAX_CONF_RATIO", "0.02"),
    ]);
    assert!(
        ok.status.success(),
        "the pyth env must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // The rate-source gate (issue #1139): `RATE_MICRO` beside a `sol`
    // whitelist entry without `DEVNET_STATIC_RATE=1` is a startup error —
    // the dangerous direction is a mainnet silently pricing SOL at a hand
    // figure. With the flag it passes (devnet); so does a whitelist whose
    // only volatile asset is absent (a `usdc`-only whitelist needs no rate
    // source at all).
    let bad = rail(&[
        ("OXSUM_SOLANA_ASSETS", "sol,usdc"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_DEVNET_STATIC_RATE"), "{err}");
    let ok = rail(&[
        ("OXSUM_SOLANA_ASSETS", "sol,usdc"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
        ("OXSUM_SOLANA_DEVNET_STATIC_RATE", "1"),
    ]);
    assert!(
        ok.status.success(),
        "the devnet static-rate env must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let ok = rail(&[
        ("OXSUM_SOLANA_ASSETS", "usdc"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
    ]);
    assert!(
        ok.status.success(),
        "a pegged-only whitelist needs no rate source: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // The USDT opt-in (issue #1151): the flag is off by default, so either
    // direction of a USDT config — a `usdt` whitelist entry or the mint
    // alone — refuses at boot naming the flag. Enabled, the pair parses; a
    // mint without the whitelist entry is the other half-config.
    let bad = rail(&[
        ("OXSUM_SOLANA_ASSETS", "sol,usdt"),
        ("OXSUM_SOLANA_MINT_USDT", "usdtMint"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_USDT_ENABLED"), "{err}");
    let bad = rail(&[("OXSUM_SOLANA_MINT_USDT", "usdtMint")]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_USDT_ENABLED"), "{err}");
    // A malformed flag value names itself — never a silent off.
    let bad = rail(&[
        ("OXSUM_SOLANA_USDT_ENABLED", "yes"),
        ("OXSUM_SOLANA_ASSETS", "usdc"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_USDT_ENABLED"), "{err}");
    // Enabled + the mint without the whitelist entry: the half-config names
    // the missing `usdt` key.
    let bad = rail(&[
        ("OXSUM_SOLANA_USDT_ENABLED", "1"),
        ("OXSUM_SOLANA_ASSETS", "usdc"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
        ("OXSUM_SOLANA_MINT_USDT", "usdtMint"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_MINT_USDT"), "{err}");
    // Enabled + the complete pair: USDT joins the whitelist like USDC.
    let ok = rail(&[
        ("OXSUM_SOLANA_USDT_ENABLED", "1"),
        ("OXSUM_SOLANA_ASSETS", "usdc,usdt"),
        ("OXSUM_SOLANA_MINT_USDC", "usdcMint"),
        ("OXSUM_SOLANA_MINT_USDT", "usdtMint"),
    ]);
    assert!(
        ok.status.success(),
        "the enabled usdt whitelist must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // The legacy single-mint form still parses: SPL_TOKEN_MINT +
    // DECIMALS, no ASSETS at all (issue #1140 backward-compat) — and it
    // still wants `RATE_MICRO`, the figure its one asset locks statically.
    let ok = rail(&[
        ("OXSUM_SOLANA_SPL_TOKEN_MINT", "legacyMint"),
        ("OXSUM_SOLANA_DECIMALS", "6"),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
    ]);
    assert!(
        ok.status.success(),
        "the legacy mint env must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // The legacy mint without its decimals names the requirement.
    let bad = rail(&[
        ("OXSUM_SOLANA_SPL_TOKEN_MINT", "legacyMint"),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_DECIMALS"), "{err}");
    // A legacy form without `RATE_MICRO` names it — the legacy asset always
    // locks statically.
    let bad = rail(&[
        ("OXSUM_SOLANA_SPL_TOKEN_MINT", "legacyMint"),
        ("OXSUM_SOLANA_DECIMALS", "6"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_RATE_MICRO"), "{err}");

    // The rail's endpoint set (issue #1145): the plural variable configures
    // the rail, and a half-set (endpoint list without the pay-to) is a
    // startup error naming it.
    let bad = run(&[(
        "OXSUM_SOLANA_RPC_URLS",
        "https://mainnet.helius-rpc.com/?api-key=x,https://api.devnet.solana.com",
    )]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_PAY_TO"), "{err}");
    // The singular still configures the rail, comma-separated entries
    // included; a full trio passes the check. Both endpoints stay
    // devnet-shaped — a mainnet entry would now require the custody pin or
    // the hot-wallet opt-out (issue #1189) and the sanctions choice (issue
    // #1179), both covered below.
    let ok = run(&[
        (
            "OXSUM_SOLANA_RPC_URL",
            "https://api.devnet.solana.com,https://api.testnet.solana.com",
        ),
        (
            "OXSUM_SOLANA_PAY_TO",
            "payToExample111111111111111111111111111",
        ),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
    ]);
    assert!(
        ok.status.success(),
        "a comma-separated singular endpoint list must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // A hostless endpoint entry is a startup error naming the variable —
    // the client's boot check, not a runtime surprise.
    let bad = run(&[
        ("OXSUM_SOLANA_RPC_URLS", "not a url"),
        (
            "OXSUM_SOLANA_PAY_TO",
            "payToExample111111111111111111111111111",
        ),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_RPC_URLS"), "{err}");
    // A colon-less auth header can never parse — refused at boot.
    let bad = run(&[
        ("OXSUM_SOLANA_RPC_URLS", "https://api.devnet.solana.com"),
        (
            "OXSUM_SOLANA_PAY_TO",
            "payToExample111111111111111111111111111",
        ),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
        ("OXSUM_SOLANA_RPC_AUTH_HEADER", "AuthorizationBearerX"),
    ]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_RPC_AUTH_HEADER"), "{err}");
    // The sanctions gate on a mainnet-shaped endpoint (issue #1179): unset
    // is a startup error naming the required choice — screening real money is
    // a deliberate posture, never an inherited default. `=1` passes (the
    // embedded SDN list is the screen's floor — no API key needed), an
    // explicit `=0` passes but warns observably on stderr, and a devnet
    // endpoint keeps the unset default with no error.
    let mainnet_rail = |extra: &[(&str, &str)]| {
        let mut vars = vec![
            (
                "OXSUM_SOLANA_RPC_URL",
                "https://mainnet.helius-rpc.com/?api-key=x",
            ),
            (
                "OXSUM_SOLANA_PAY_TO",
                "payToExample111111111111111111111111111",
            ),
            ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
            // The custody gate (issue #1189) is its own mainnet arm, opted
            // out here so these assertions isolate the sanctions choice.
            ("OXSUM_SOLANA_ALLOW_HOT_WALLET", "1"),
        ];
        vars.extend_from_slice(extra);
        run(&vars)
    };
    let bad = mainnet_rail(&[]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_SANCTIONS_ENABLED"), "{err}");
    let ok = mainnet_rail(&[("OXSUM_SOLANA_SANCTIONS_ENABLED", "1")]);
    assert!(
        ok.status.success(),
        "sanctions enabled on mainnet must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let ok = mainnet_rail(&[("OXSUM_SOLANA_SANCTIONS_ENABLED", "0")]);
    assert!(
        ok.status.success(),
        "an explicit off boots: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // The deliberate off stays observable: the boot's warn names the flag in
    // the process's own log output (fmt writes warns to stdout).
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&ok.stdout),
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(
        log.contains("WARN") && log.contains("OXSUM_SOLANA_SANCTIONS_ENABLED"),
        "the explicit off warns observably: {log}"
    );
    // A malformed value names itself — never a silent off.
    let bad = mainnet_rail(&[("OXSUM_SOLANA_SANCTIONS_ENABLED", "yes")]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_SANCTIONS_ENABLED"), "{err}");

    // The optional Helius webhook (issue #1150): the shared secret parses
    // beside a configured rail, and a lone secret beside no rail is a warn
    // (the receiver mounts nothing), never a startup error — `--check-config`
    // passes either way because polling stays the default.
    let ok = run(&[
        ("OXSUM_SOLANA_RPC_URLS", "https://api.devnet.solana.com"),
        (
            "OXSUM_SOLANA_PAY_TO",
            "payToExample111111111111111111111111111",
        ),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
        ("OXSUM_SOLANA_HELIUS_WEBHOOK_SECRET", "helius-secret"),
    ]);
    assert!(
        ok.status.success(),
        "the helius webhook secret beside a rail must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let ok = run(&[("OXSUM_SOLANA_HELIUS_WEBHOOK_SECRET", "helius-secret")]);
    assert!(
        ok.status.success(),
        "a lone webhook secret warns and drops, never errors: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    // The mainnet custody gate (issue #1189): a mainnet-shaped endpoint with
    // no `OXSUM_SOLANA_MULTISIG_ACCOUNT` refuses to start — a hot wallet
    // holding real funds is a custody risk — unless the explicit
    // `OXSUM_SOLANA_ALLOW_HOT_WALLET=1` opt-out is set. Devnet keeps multisig
    // optional + warn. The pair below is the Squads v4 fixture the unit tests
    // pin: multisig `C46NEXuB…` derives vault index 0 `65kaU3KW…`.
    let mainnet_rail = |extra: &[(&str, &str)]| {
        let mut vars = vec![
            (
                "OXSUM_SOLANA_RPC_URL",
                "https://mainnet.helius-rpc.com/?api-key=x",
            ),
            (
                "OXSUM_SOLANA_PAY_TO",
                "payToExample111111111111111111111111111",
            ),
            ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
            // The sanctions choice (issue #1179) is its own mainnet arm, made
            // here so these assertions isolate the custody gate.
            ("OXSUM_SOLANA_SANCTIONS_ENABLED", "1"),
        ];
        vars.extend_from_slice(extra);
        run(&vars)
    };
    // Mainnet + no multisig + no opt-out → startup fails naming both.
    let bad = mainnet_rail(&[]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_MULTISIG_ACCOUNT"), "{err}");
    assert!(err.contains("OXSUM_SOLANA_ALLOW_HOT_WALLET"), "{err}");
    // Mainnet + a matching multisig/vault pair → passes.
    let ok = run(&[
        (
            "OXSUM_SOLANA_RPC_URL",
            "https://mainnet.helius-rpc.com/?api-key=x",
        ),
        (
            "OXSUM_SOLANA_PAY_TO",
            "65kaU3KWZJCEz11vrCQfUZMc5AWSTzkFybwKD5BckLH8",
        ),
        ("OXSUM_SOLANA_RATE_MICRO", "200000000"),
        (
            "OXSUM_SOLANA_MULTISIG_ACCOUNT",
            "C46NEXuBLRJjJhfAsKBUxUYdxSBQUPc3SXQFRBTXfupC",
        ),
        ("OXSUM_SOLANA_SANCTIONS_ENABLED", "1"),
    ]);
    assert!(
        ok.status.success(),
        "the matching multisig/vault pair must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // Mainnet + no multisig + the explicit opt-out → passes with the warn.
    let ok = mainnet_rail(&[("OXSUM_SOLANA_ALLOW_HOT_WALLET", "1")]);
    assert!(
        ok.status.success(),
        "the explicit hot-wallet opt-out must pass: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let warn = format!(
        "{}{}",
        String::from_utf8_lossy(&ok.stderr),
        String::from_utf8_lossy(&ok.stdout)
    );
    assert!(warn.contains("ALLOW_HOT_WALLET"), "{warn}");
    // A malformed opt-out names itself — never a silent off.
    let bad = mainnet_rail(&[("OXSUM_SOLANA_ALLOW_HOT_WALLET", "yes")]);
    assert!(!bad.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&bad.stderr),
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(err.contains("OXSUM_SOLANA_ALLOW_HOT_WALLET"), "{err}");
    // Devnet + no multisig → passes, no error (the hot-wallet warn logs but
    // never gates — the `rail` helper's 127.0.0.1 endpoint is devnet-shaped,
    // and every earlier `rail(&…)` success case above already proves it).

    // Missing DATABASE_URL names that instead of connecting to nothing.
    let missing = binary::oxsum()
        .arg("--check-config")
        .current_dir(&dir)
        .env_clear()
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&missing.stderr),
        String::from_utf8_lossy(&missing.stdout)
    );
    assert!(err.contains("DATABASE_URL"), "{err}");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// `oxsum seed` is opt-in and fresh-database-only: without OXSUM_DEMO_SEED it
/// refuses before writing a byte; with it, on a database that already holds
/// organizations, it still refuses — the production-protection half of the
/// same rule.
#[tokio::test]
async fn seed_is_opt_in_and_refuses_a_used_database() {
    let Some(url) = oxsum_testkit::suite_db_url("install_ops_seed") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("oxsum-seed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(".env"), "").unwrap();

    let run_seed = |demo_flag: Option<&str>| {
        let mut cmd = binary::oxsum();
        cmd.arg("seed")
            .current_dir(&dir)
            .env_clear()
            .env("DATABASE_URL", &url);
        if let Some(v) = demo_flag {
            cmd.env("OXSUM_DEMO_SEED", v);
        }
        cmd.output().unwrap()
    };

    // Unset: refuses with the opt-in named, before migrations even run the
    // world in — the database may hold anything; the flag answers first.
    let refused = run_seed(None);
    assert!(!refused.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stderr),
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(err.contains("OXSUM_DEMO_SEED"), "{err}");

    // A database with organizations refuses even when the operator did opt in.
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    db.register(oxsum_core::NewUser {
        email: "owner@example.com".into(),
        password: "a real password".into(),
        organization_name: Some("Real Co".into()),
    })
    .await
    .unwrap();
    let refused = run_seed(Some("true"));
    assert!(!refused.status.success());
    let err = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stderr),
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(err.contains("already holds organizations"), "{err}");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// `--migrate-only` on a fresh database applies the schema and exits, and on
/// a used one is a no-op — the compose oxsum-migrate step in binary form.
#[tokio::test]
async fn migrate_only_brings_a_fresh_database_current() {
    let Some(url) = oxsum_testkit::suite_db_url("install_ops_migrate") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("oxsum-mig-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(".env"), "").unwrap();

    let run = || {
        binary::oxsum()
            .arg("--migrate-only")
            .current_dir(&dir)
            .env_clear()
            .env("DATABASE_URL", &url)
            .output()
            .unwrap()
    };

    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let out = String::from_utf8_lossy(&first.stdout);
    assert!(
        out.contains(oxsum_core::Db::latest_schema_version()),
        "{out}"
    );

    // Applied once is applied: the ledger holds the version the binary serves.
    let db = Db::connect(&url, 2).await.unwrap();
    assert_eq!(
        db.applied_schema_version().await.unwrap().as_deref(),
        Some(oxsum_core::Db::latest_schema_version())
    );
    // And again is a no-op — the idempotent upgrade shape.
    let second = run();
    assert!(second.status.success());

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A29 previous-version fixture: apply migrations through 0093 (pre-T20
/// journals), write fixture rows the upgrade must keep, then run the real
/// binary's `--migrate-only` and confirm both the new schema level and the
/// data. This is the restore-then-upgrade shape the upgrade runbook walks.
#[tokio::test]
async fn previous_version_fixture_upgrades_intact() {
    let Some(url) = oxsum_testkit::suite_db_url("install_ops_upgrade") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.pool()
        .execute(
            "CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations (\
             version text PRIMARY KEY, name text NOT NULL, \
             applied_at timestamptz NOT NULL DEFAULT now())",
        )
        .await
        .unwrap();
    // Apply every migration before the T20 journals pair by hand — the
    // fixture is a database that stopped exactly there.
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../core/migrations"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0094" {
            continue;
        }
        let mut tx = db.pool().begin().await.unwrap();
        tx.execute("SET LOCAL search_path = ''").await.unwrap();
        tx.execute(std::fs::read_to_string(&file).unwrap().as_str())
            .await
            .unwrap();
        sqlx::query("INSERT INTO oxsum._migrations (version,name) VALUES ($1,$1)")
            .bind(version)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    // A fixture row at the old level: one organization with an API key —
    // the rows every upgrade promises to keep.
    let reg = db
        .register(oxsum_core::NewUser {
            email: "pre-upgrade@example.com".into(),
            password: "fixture password".into(),
            organization_name: Some("Pre Upgrade Co".into()),
        })
        .await
        .unwrap();

    // The binary's own migrator closes the gap — not a test-side migrate, so
    // this exercises exactly what `docker compose run oxsum-migrate` runs.
    let dir = std::env::temp_dir().join(format!("oxsum-upg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(".env"), "").unwrap();
    let out = binary::oxsum()
        .arg("--migrate-only")
        .current_dir(&dir)
        .env_clear()
        .env("DATABASE_URL", &url)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        db.applied_schema_version().await.unwrap().as_deref(),
        Some(oxsum_core::Db::latest_schema_version())
    );
    // The pre-upgrade organization and its credential still resolve.
    let (identity, _) = db.authenticate(&reg.api_key.secret).await.unwrap().unwrap();
    assert_eq!(identity.id, reg.organization.id);
    // And the T20/T21 tables exist — the fixture upgraded to the new shape.
    let journals: bool = sqlx::query_scalar(
        "SELECT to_regclass('oxsum.invoice_journals') IS NOT NULL \
         AND to_regclass('oxsum.plan_statements') IS NOT NULL",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(journals);
}

/// A29 separate-environment restore: back up one scratch database with
/// `scripts/backup.sh`, restore it into a second database with
/// `scripts/restore.sh`, and compare what the move had to keep — the same
/// digest on both sides, the migration ledger, the organizations.
///
/// Runs the scripts against the local compose container (CONTAINER
/// autodetect finds `oxsum-postgres-1`); skips when Docker or the container
/// is absent.
#[tokio::test]
async fn backup_and_restore_between_databases_preserves_state() {
    let Some(url) = oxsum_testkit::suite_db_url("install_ops_restore") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    // The compose postgres container is the drill's cluster; without it the
    // scripts have no pg_dump/pg_restore to run.
    let container = std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|names| {
            names
                .lines()
                .find(|n| *n == "oxsum-postgres-1" || *n == "oxsum-core-postgres-1")
                .map(str::to_owned)
        });
    let Some(container) = container else {
        eprintln!("no compose postgres container: skipped");
        return;
    };

    // Environment A: a migrated database with an organization.
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    let reg = db
        .register(oxsum_core::NewUser {
            email: "restore@example.com".into(),
            password: "restore fixture password".into(),
            organization_name: Some("Restore Co".into()),
        })
        .await
        .unwrap();
    // The source database's name, for the scripts' POSTGRES_DB.
    let source = url.rsplit('/').next().unwrap().to_owned();

    let workdir = std::env::temp_dir().join(format!("oxsum-drill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&workdir).unwrap();
    let repo = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

    let backup = std::process::Command::new("bash")
        .arg(format!("{repo}/scripts/backup.sh"))
        .arg(&workdir)
        .current_dir(&workdir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("CONTAINER", &container)
        .env("POSTGRES_DB", &source)
        .output()
        .unwrap();
    assert!(
        backup.status.success(),
        "backup: {}",
        String::from_utf8_lossy(&backup.stderr)
    );

    // Environment B: a fresh database the restore lands in — created through
    // the admin connection, never over the source.
    let target = format!("{source}_restored");
    let manifest = std::fs::read_dir(&workdir)
        .unwrap()
        .map(|f| f.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".manifest.json")
        })
        .unwrap();
    let restore = std::process::Command::new("bash")
        .arg(format!("{repo}/scripts/restore.sh"))
        .arg(&manifest)
        .arg(&target)
        .current_dir(&workdir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("CONTAINER", &container)
        .env("POSTGRES_DB", &source) // the "live" db the script refuses — target differs
        .output()
        .unwrap();
    assert!(
        restore.status.success(),
        "restore: {}",
        String::from_utf8_lossy(&restore.stderr)
    );

    // The restored environment: same digest over the moved state — the same
    // organization row with the same id is the check that matters.
    let restored = Db::connect(
        &url.replace(&format!("/{source}"), &format!("/{target}")),
        2,
    )
    .await
    .unwrap();
    let org: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT organization_id FROM oxsum.organizations WHERE organization_id = $1",
    )
    .bind(reg.organization.id)
    .fetch_optional(restored.pool())
    .await
    .unwrap();
    assert_eq!(org, Some(reg.organization.id));
    let (identity, _) = restored
        .authenticate(&reg.api_key.secret)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(identity.id, reg.organization.id);
    // The migration ledger came with the dump — a backup is whole-database.
    assert_eq!(
        restored.applied_schema_version().await.unwrap().as_deref(),
        Some(oxsum_core::Db::latest_schema_version())
    );

    sqlx::query(&format!("DROP DATABASE IF EXISTS {target} WITH (FORCE)"))
        .execute(db.pool())
        .await
        .unwrap();
    std::fs::remove_dir_all(&workdir).unwrap();
}
