//! Fully funded admission regressions, sharing the coordinated gateway fixture.

#[path = "team_deletion.rs"]
mod team_deletion;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{ActingKey, NewOperation, OpenHold, ServingAccess};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

use super::{Answer, World, bind, call, chat, request_id, set_plan, url, world};

async fn cover(world: &World) {
    cover_with(world, 1_000_000).await;
}

async fn cover_with(world: &World, quota: i64) {
    let (status, body) = set_plan(
        &world.app,
        &format!("authority-{}", Uuid::new_v4()),
        json!({"quotaMinor": quota, "resetPeriod": "monthly", "allowWalletOverflow": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = bind(world, body["data"]["planId"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn complete(world: &World) -> Value {
    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 2,
            output: 1,
        },
    );
    let response = chat(&world.app, &world.key, &world.model, json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap();
    world.settlement_within(&id).await
}

async fn no_reservations(world: &World) {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.open_holds WHERE tenant_id=$1")
        .bind(&world.tenant_id)
        .fetch_one(&world.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.request_operations WHERE organization_id=$1 AND state IN ('created','leased','running')")
        .bind(world.organization_id.parse::<Uuid>().unwrap()).fetch_one(&world.pool).await.unwrap();
    assert_eq!(count, 0);
    assert_eq!(world.wallet().await.reserved().await.unwrap(), 0);
}

struct Reservation {
    key: ActingKey,
    watch: OpenHold,
    operation: NewOperation,
    access: ServingAccess,
}

async fn reservation(world: &World, freeze: i64) -> Reservation {
    let organization = world.organization_id.parse::<Uuid>().unwrap();
    let (_, key) = world.db.authenticate(&world.key).await.unwrap().unwrap();
    let route = world.db.routes(&world.model).await.unwrap().remove(0);
    let id = Uuid::new_v4();
    let access = world
        .db
        .serving_access(organization, Some(key.key_id))
        .await
        .unwrap();
    Reservation {
        watch: OpenHold {
            hold_key: format!("req-{id}:hold"),
            tenant_id: world.tenant_id.clone(),
            request_id: id.to_string(),
            model: world.model.clone(),
            channel: route.channel.clone(),
            price_version: route.version,
            input_price: 0,
            output_price: 0,
            freeze_minor: freeze,
            key_id: Some(key.key_id),
            mandate_id: None,
            end_user: None,
            service_tier: None,
            tags: BTreeMap::new(),
            sweep_attempts: 0,
            last_error: None,
            dead_at: None,
        },
        operation: NewOperation {
            operation_id: id,
            organization_id: organization,
            key_id: Some(key.key_id),
            surface: "chat".into(),
            fingerprint: format!("fixture/{id}"),
            bound_minor: freeze,
            model: world.model.clone(),
            channel: Some(route.channel),
            customer_id: None,
            terms_version: Some(route.version),
        },
        access,
        key,
    }
}

async fn admit(
    db: &oxsum_core::Db,
    wallet: &oxsum_core::Wallet,
    input: &Reservation,
) -> Result<oxsum_core::GatewayAdmission, oxsum_core::WalletError> {
    let description = oxsum_core::hold_description(
        &input.watch.request_id,
        &input.watch.model,
        input.watch.freeze_minor,
    )
    .unwrap();
    wallet
        .admit_gateway(
            db,
            &input.key,
            &input.watch,
            &input.operation,
            &input.access,
            &description,
            "fixture-worker",
            OffsetDateTime::now_utc().date(),
            None,
        )
        .await
}

async fn remaining_draw(world: &World) -> i64 {
    sqlx::query_scalar(
        "SELECT consumed_minor FROM oxsum.org_subscriptions \
         WHERE organization_id=$1 AND state='active'",
    )
    .bind(world.organization_id.parse::<Uuid>().unwrap())
    .fetch_one(&world.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn funded_calls_respect_each_organization_restriction() {
    for restriction in ["manual", "dunning", "archived"] {
        let world = world_or_skip!(0);
        cover(&world).await;
        world.script.answers(
            &world.model,
            Answer::Completion {
                input: 2,
                output: 1,
            },
        );
        let organization = world.organization_id.parse::<Uuid>().unwrap();
        match restriction {
            "manual" => {
                world.db.set_suspended(organization, true).await.unwrap();
            }
            "dunning" => {
                sqlx::query("UPDATE oxsum.organizations SET dunning_suspended_at=now() WHERE organization_id=$1")
                    .bind(organization).execute(&world.pool).await.unwrap();
            }
            _ => {
                sqlx::query("UPDATE oxsum.customer_profiles SET state='archived',archived_at=now() WHERE organization_id=$1")
                    .bind(organization).execute(&world.pool).await.unwrap();
            }
        }
        let response = chat(&world.app, &world.key, &world.model, json!({})).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{restriction}");
        let _ = response.into_body().collect().await.unwrap();
        assert!(world.script.seen().is_empty(), "refusal precedes upstream");
        assert_eq!(remaining_draw(&world).await, 0);
        assert_eq!(world.wallet().await.available().await.unwrap(), 0);
    }
}

#[tokio::test]
async fn funded_calls_respect_mandate_time_windows() {
    let world = world_or_skip!(0);
    cover(&world).await;
    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 2,
            output: 1,
        },
    );
    let (_, key) = world.db.authenticate(&world.key).await.unwrap().unwrap();
    for (before, after, expected) in [(-2, -1, "MANDATE_EXPIRED"), (1, 2, "MANDATE_NOT_YET_VALID")]
    {
        let now = OffsetDateTime::now_utc();
        let format = time::format_description::well_known::Rfc3339;
        let (status, created) = call(
            &world.app,
            "POST",
            &format!("/api/v1/org/keys/{}/mandates", key.key_id),
            Some(json!({
                "capMinor": 1_000,
                "notBefore": (now + time::Duration::hours(before)).format(&format).unwrap(),
                "notAfter": (now + time::Duration::hours(after)).format(&format).unwrap(),
            })),
            Some(&world.session),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let response = world
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", world.key))
                    .header(
                        "X-Oxsum-Mandate",
                        created["data"]["mandateBlob"].as_str().unwrap(),
                    )
                    .body(Body::from(
                        json!({"model": world.model, "messages": [{"role":"user","content":"hi"}]})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{expected}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], expected);
        assert!(world.script.seen().is_empty());
        assert_eq!(remaining_draw(&world).await, 0);
    }
}

#[tokio::test]
async fn funded_calls_count_against_concurrency_until_cancellation_settles() {
    let world = world_or_skip!(0);
    cover(&world).await;
    let (_, key) = world.db.authenticate(&world.key).await.unwrap().unwrap();
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", key.key_id),
        Some(json!({"maxConcurrentHolds": 1})),
        Some(&world.session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    world.script.answers(&world.model, Answer::Stall);
    let first = chat(
        &world.app,
        &world.key,
        &world.model,
        json!({"stream": true}),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first_id = request_id(&first);
    let mut first_body = first.into_body();
    assert!(first_body.frame().await.unwrap().is_ok());
    let second = chat(
        &world.app,
        &world.key,
        &world.model,
        json!({"stream": true}),
    )
    .await;
    let second_status = second.status();
    drop(second);
    drop(first_body);
    world.settlement_within(&first_id).await;
    assert_eq!(second_status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(world.script.seen().len(), 1);
    let watches: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.open_holds WHERE tenant_id=$1")
            .bind(&world.tenant_id)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert_eq!(watches, 0);
    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 2,
            output: 1,
        },
    );
    let third = chat(&world.app, &world.key, &world.model, json!({})).await;
    assert_eq!(third.status(), StatusCode::OK);
    let id = request_id(&third);
    let _ = third.into_body().collect().await.unwrap();
    world.settlement_within(&id).await;
    assert_eq!(world.wallet().await.available().await.unwrap(), 0);
}

#[tokio::test]
async fn a_genuinely_free_call_records_a_proof_without_wallet_or_quota() {
    let world = world_or_skip!(0);
    let route = world.db.routes(&world.model).await.unwrap().remove(0);
    let mut free = route.price;
    free.cost_per_request = Some(0);
    world
        .db
        .append_price(&route.channel, &world.model, free, 100)
        .await
        .unwrap();
    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 2,
            output: 1,
        },
    );
    let response = chat(&world.app, &world.key, &world.model, json!({})).await;
    let status = response.status();
    let id = request_id(&response);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let record = world.settlement_within(&id).await;
    assert_eq!(record["charged"], 0);
    assert_eq!(record["freeze"], 0);
    let wallet = world.wallet().await;
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    let entry =
        oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!("req-{id}:hold")));
    let proof = wallet.receipt_proof(entry, None).await.unwrap().unwrap();
    let hash = proof.entry.content_hash();
    assert!(oxsum_core::verify_bundle(&serde_json::to_string(&proof).unwrap(), &hash).unwrap());
    let draws: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.subscription_draws WHERE hold_key=$1")
            .bind(format!("req-{id}:hold"))
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert_eq!(draws, 0);
    cover(&world).await;
    complete(&world).await;
    assert_eq!(
        remaining_draw(&world).await,
        0,
        "free pricing must not spend paid quota"
    );
}

#[tokio::test]
async fn wallet_refusal_rolls_back_a_partial_quota_draw() {
    let world = world_or_skip!(0);
    cover_with(&world, 20).await;
    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 2,
            output: 1,
        },
    );
    let response = chat(&world.app, &world.key, &world.model, json!({})).await;
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    let _ = response.into_body().collect().await.unwrap();
    assert!(world.script.seen().is_empty());
    assert_eq!(remaining_draw(&world).await, 0);
    no_reservations(&world).await;
}

#[tokio::test]
async fn zero_wallet_requests_allow_exact_caps_and_refuse_preexisting_excess() {
    for scope in ["key", "member", "team"] {
        let world = world_or_skip!(100);
        complete(&world).await;
        cover(&world).await;
        let organization = world.organization_id.parse::<Uuid>().unwrap();
        let user = world.user_id.parse::<Uuid>().unwrap();
        let (_, key) = world.db.authenticate(&world.key).await.unwrap().unwrap();
        let team = if scope == "team" {
            let team = world
                .db
                .create_team(
                    organization,
                    oxsum_core::MembershipActor {
                        user_id: user,
                        role: oxsum_core::Role::Owner,
                    },
                    "budgeted",
                    Some(40),
                )
                .await
                .unwrap();
            world
                .db
                .update_member(
                    organization,
                    oxsum_core::MembershipActor {
                        user_id: user,
                        role: oxsum_core::Role::Owner,
                    },
                    user,
                    None,
                    None,
                    Some(Some(team.team_id)),
                )
                .await
                .unwrap();
            Some(team.team_id)
        } else {
            None
        };
        for (cap, expected) in [(40, StatusCode::OK), (39, StatusCode::TOO_MANY_REQUESTS)] {
            match scope {
                "key" => {
                    sqlx::query("UPDATE oxsum.api_keys SET spend_limit_minor=$2 WHERE key_id=$1")
                        .bind(key.key_id)
                        .bind(cap)
                        .execute(&world.pool)
                        .await
                        .unwrap();
                }
                "member" => {
                    sqlx::query("UPDATE oxsum.memberships SET budget_limit_minor=$3 WHERE organization_id=$1 AND user_id=$2").bind(organization).bind(user).bind(cap).execute(&world.pool).await.unwrap();
                }
                _ => {
                    sqlx::query("UPDATE oxsum.teams SET budget_limit_minor=$2 WHERE team_id=$1")
                        .bind(team)
                        .bind(cap)
                        .execute(&world.pool)
                        .await
                        .unwrap();
                }
            }
            let response = chat(&world.app, &world.key, &world.model, json!({})).await;
            assert_eq!(response.status(), expected, "{scope} cap {cap}");
            let id = request_id(&response);
            let _ = response.into_body().collect().await.unwrap();
            if expected == StatusCode::OK {
                world.settlement_within(&id).await;
            }
        }
        assert_eq!(remaining_draw(&world).await, 40);
        assert_eq!(world.script.seen().len(), 2);
        assert_eq!(
            world
                .wallet()
                .await
                .key_committed(&key.key_id)
                .await
                .unwrap(),
            40
        );
        no_reservations(&world).await;
    }
}

#[tokio::test]
async fn a_funded_cache_hit_rechecks_organization_restrictions() {
    let world = world_or_skip!(0);
    cover(&world).await;
    let route = world.db.routes(&world.model).await.unwrap().remove(0);
    world
        .db
        .set_semantic_cache(
            &route.channel,
            Some(&json!({"enabled":true,"ttlSeconds":3600})),
        )
        .await
        .unwrap();
    let served = complete(&world).await;
    let source = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!(
        "req-{}:hold",
        served["request"].as_str().unwrap()
    )));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let cached: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM oxsum.cached_responses WHERE source_entry_id=$1)",
            )
            .bind(source.as_uuid())
            .fetch_one(&world.pool)
            .await
            .unwrap();
            if cached {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let hit = chat(&world.app, &world.key, &world.model, json!({})).await;
    assert_eq!(hit.status(), StatusCode::OK);
    assert_eq!(hit.headers()["x-oxsum-cache"], "hit");
    let id = request_id(&hit);
    let _ = hit.into_body().collect().await.unwrap();
    world.settlement_within(&id).await;
    let before = remaining_draw(&world).await;
    world
        .db
        .set_suspended(world.organization_id.parse().unwrap(), true)
        .await
        .unwrap();
    let refused = chat(&world.app, &world.key, &world.model, json!({})).await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let _ = refused.into_body().collect().await.unwrap();
    assert_eq!(world.script.seen().len(), 1);
    assert_eq!(remaining_draw(&world).await, before);
    no_reservations(&world).await;
}

