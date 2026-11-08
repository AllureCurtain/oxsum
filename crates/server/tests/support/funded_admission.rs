//! Fully funded admission regressions, sharing the coordinated gateway fixture.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

use super::{Answer, World, bind, call, chat, request_id, set_plan, url, world};

async fn cover(world: &World) {
    let (status, body) = set_plan(
        &world.app,
        &format!("authority-{}", Uuid::new_v4()),
        json!({"quotaMinor": 1_000_000, "resetPeriod": "monthly"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = bind(world, body["data"]["planId"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
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
}