#[tokio::test]
async fn independent_identical_requests_have_distinct_operations_and_explicit_replay_does_not() {
    let world = world_or_skip!(0);
    cover(&world).await;
    complete(&world).await;
    complete(&world).await;
    for _ in 0..2 {
        let response = world
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", world.key))
                    .header("Idempotency-Key", "same-explicit-turn")
                    .body(Body::from(
                        json!({"model":world.model,"messages":[{"role":"user","content":"hi"}]})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = response.into_body().collect().await.unwrap();
    }
    let operations: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT operation_id,hold_key FROM oxsum.request_operations WHERE organization_id=$1",
    )
    .bind(world.organization_id.parse::<Uuid>().unwrap())
    .fetch_all(&world.pool)
    .await
    .unwrap();
    assert_eq!(operations.len(), 3);
    for (id, hold) in operations {
        assert_eq!(hold, format!("req-{id}:hold"));
    }
    assert_eq!(world.script.seen().len(), 3);
    assert_eq!(remaining_draw(&world).await, 120);
    no_reservations(&world).await;
}

#[tokio::test]
async fn current_key_state_and_route_permissions_win_before_atomic_admission() {
    for change in ["revoke", "expire", "models", "tier", "group"] {
        let world = world_or_skip!(0);
        cover(&world).await;
        let input = reservation(&world, 40).await;
        match change {
            "revoke" => {
                sqlx::query("UPDATE oxsum.api_keys SET revoked_at=now() WHERE key_id=$1")
                    .bind(input.key.key_id)
                    .execute(&world.pool)
                    .await
                    .unwrap();
            }
            "expire" => {
                sqlx::query("UPDATE oxsum.api_keys SET expires_at=now()-interval '1 second' WHERE key_id=$1").bind(input.key.key_id).execute(&world.pool).await.unwrap();
            }
            "models" => {
                sqlx::query(
                    "UPDATE oxsum.api_keys SET model_allowlist=ARRAY['other'] WHERE key_id=$1",
                )
                .bind(input.key.key_id)
                .execute(&world.pool)
                .await
                .unwrap();
            }
            "tier" => {
                sqlx::query("INSERT INTO oxsum.tier_profiles(name,model_allowlist) VALUES ('restricted-fixture','[\"other\"]') ON CONFLICT(name) DO UPDATE SET model_allowlist=EXCLUDED.model_allowlist").execute(&world.pool).await.unwrap();
                sqlx::query("UPDATE oxsum.organizations SET tier='restricted-fixture' WHERE organization_id=$1").bind(input.operation.organization_id).execute(&world.pool).await.unwrap();
            }
            _ => {
                let group: Uuid = sqlx::query_scalar(
                    "INSERT INTO oxsum.groups(org_id,name) VALUES ($1,'empty') RETURNING id",
                )
                .bind(input.operation.organization_id)
                .fetch_one(&world.pool)
                .await
                .unwrap();
                sqlx::query("UPDATE oxsum.api_keys SET group_id=$2 WHERE key_id=$1")
                    .bind(input.key.key_id)
                    .bind(group)
                    .execute(&world.pool)
                    .await
                    .unwrap();
            }
        }
        let wallet = world.wallet().await;
        let error = admit(&world.db, &wallet, &input).await.unwrap_err();
        assert!(
            matches!(
                error,
                oxsum_core::WalletError::Unauthenticated | oxsum_core::WalletError::Forbidden(_)
            ),
            "{change}: {error}"
        );
        assert_eq!(remaining_draw(&world).await, 0);
        no_reservations(&world).await;
    }
}

#[tokio::test]
async fn crash_recovery_of_funded_and_free_calls_is_atomic_and_replay_safe_with_one_connection() {
    let Some(database_url) = url() else { return };
    for amount in [0, 40] {
        let world = world(&database_url, 0).await;
        cover(&world).await;
        let input = reservation(&world, amount).await;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect(&database_url)
            .await
            .unwrap();
        let db = oxsum_core::Db::from_pool(pool.clone());
        let tenants = oxsum_core::Tenants::new(pool);
        let wallet = tenants.get(&world.tenant_id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), admit(&db, &wallet, &input))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remaining_draw(&world).await, amount);
        assert!(matches!(
            admit(&db, &wallet, &input).await,
            Err(oxsum_core::WalletError::Conflict(_))
        ));
        let now = OffsetDateTime::now_utc();
        // Failure after the receipt append must roll back every sibling effect.
        let function = format!("fixture_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.request_id='{}' THEN RAISE EXCEPTION 'sweep rollback fixture'; END IF; RETURN NEW; END $$", input.watch.request_id)).execute(&world.pool).await.unwrap();
        sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON oxsum.usage_records FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()")) .execute(&world.pool).await.unwrap();
        let report = tokio::time::timeout(
            Duration::from_secs(3),
            oxsum_core::sweep_stale_holds(
                &db,
                &tenants,
                now + time::Duration::minutes(1),
                now.date(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.resolved, 0);
        assert_eq!(remaining_draw(&world).await, amount);
        assert!(
            wallet
                .settled_record(&input.watch.hold_key)
                .await
                .unwrap()
                .is_none()
        );
        assert!(db.open_hold(&input.watch.hold_key).await.unwrap().is_some());
        sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.usage_records"))
            .execute(&world.pool)
            .await
            .unwrap();
        sqlx::query(&format!("DROP FUNCTION oxsum.{function}()"))
            .execute(&world.pool)
            .await
            .unwrap();
        let report = tokio::time::timeout(
            Duration::from_secs(3),
            oxsum_core::sweep_stale_holds(
                &db,
                &tenants,
                now + time::Duration::minutes(1),
                now.date(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.resolved, 1);
        assert_eq!(remaining_draw(&world).await, 0);
        let record = wallet
            .settled_record(&input.watch.hold_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.kind, oxsum_core::SettlementKind::Swept);
        assert_eq!(record.charged, 0);
        let operation = db
            .operation(
                input.operation.organization_id,
                input.operation.operation_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, oxsum_core::OperationState::TimedOut);
        let entry =
            oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&input.watch.hold_key));
        let proof = wallet.receipt_proof(entry, None).await.unwrap().unwrap();
        assert!(
            oxsum_core::verify_bundle(
                &serde_json::to_string(&proof).unwrap(),
                &proof.entry.content_hash()
            )
            .unwrap()
        );
        let report = oxsum_core::sweep_stale_holds(
            &db,
            &tenants,
            now + time::Duration::minutes(1),
            now.date(),
        )
        .await
        .unwrap();
        assert_eq!(report.resolved, 0);
        no_reservations(&world).await;
    }
}

#[tokio::test]
async fn cancellation_and_recovery_choose_one_complete_funded_outcome() {
    for (freeze, quota, top_up) in [(0, 100, 0), (40, 100, 0), (40, 20, 100)] {
        for sweep_first in [false, true] {
            let world = world_or_skip!(top_up);
            cover_with(&world, quota).await;
            if freeze == 0 {
                let route = world.db.routes(&world.model).await.unwrap().remove(0);
                let mut free = route.price;
                free.cost_per_request = Some(0);
                world
                    .db
                    .append_price(&route.channel, &world.model, free, 100)
                    .await
                    .unwrap();
            }
            world.script.answers(&world.model, Answer::Stall);
            let response = chat(
                &world.app,
                &world.key,
                &world.model,
                json!({"stream": true}),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let id = request_id(&response);
            let mut body = response.into_body();
            assert!(body.frame().await.unwrap().is_ok());
            let (_, key) = world.db.authenticate(&world.key).await.unwrap().unwrap();
            let wallet = world.wallet().await;
            let mut blocker = world.pool.begin().await.unwrap();
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *blocker)
                .await
                .unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(oxsum_core::limit_lock_key(&world.tenant_id, &key.key_id))
                .execute(&mut *blocker)
                .await
                .unwrap();
            let now = OffsetDateTime::now_utc();
            let db = world.db.clone();
            let tenants = world.tenants.clone();
            let sweep = async move {
                oxsum_core::sweep_stale_holds(
                    &db,
                    &tenants,
                    now + time::Duration::minutes(1),
                    now.date(),
                )
                .await
            };
            let job = if sweep_first {
                let job = tokio::spawn(sweep);
                wait_for_blocked(&world, pid, 1).await;
                drop(body);
                job
            } else {
                drop(body);
                wait_for_blocked(&world, pid, 1).await;
                tokio::spawn(sweep)
            };
            wait_for_blocked(&world, pid, 2).await;
            blocker.commit().await.unwrap();
            let report = tokio::time::timeout(Duration::from_secs(3), job)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(report.resolved, 1);
            let record = world.settlement_within(&id).await;
            let kind = if sweep_first {
                "swept"
            } else {
                "client_cancelled"
            };
            assert_eq!(record["kind"], kind);
            let charged = record["charged"].as_i64().unwrap();
            let subscription_charge = record["subscription"]["chargedMinor"].as_i64().unwrap_or(0);
            assert_eq!(remaining_draw(&world).await, subscription_charge);
            assert_eq!(
                wallet.available().await.unwrap(),
                top_up - charged + subscription_charge
            );
            let operation = world
                .db
                .operation(
                    world.organization_id.parse().unwrap(),
                    Uuid::parse_str(&id).unwrap(),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                operation.state,
                if sweep_first {
                    oxsum_core::OperationState::TimedOut
                } else {
                    oxsum_core::OperationState::Cancelled
                }
            );
            assert_eq!(operation.charged_minor, Some(charged));
            assert_eq!(operation.outcome_kind.as_deref(), Some(kind));
            let usage: Vec<(String, i64)> = sqlx::query_as(
                "SELECT kind,charged_minor FROM oxsum.usage_records WHERE request_id=$1",
            )
            .bind(&id)
            .fetch_all(&world.pool)
            .await
            .unwrap();
            assert_eq!(usage, vec![(kind.to_owned(), charged)]);
            let entry = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!(
                "req-{id}:hold"
            )));
            let proof = wallet.receipt_proof(entry, None).await.unwrap().unwrap();
            assert!(
                oxsum_core::verify_bundle(
                    &serde_json::to_string(&proof).unwrap(),
                    &proof.entry.content_hash()
                )
                .unwrap()
            );
            no_reservations(&world).await;
        }
    }
}

async fn wait_for_blocked(world: &World, pid: i32, count: i64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blocked: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid))")
                .bind(pid).fetch_one(&world.pool).await.unwrap();
            if blocked >= count { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
}

#[tokio::test]
async fn one_connection_also_admits_and_recovers_a_real_wallet_reservation() {
    let Some(database_url) = url() else { return };
    let world = world(&database_url, 100).await;
    let input = reservation(&world, 40).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&database_url)
        .await
        .unwrap();
    let db = oxsum_core::Db::from_pool(pool.clone());
    let tenants = oxsum_core::Tenants::new(pool);
    let wallet = tenants.get(&world.tenant_id).await.unwrap();
    let admitted = tokio::time::timeout(Duration::from_secs(3), admit(&db, &wallet, &input))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admitted.wallet_minor, 40);
    assert_eq!(wallet.reserved().await.unwrap(), 40);
    let now = OffsetDateTime::now_utc();
    let report = tokio::time::timeout(
        Duration::from_secs(3),
        oxsum_core::sweep_stale_holds(&db, &tenants, now + time::Duration::minutes(1), now.date()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(report.resolved, 1);
    assert_eq!(wallet.available().await.unwrap(), 100);
    no_reservations(&world).await;
}

#[tokio::test]
async fn a_failed_watch_write_rolls_back_quota_wallet_and_operation() {
    let world = world_or_skip!(100);
    cover_with(&world, 20).await;
    let input = reservation(&world, 40).await;
    let wallet = world.wallet().await;
    let function = format!("fixture_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.request_id='{}' THEN RAISE EXCEPTION 'admission rollback fixture'; END IF; RETURN NEW; END $$", input.watch.request_id)).execute(&world.pool).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON oxsum.open_holds FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()" )).execute(&world.pool).await.unwrap();
    assert!(admit(&world.db, &wallet, &input).await.is_err());
    assert_eq!(remaining_draw(&world).await, 0);
    assert_eq!(world.wallet().await.available().await.unwrap(), 100);
    no_reservations(&world).await;
    sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.open_holds"))
        .execute(&world.pool)
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION oxsum.{function}()"))
        .execute(&world.pool)
        .await
        .unwrap();
    let admitted = admit(&world.db, &wallet, &input).await.unwrap();
    assert_eq!(admitted.subscription.drawn_minor, 20);
    assert_eq!(admitted.wallet_minor, 20);
}

#[tokio::test]
async fn a_racing_legacy_watch_cannot_become_an_authorized_reservation() {
    let world = world_or_skip!(100);
    cover_with(&world, 20).await;
    let input = reservation(&world, 40).await;
    let operation_id = input.operation.operation_id;
    let mut legacy = input.watch.clone();
    legacy.request_id = Uuid::new_v4().to_string();
    legacy.key_id = None;
    legacy.tags.insert("fixture".into(), "legacy".into());
    let function = format!("fixture_{}", Uuid::new_v4().simple());
    let gate = oxsum_core::limit_lock_key(&function, &Uuid::new_v4());
    let mut blocker = world.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(gate)
        .execute(&mut *blocker)
        .await
        .unwrap();
    // Pause after the existence check, immediately before the unique insert.
    sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.request_id='{}' THEN PERFORM pg_advisory_xact_lock({gate}::bigint); END IF; RETURN NEW; END $$", input.watch.request_id)).execute(&world.pool).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON oxsum.open_holds FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()" )).execute(&world.pool).await.unwrap();
    let db = world.db.clone();
    let wallet = world.wallet().await;
    let job = tokio::spawn(async move { admit(&db, &wallet, &input).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)))")
                .bind(pid).fetch_one(&world.pool).await.unwrap();
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    world.db.note_open_hold(&legacy).await.unwrap();
    blocker.commit().await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), job)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, oxsum_core::WalletError::Conflict(_)));
    assert_eq!(remaining_draw(&world).await, 0);
    assert_eq!(world.wallet().await.available().await.unwrap(), 100);
    assert_eq!(world.wallet().await.reserved().await.unwrap(), 0);
    assert!(
        world
            .db
            .operation(world.organization_id.parse().unwrap(), operation_id)
            .await
            .unwrap()
            .is_none()
    );
    let retained = world.db.open_hold(&legacy.hold_key).await.unwrap().unwrap();
    assert_eq!(retained.request_id, legacy.request_id);
    assert_eq!(retained.tags, legacy.tags);
    assert_eq!(retained.key_id, None);
    let wallet_minor: Option<i64> =
        sqlx::query_scalar("SELECT wallet_minor FROM oxsum.open_holds WHERE hold_key=$1")
            .bind(&legacy.hold_key)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert_eq!(wallet_minor, None);
    sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.open_holds"))
        .execute(&world.pool)
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION oxsum.{function}()"))
        .execute(&world.pool)
        .await
        .unwrap();
    world.db.clear_open_hold(&legacy.hold_key).await.unwrap();
    no_reservations(&world).await;
}

#[tokio::test]
async fn racing_funded_requests_reserve_one_slot_and_a_wallet_hold_counts_once() {
    for top_up in [0, 100] {
        let world = world_or_skip!(top_up);
        if top_up == 0 {
            cover(&world).await;
        }
        let a = reservation(&world, 40).await;
        let b = reservation(&world, 40).await;
        sqlx::query("UPDATE oxsum.api_keys SET max_concurrent_holds=1 WHERE key_id=$1")
            .bind(a.key.key_id)
            .execute(&world.pool)
            .await
            .unwrap();
        let wallet = world.wallet().await;
        let (first, second) =
            tokio::join!(admit(&world.db, &wallet, &a), admit(&world.db, &wallet, &b));
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let error = if let Err(error) = first {
            error
        } else {
            second.unwrap_err()
        };
        assert!(matches!(
            error,
            oxsum_core::WalletError::TooManyHolds { open: 1, limit: 1 }
        ));
        let entitlement = wallet
            .entitlement_read(
                &a.operation.organization_id,
                &a.key.key_id,
                oxsum_core::KeyScope::Key(a.key.key_id),
                Some(&world.model),
                None,
                &BTreeMap::new(),
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entitlement.open_holds, 1);
        let now = OffsetDateTime::now_utc();
        let report = oxsum_core::sweep_stale_holds(
            &world.db,
            &world.tenants,
            now + time::Duration::minutes(1),
            now.date(),
        )
        .await
        .unwrap();
        assert_eq!(report.resolved, 1);
        no_reservations(&world).await;
    }
}

#[tokio::test]
async fn plan_dunning_keeps_paid_quota_serving_but_refuses_wallet_overage() {
    let world = world_or_skip!(100);
    let organization = world.organization_id.parse::<Uuid>().unwrap();
    let business: Uuid = sqlx::query_scalar(
        "SELECT business_id FROM oxsum.customer_profiles WHERE organization_id=$1",
    )
    .bind(organization)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    let plan = world
        .db
        .publish_sold_plan(
            business,
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: "covered quota".into(),
                price_minor: 0,
                currency: "USD".into(),
                quota_minor: Some(60),
                period_kind: oxsum_core::PeriodKind::CalendarMonth,
                overage_enabled: true,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    let customer_plan = world
        .db
        .bind_customer_plan(
            organization,
            plan.plan_version_id,
            oxsum_core::PeriodAuthorization::Complimentary(Uuid::new_v4()),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.customer_plans SET dunning_suspended_at=now() WHERE customer_plan_id=$1",
    )
    .bind(customer_plan.customer_plan_id)
    .execute(&world.pool)
    .await
    .unwrap();
    complete(&world).await;
    assert_eq!(remaining_draw(&world).await, 40);
    let refused = chat(&world.app, &world.key, &world.model, json!({})).await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let _ = refused.into_body().collect().await.unwrap();
    assert_eq!(world.script.seen().len(), 1);
    assert_eq!(remaining_draw(&world).await, 40);
    assert_eq!(world.wallet().await.available().await.unwrap(), 100);
    no_reservations(&world).await;
}

#[tokio::test]
async fn legacy_wallet_and_funded_admissions_wait_for_append_before_key_rows() {
    let world = world_or_skip!(100);
    cover(&world).await;
    let funded = reservation(&world, 40).await;
    let second_key = super::mint_key(&world.app, &world.session, "legacy-wallet").await;
    let (_, legacy) = world.db.authenticate(&second_key).await.unwrap().unwrap();
    let legacy_id = legacy.key_id;
    let wallet = world.wallet().await;
    let store = doubleentry::storage::postgres::PostgresStore::<{ oxsum_core::SCALE }>::new(
        world.pool.clone(),
        doubleentry::LedgerId::new(format!("tenant-{}", world.tenant_id)).unwrap(),
    );
    let mut blocker = world.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(store.append_lock())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let legacy_wallet = wallet.clone();
    let legacy_job = tokio::spawn(async move {
        legacy_wallet
            .hold_for_key(
                &legacy,
                None,
                &format!("legacy-{}", Uuid::new_v4()),
                "",
                40,
                OffsetDateTime::now_utc().date(),
                None,
            )
            .await
    });
    let funded_wallet = wallet.clone();
    let db = world.db.clone();
    let funded_job = tokio::spawn(async move { admit(&db, &funded_wallet, &funded).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory'")
                .fetch_one(&world.pool).await.unwrap();
            if waiting >= 2 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    // Neither waiter may own a key row while waiting on the append owner.
    sqlx::query("SET LOCAL lock_timeout='200ms'")
        .execute(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT key_id FROM oxsum.api_keys WHERE key_id=$1 FOR UPDATE")
        .bind(legacy_id)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), legacy_job)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), funded_job)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(wallet.reserved().await.unwrap(), 40);
    assert_eq!(remaining_draw(&world).await, 40);
}

#[tokio::test]
async fn independent_keys_do_not_exhaust_a_small_pool_while_waiting_to_reserve() {
    let Some(database_url) = url() else { return };
    let mut world = world(&database_url, 500).await;
    let mut inputs = Vec::new();
    for n in 0..6 {
        world.key = super::mint_key(&world.app, &world.session, &format!("burst-{n}")).await;
        inputs.push(reservation(&world, 40).await);
    }
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&database_url)
        .await
        .unwrap();
    let wallet = std::sync::Arc::new(
        oxsum_core::Wallet::open_existing(pool.clone(), &world.tenant_id)
            .await
            .unwrap(),
    );
    let db = oxsum_core::Db::from_pool(pool);
    let mut jobs = Vec::new();
    for input in inputs {
        let wallet = wallet.clone();
        let db = db.clone();
        jobs.push(tokio::spawn(
            async move { admit(&db, &wallet, &input).await },
        ));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        for job in jobs {
            job.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(wallet.reserved().await.unwrap(), 240);
}

#[tokio::test]
async fn a_funded_request_still_obeys_the_mandates_existing_monetary_cap() {
    let world = world_or_skip!(100);
    let input = reservation(&world, 40).await;
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let now = OffsetDateTime::now_utc();
    let created = world
        .db
        .create_mandate(
            input.operation.organization_id,
            input.key.key_id,
            oxsum_core::KeyScope::Organization,
            39,
            None,
            None,
            Some(now - time::Duration::minutes(1)),
            now + time::Duration::hours(1),
            &secret,
        )
        .await
        .unwrap()
        .unwrap();
    let verifying = world
        .db
        .mandate_verifying_key(&input.key.key_id, &secret)
        .await
        .unwrap()
        .unwrap();
    let presented = oxsum_core::parse_presented(&created.mandate_blob, &verifying).unwrap();
    // A historical ledger fact can exceed a mandate's cap (for example after
    // importing records). Admission must still inspect that monetary basis
    // when its new quota share requires zero wallet money.
    let wallet = world.wallet().await;
    let hold = format!("mandate-ledger-{}", Uuid::new_v4());
    let mut historical = presented.clone();
    historical.verified.cap_minor = 40;
    wallet
        .hold_for_key(
            &input.key,
            Some(&world.model),
            &hold,
            "",
            40,
            now.date(),
            Some(&historical),
        )
        .await
        .unwrap();
    wallet.settle(&hold, "", 40, now.date()).await.unwrap();
    cover(&world).await;
    let mut next = reservation(&world, 40).await;
    next.watch.mandate_id = Some(presented.verified.mandate_id);
    let description =
        oxsum_core::hold_description(&next.watch.request_id, &world.model, 40).unwrap();
    let error = wallet
        .admit_gateway(
            &world.db,
            &next.key,
            &next.watch,
            &next.operation,
            &next.access,
            &description,
            "fixture",
            now.date(),
            Some(&presented),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        oxsum_core::WalletError::MandateExceeded {
            cap_minor: 39,
            committed_minor: 40
        }
    ));
    assert_eq!(remaining_draw(&world).await, 0);
    no_reservations(&world).await;
}

async fn sold_quota(world: &World, quota: i64, legacy: bool) -> oxsum_core::CustomerPlan {
    let organization = world.organization_id.parse::<Uuid>().unwrap();
    let business: Uuid = sqlx::query_scalar(
        "SELECT business_id FROM oxsum.customer_profiles WHERE organization_id=$1",
    )
    .bind(organization)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    let terms = oxsum_core::NewSoldPlan {
        plan_id: None,
        name: "atomic quota".into(),
        price_minor: 0,
        currency: "USD".into(),
        quota_minor: Some(quota),
        period_kind: oxsum_core::PeriodKind::CalendarMonth,
        overage_enabled: true,
        sort_order: 0,
    };
    let plan = if legacy {
        super::legacy_plan::publish(&world.db, business, &terms).await
    } else {
        world.db.publish_sold_plan(business, &terms).await
    }
    .unwrap();
    world
        .db
        .bind_customer_plan(
            organization,
            plan.plan_version_id,
            oxsum_core::PeriodAuthorization::Complimentary(Uuid::new_v4()),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn quota_reservations_do_not_consume_the_plan_wallet_credit_limit() {
    let world = world_or_skip!(100);
    let plan = sold_quota(&world, 60, false).await;
    sqlx::query("UPDATE oxsum.customer_plans SET credit_limit_minor=20 WHERE customer_plan_id=$1")
        .bind(plan.customer_plan_id)
        .execute(&world.pool)
        .await
        .unwrap();
    let first = reservation(&world, 40).await;
    let wallet = world.wallet().await;
    let admitted = admit(&world.db, &wallet, &first).await.unwrap();
    assert_eq!(admitted.wallet_minor, 0);
    let second = reservation(&world, 40).await;
    let admitted = admit(&world.db, &wallet, &second).await.unwrap();
    assert_eq!(admitted.subscription.drawn_minor, 20);
    assert_eq!(admitted.wallet_minor, 20);
    assert_eq!(wallet.reserved().await.unwrap(), 20);
    let third = reservation(&world, 40).await;
    assert!(matches!(
        admit(&world.db, &wallet, &third).await,
        Err(oxsum_core::WalletError::InsufficientFunds)
    ));
    assert_eq!(remaining_draw(&world).await, 60);
}

#[tokio::test]
async fn lazy_plan_boundary_reuses_the_admissions_only_connection() {
    let Some(database_url) = url() else { return };
    let world = world(&database_url, 100).await;
    // Historical v1 periods can be moved to a due boundary by the fixture.
    // Today's v2 publisher freezes service dates; do not rewrite that history.
    let plan = sold_quota(&world, 60, true).await;
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_end=now()-interval '1 second' WHERE subscription_id=$1").bind(plan.subscription_id).execute(&world.pool).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_end=now()-interval '1 second' WHERE customer_plan_id=$1 AND state='active'").bind(plan.customer_plan_id).execute(&world.pool).await.unwrap();
    let input = reservation(&world, 40).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&database_url)
        .await
        .unwrap();
    let wallet = oxsum_core::Wallet::open_existing(pool.clone(), &world.tenant_id)
        .await
        .unwrap();
    let db = oxsum_core::Db::from_pool(pool);
    let admission = tokio::time::timeout(Duration::from_secs(3), admit(&db, &wallet, &input))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        admission.subscription.drawn_minor, 40,
        "past-due entitlement remains usable"
    );
    assert_eq!(
        world
            .db
            .customer_plan(input.operation.organization_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        oxsum_core::CustomerPlanState::PastDue
    );
}

async fn cancel_at_due_boundary(world: &World, plan: &oxsum_core::CustomerPlan) {
    world
        .db
        .cancel_customer_plan(world.organization_id.parse().unwrap())
        .await
        .unwrap();
    // The fixture uses an original v1 period whose dates are mutable. Current
    // v2 periods retain their immutable service dates and are never rewritten.
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_end=now()-interval '1 second' WHERE subscription_id=$1")
        .bind(plan.subscription_id).execute(&world.pool).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_end=now()-interval '1 second' WHERE customer_plan_id=$1 AND state='active'")
        .bind(plan.customer_plan_id).execute(&world.pool).await.unwrap();
}

async fn closed_cancelled_period(world: &World, plan: &oxsum_core::CustomerPlan, consumed: i64) {
    let organization = world.organization_id.parse().unwrap();
    assert!(
        world
            .db
            .customer_plan(organization)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        world
            .db
            .active_subscription(organization)
            .await
            .unwrap()
            .is_none()
    );
    let periods = world
        .db
        .subscription_periods(plan.subscription_id.unwrap())
        .await
        .unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].outcome, "cancelled");
    assert_eq!(periods[0].consumed_minor, consumed);
    let periods = world.db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].state, oxsum_core::PlanPeriodState::Closed);
    let journals: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.invoice_journals WHERE customer_plan_id=$1")
            .bind(plan.customer_plan_id)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert_eq!(journals, 1, "cancellation closes and bills exactly once");
}

#[tokio::test]
async fn a_cancelled_boundary_refusal_rolls_back_every_admission_effect() {
    let world = world_or_skip!(0);
    let plan = sold_quota(&world, 60, true).await;
    cancel_at_due_boundary(&world, &plan).await;
    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 2,
            output: 1,
        },
    );
    for _ in 0..2 {
        let response = chat(&world.app, &world.key, &world.model, json!({})).await;
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        let _ = response.into_body().collect().await.unwrap();
        assert!(
            world.script.seen().is_empty(),
            "ended quota never reaches upstream"
        );
        assert_eq!(remaining_draw(&world).await, 0);
        no_reservations(&world).await;
        let periods = world
            .db
            .subscription_periods(plan.subscription_id.unwrap())
            .await
            .unwrap();
        assert_eq!(
            periods[0].outcome, "open",
            "the refused transaction rolls back"
        );
        let effects: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM oxsum.subscription_draws WHERE subscription_id=$1), \
                    (SELECT count(*) FROM oxsum.invoice_journals WHERE customer_plan_id=$2)",
        )
        .bind(plan.subscription_id)
        .bind(plan.customer_plan_id)
        .fetch_one(&world.pool)
        .await
        .unwrap();
        assert_eq!(effects, (0, 0));
        assert_eq!(world.wallet().await.available().await.unwrap(), 0);
    }
    world
        .wallet()
        .await
        .top_up(
            "cancelled-boundary-funding",
            100,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let record = complete(&world).await;
    assert_eq!(record["charged"], 40);
    assert!(record.get("subscription").is_none());
    assert_eq!(world.wallet().await.available().await.unwrap(), 60);
    assert_eq!(world.script.seen().len(), 1);
    no_reservations(&world).await;
    closed_cancelled_period(&world, &plan, 0).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    closed_cancelled_period(&world, &plan, 0).await;
}

#[tokio::test]
async fn both_cancelled_boundary_winners_preserve_old_draws_and_charge_new_calls() {
    for worker_first in [false, true] {
        let world = world_or_skip!(100);
        let organization = world.organization_id.parse().unwrap();
        let plan = sold_quota(&world, 60, true).await;
        let old_key = format!("old-boundary-draw-{}", Uuid::new_v4());
        let old = world
            .db
            .admit_subscription_draw(organization, &old_key, 40)
            .await
            .unwrap();
        assert_eq!(old.drawn_minor, 40);
        cancel_at_due_boundary(&world, &plan).await;
        world.script.answers(
            &world.model,
            Answer::Completion {
                input: 2,
                output: 1,
            },
        );

        // Stop the first boundary owner after it owns the subscription lock.
        // pg_blocking_pids proves the other boundary path and old settlement
        // actually wait behind it; scheduler timing does not select a winner.
        let gate = Uuid::new_v4().as_u128() as i64;
        let function = format!("fixture_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(
            "CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN IF NEW.customer_plan_id='{}' AND NEW.state='cancelled' THEN \
             PERFORM pg_advisory_xact_lock({gate}); END IF; RETURN NEW; END $$",
            plan.customer_plan_id,
        ))
        .execute(&world.pool)
        .await
        .unwrap();
        sqlx::query(&format!(
            "CREATE TRIGGER {function} BEFORE UPDATE ON oxsum.customer_plans \
             FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()"
        ))
        .execute(&world.pool)
        .await
        .unwrap();
        let mut blocker = world.pool.begin().await.unwrap();
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(gate)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let launch_worker = || {
            let db = world.db.clone();
            tokio::spawn(async move { db.rollover_due_subscriptions().await })
        };
        let launch_request = || {
            let app = world.app.clone();
            let key = world.key.clone();
            let model = world.model.clone();
            tokio::spawn(async move {
                let response = chat(&app, &key, &model, json!({})).await;
                let status = response.status();
                let id = request_id(&response);
                let _ = response.into_body().collect().await.unwrap();
                (status, id)
            })
        };
        let mut worker = worker_first.then(launch_worker);
        let mut request = (!worker_first).then(launch_request);
        wait_for_blocked(&world, blocker_pid, 1).await;
        let winner_pid: i32 = sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity WHERE datname=current_database() \
             AND $1=ANY(pg_blocking_pids(pid))",
        )
        .bind(blocker_pid)
        .fetch_one(&world.pool)
        .await
        .unwrap();
        if worker_first {
            request = Some(launch_request());
        } else {
            worker = Some(launch_worker());
        }
        let db = world.db.clone();
        let settle_key = old_key.clone();
        let settlement =
            tokio::spawn(async move { db.settle_subscription_draw(&settle_key, 20).await });
        wait_for_blocked(&world, winner_pid, 2).await;
        blocker.commit().await.unwrap();
        let (worker, request, settled) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(worker.unwrap(), request.unwrap(), settlement)
        })
        .await
        .unwrap();
        let report = worker.unwrap().unwrap();
        assert_eq!(report.expired, usize::from(worker_first));
        assert_eq!(report.rolled, 0);
        assert_eq!(settled.unwrap().unwrap(), Some(20));
        let (status, id) = request.unwrap();
        assert_eq!(status, StatusCode::OK);
        let record = world.settlement_within(&id).await;
        assert_eq!(record["charged"], 40);
        assert!(record.get("subscription").is_none());
        assert_eq!(world.wallet().await.available().await.unwrap(), 60);
        assert_eq!(world.script.seen().len(), 1);
        assert_eq!(
            world
                .db
                .settle_subscription_draw(&old_key, 20)
                .await
                .unwrap(),
            Some(20)
        );
        no_reservations(&world).await;
        closed_cancelled_period(&world, &plan, 20).await;
        let new_draws: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.subscription_draws WHERE subscription_id=$1",
        )
        .bind(plan.subscription_id)
        .fetch_one(&world.pool)
        .await
        .unwrap();
        assert_eq!(new_draws, 1, "only the pre-boundary draw remains");
        world.db.rollover_due_subscriptions().await.unwrap();
        closed_cancelled_period(&world, &plan, 20).await;
        sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.customer_plans"))
            .execute(&world.pool)
            .await
            .unwrap();
        sqlx::query(&format!("DROP FUNCTION oxsum.{function}()"))
            .execute(&world.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn legacy_unproven_watch_cleanup_never_creates_a_free_receipt() {
    let world = world_or_skip!(0);
    let input = reservation(&world, 40).await;
    world.db.note_open_hold(&input.watch).await.unwrap();
    let now = OffsetDateTime::now_utc();
    let report = oxsum_core::sweep_stale_holds(
        &world.db,
        &world.tenants,
        now + time::Duration::minutes(1),
        now.date(),
    )
    .await
    .unwrap();
    assert_eq!(report.resolved, 1);
    assert!(
        world
            .wallet()
            .await
            .settled_record(&input.watch.hold_key)
            .await
            .unwrap()
            .is_none()
    );
    no_reservations(&world).await;
}

#[tokio::test]
async fn an_inflight_personal_key_edit_cannot_deadlock_operation_insertion() {
    let world = world_or_skip!(0);
    cover(&world).await;
    let input = reservation(&world, 40).await;
    let key = input.key.key_id;
    let mut editor = world.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *editor)
        .await
        .unwrap();
    sqlx::query(
        "SELECT organization_id FROM oxsum.organizations WHERE organization_id=$1 FOR UPDATE",
    )
    .bind(input.operation.organization_id)
    .fetch_one(&mut *editor)
    .await
    .unwrap();
    let db = world.db.clone();
    let wallet = world.wallet().await;
    let job = tokio::spawn(async move { admit(&db, &wallet, &input).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)))")
                .bind(pid).fetch_one(&world.pool).await.unwrap();
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    sqlx::query("SET LOCAL lock_timeout='200ms'")
        .execute(&mut *editor)
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.api_keys SET revoked_at=now() WHERE key_id=$1")
        .bind(key)
        .execute(&mut *editor)
        .await
        .unwrap();
    editor.commit().await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), job)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, oxsum_core::WalletError::Unauthenticated));
    assert_eq!(remaining_draw(&world).await, 0);
    no_reservations(&world).await;
}
