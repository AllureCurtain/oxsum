//! Explicit application reads: authority, customer sale, quota and no hidden mutations.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/legacy_plan.rs"]
mod legacy_plan;
#[path = "support/integration.rs"]
mod support;

use std::collections::BTreeMap;

use oxsum_core::{
    ActingService, ApplicationCustomerArchiveRequest, ApplicationCustomerKeyRequest,
    CustomerPlanState, Db, IntegrationApplicationRequest, IntegrationCredentialRevokeRequest,
    IntegrationScope, KeyConstraints, NewSoldPlan, PeriodAuthorization, PeriodKind, Price,
    ProvisionCustomerRequest, SaleChange, SaleChangeRequest, SaleTerms, SecretKey, WalletError,
};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("application_serving_reads")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
async fn grant(db: &Db, application: Uuid, scopes: Vec<IntegrationScope>) -> (ActingService, Uuid) {
    let mut request = support::request(vec![], scopes);
    request.application_id = Some(application);
    let created = db
        .create_integration_credential(&support::owner(db).await, &request)
        .await
        .unwrap();
    (
        db.authenticate_service(created.secret.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap(),
        created.credential.id,
    )
}
async fn setup(db: &Db) -> (ActingService, Uuid, Uuid, Uuid) {
    let app = db
        .create_integration_application(
            &support::owner(db).await,
            &IntegrationApplicationRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                key: Uuid::new_v4().simple().to_string(),
                name: "Read fixture".into(),
                customer_limit: 10,
                reason: "scoped read fixture".into(),
            },
        )
        .await
        .unwrap();
    let (service, credential) = grant(
        db,
        app.application_id,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::EntitlementsRead,
            IntegrationScope::CustomerKeysWrite,
            IntegrationScope::CustomersLifecycleWrite,
        ],
    )
    .await;
    let customer = provision(db, &service).await;
    (service, app.application_id, customer, credential)
}
async fn provision(db: &Db, service: &ActingService) -> Uuid {
    db.provision_application_customer(
        service,
        &ProvisionCustomerRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            external_id: Uuid::new_v4().to_string(),
            name: "Read customer".into(),
            contact_email: None,
        },
    )
    .await
    .unwrap()
    .customer
    .organization_id
}
async fn key(db: &Db, service: &ActingService, org: Uuid, names: &[&str]) -> Uuid {
    db.create_application_customer_key(
        service,
        org,
        &ApplicationCustomerKeyRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            name: "Read key".into(),
            reason: "model filter".into(),
            expires_at: OffsetDateTime::now_utc() + Duration::hours(1),
            spend_limit_minor: 100,
            model_allowlist: names.iter().map(|v| (*v).into()).collect(),
            requests_per_minute: 5,
            max_concurrent_holds: 1,
        },
    )
    .await
    .unwrap()
    .key
    .id
}
fn terms(rate: i64) -> SaleTerms {
    serde_json::from_value(json!({"price":{"inputPricePerMillion":rate,"outputPricePerMillion":rate,"maxOutputTokens":20},"discountPercent":0})).unwrap()
}
fn price() -> Price {
    serde_json::from_value(json!({"inputPricePerMillion":888888,"outputPricePerMillion":999999,"maxOutputTokens":20,"upstream":{"inputPricePerMillion":777777,"outputPricePerMillion":777777}})).unwrap()
}
async fn channel(db: &Db, names: &[&str], protocol: &str) -> String {
    let channel = Uuid::new_v4().simple().to_string();
    db.set_channel(
        &channel,
        "https://private-provider.example/v1",
        "provider-private-key",
        protocol,
        &SecretKey::from_bytes([41; 32]),
    )
    .await
    .unwrap();
    for model in names {
        db.append_price(&channel, model, price(), 100)
            .await
            .unwrap();
    }
    channel
}
async fn change(db: &Db, operation: SaleChange) -> Uuid {
    db.change_sale_terms(
        &support::owner(db).await,
        &SaleChangeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "sale read fixture".into(),
            change: operation,
        },
    )
    .await
    .unwrap()
    .resource_id
}
async fn book(db: &Db, models: &[(&str, i64)]) -> Uuid {
    let book = change(
        db,
        SaleChange::CreateBook {
            name: Uuid::new_v4().to_string(),
        },
    )
    .await;
    change(
        db,
        SaleChange::SaveDraft {
            book_id: book,
            expected_revision: 0,
            models: models
                .iter()
                .map(|(name, rate)| ((*name).to_owned(), terms(*rate)))
                .collect::<BTreeMap<_, _>>(),
        },
    )
    .await;
    change(
        db,
        SaleChange::PublishDraft {
            book_id: book,
            draft_revision: 1,
        },
    )
    .await;
    book
}
async fn scope_models(db: &Db, org: Uuid, names: &[&str]) {
    let tier = Uuid::new_v4().simple().to_string();
    db.set_tier(
        &tier,
        None,
        Some(names.iter().map(|name| (*name).into()).collect()),
    )
    .await
    .unwrap();
    db.set_organization_tier(org, Some(&tier)).await.unwrap();
}
async fn facts(db: &Db, org: Uuid) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object(\
      'organization',(SELECT to_jsonb(o) FROM oxsum.organizations o WHERE organization_id=$1),\
      'profile',(SELECT to_jsonb(c) FROM oxsum.customer_profiles c WHERE organization_id=$1),\
      'plans',(SELECT jsonb_agg(to_jsonb(p) ORDER BY customer_plan_id) FROM oxsum.customer_plans p WHERE organization_id=$1),\
      'quota',(SELECT jsonb_agg(to_jsonb(s) ORDER BY subscription_id) FROM oxsum.org_subscriptions s WHERE organization_id=$1),\
      'periods',(SELECT jsonb_agg(to_jsonb(p) ORDER BY plan_period_id) FROM oxsum.plan_periods p JOIN oxsum.customer_plans c USING(customer_plan_id) WHERE c.organization_id=$1),\
      'draws',(SELECT count(*) FROM oxsum.subscription_draws d JOIN oxsum.org_subscriptions s USING(subscription_id) WHERE s.organization_id=$1),\
      'digests',(SELECT count(*) FROM oxsum.sale_snapshot_digests WHERE organization_id=$1),\
      'statements',(SELECT jsonb_agg(to_jsonb(s) ORDER BY statement_id) FROM oxsum.plan_statements s WHERE organization_id=$1))")
        .bind(org).fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
async fn explicit_scope_namespace_current_revocation_and_expiry_are_enforced() {
    let Some(db) = db().await else { return };
    let (reader, app, org, credential) = setup(&db).await;
    let (foreign, _, foreign_org, _) = setup(&db).await;
    for scope in [
        IntegrationScope::CustomersRead,
        IntegrationScope::BillingRead,
        IntegrationScope::CustomerKeysWrite,
    ] {
        let (limited, _) = grant(&db, app, vec![scope]).await;
        assert!(matches!(
            db.application_customer_entitlement(&limited, org).await,
            Err(WalletError::Forbidden(_))
        ));
        assert!(matches!(
            db.application_customer_models(&limited, org, None, None, 20)
                .await,
            Err(WalletError::Forbidden(_))
        ));
    }
    let legacy = support::request(vec![org], vec![IntegrationScope::EntitlementsRead]);
    assert!(matches!(
        db.create_integration_credential(&support::owner(&db).await, &legacy)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    assert!(matches!(
        db.application_customer_entitlement(&foreign, org).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.application_customer_models(&reader, foreign_org, None, None, 20)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let (rotated, rotated_id) = grant(&db, app, vec![IntegrationScope::EntitlementsRead]).await;
    db.revoke_integration_credential(
        &support::owner(&db).await,
        credential,
        &IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotate reader".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.application_customer_entitlement(&reader, org).await,
        Err(WalletError::InvalidCredentials)
    ));
    assert_eq!(
        db.application_customer_entitlement(&rotated, org)
            .await
            .unwrap()
            .application_id,
        app
    );
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=clock_timestamp()-interval '2 days', expires_at=clock_timestamp()-interval '1 second' WHERE credential_id=$1").bind(rotated_id).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.application_customer_models(&rotated, org, None, None, 20)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn model_pagination_applies_alias_customer_key_and_group_authority_before_limit() {
    let Some(db) = db().await else { return };
    let (reader, _, org, _) = setup(&db).await;
    let (foreign, _, foreign_org, _) = setup(&db).await;
    let suffix = Uuid::new_v4().simple().to_string();
    let a = format!("a-{suffix}");
    let alias = format!("b-{suffix}");
    let target = format!("c-{suffix}");
    let hidden = format!("d-{suffix}");
    let event = format!("e-{suffix}");
    let task = format!("f-{suffix}");
    let allowed = channel(&db, &[&a, &target, &hidden], "openai").await;
    // Two routes for one public name still produce one page row.
    channel(&db, &[&a], "openai").await;
    db.create_alias(&allowed, &alias, &target).await.unwrap();
    let mut event_price = price();
    event_price.mode = oxsum_core::BillingMode::Event;
    event_price.max_output_tokens = 0;
    db.append_price(&allowed, &event, event_price.clone(), 100)
        .await
        .unwrap();
    let tasks = channel(&db, &[], "tasks").await;
    db.append_price(&tasks, &task, event_price, 100)
        .await
        .unwrap();
    scope_models(&db, org, &[&a, &alias, &target, &event, &task]).await;
    let page = db
        .application_customer_models(&reader, org, None, None, 2)
        .await
        .unwrap();
    assert_eq!(
        page.models
            .iter()
            .map(|row| row.model.as_str())
            .collect::<Vec<_>>(),
        vec![a.as_str(), alias.as_str()]
    );
    assert_eq!(page.next_cursor.as_deref(), Some(alias.as_str()));
    let tail = db
        .application_customer_models(&reader, org, None, page.next_cursor.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(
        tail.models
            .iter()
            .map(|row| row.model.as_str())
            .collect::<Vec<_>>(),
        vec![target.as_str(), task.as_str()]
    );
    assert!(tail.next_cursor.is_none());
    let only_alias = key(&db, &reader, org, &[&alias]).await;
    assert!(
        db.application_customer_models(&reader, org, Some(only_alias), None, 2)
            .await
            .unwrap()
            .models
            .is_empty()
    );
    let own = key(&db, &reader, org, &[&alias, &target, &hidden, &task]).await;
    let group = db
        .put_group(
            org,
            "route-filter",
            &[db.channel_id_named(&allowed).await.unwrap().unwrap()],
        )
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.api_keys SET group_id=$1 WHERE key_id=$2")
        .bind(group.id)
        .bind(own)
        .execute(db.pool())
        .await
        .unwrap();
    let narrowed = db
        .application_customer_models(&reader, org, Some(own), None, 50)
        .await
        .unwrap();
    assert_eq!(
        narrowed
            .models
            .iter()
            .map(|row| row.model.as_str())
            .collect::<Vec<_>>(),
        vec![alias.as_str(), target.as_str()]
    );
    let portal = db
        .create_key(org, None, None, None, KeyConstraints::default())
        .await
        .unwrap();
    let other = key(&db, &foreign, foreign_org, &[&a]).await;
    for key in [portal.key.id, other, Uuid::new_v4()] {
        assert!(matches!(
            db.application_customer_models(&reader, org, Some(key), None, 20)
                .await,
            Err(WalletError::NotFound(_))
        ));
    }
    sqlx::query("UPDATE oxsum.api_keys SET expires_at=clock_timestamp()-interval '1 second' WHERE key_id=$1").bind(own).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.application_customer_models(&reader, org, Some(own), None, 20)
            .await,
        Err(WalletError::NotFound(_))
    ));
    sqlx::query("UPDATE oxsum.api_keys SET revoked_at=clock_timestamp() WHERE key_id=$1")
        .bind(only_alias)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.application_customer_models(&reader, org, Some(only_alias), None, 20)
            .await,
        Err(WalletError::NotFound(_))
    ));
    for limit in [0, 51] {
        assert!(matches!(
            db.application_customer_models(&reader, org, None, None, limit)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    assert!(matches!(
        db.application_customer_models(&reader, org, None, Some(" padded "), 20)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn sale_precedence_zero_future_missing_and_two_customers_match_gateway_without_writes() {
    let Some(db) = db().await else { return };
    let (reader, _, org, _) = setup(&db).await;
    let other = provision(&db, &reader).await;
    let suffix = Uuid::new_v4().simple().to_string();
    let alpha = format!("alpha-{suffix}");
    let beta = format!("beta-{suffix}");
    let missing = format!("missing-{suffix}");
    let route = channel(&db, &[&alpha, &beta, &missing], "openai").await;
    scope_models(&db, org, &[&alpha, &beta, &missing]).await;
    scope_models(&db, other, &[&alpha, &beta, &missing]).await;
    let default = book(&db, &[(&alpha, 100), (&beta, 120)]).await;
    change(
        &db,
        SaleChange::ScheduleDefault {
            book_id: default,
            version: 1,
            effective_from: None,
            effective_until: None,
            replaces: None,
        },
    )
    .await;
    let assigned = book(&db, &[(&alpha, 200)]).await;
    change(
        &db,
        SaleChange::AssignBook {
            organization_id: org,
            book_id: assigned,
            version: 1,
            effective_from: None,
            effective_until: None,
            replaces: None,
        },
    )
    .await;
    let assigned_read = db
        .application_customer_models(&reader, org, None, None, 50)
        .await
        .unwrap();
    assert_eq!(
        assigned_read.models[0].sale_terms.as_ref().unwrap().source,
        "assigned"
    );
    assert_eq!(
        assigned_read.models[0]
            .sale_terms
            .as_ref()
            .unwrap()
            .terms
            .price
            .price()
            .input_price_per_million,
        200
    );
    change(
        &db,
        SaleChange::OverrideModel {
            organization_id: org,
            model: alpha.clone(),
            terms: Box::new(terms(0)),
            effective_from: None,
            effective_until: None,
            replaces: None,
        },
    )
    .await;
    change(
        &db,
        SaleChange::OverrideModel {
            organization_id: org,
            model: beta.clone(),
            terms: Box::new(terms(600)),
            effective_from: Some(OffsetDateTime::now_utc() + Duration::days(1)),
            effective_until: None,
            replaces: None,
        },
    )
    .await;
    let before = facts(&db, org).await;
    let page = db
        .application_customer_models(&reader, org, None, None, 50)
        .await
        .unwrap();
    assert_eq!(
        facts(&db, org).await,
        before,
        "model reads must not publish sale snapshots or alter entitlement"
    );
    let url = oxsum_testkit::suite_db_url("application_serving_reads").unwrap();
    let single = Db::connect(&url, 1).await.unwrap();
    let one = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        single.application_customer_models(&reader, org, None, None, 50),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(one.models.len(), 3);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        single.application_customer_entitlement(&reader, org),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(
        page.models[0].sale_terms.as_ref().unwrap().source,
        "override"
    );
    assert_eq!(
        page.models[0]
            .sale_terms
            .as_ref()
            .unwrap()
            .terms
            .price
            .price()
            .input_price_per_million,
        0
    );
    assert_eq!(
        page.models[1].sale_terms.as_ref().unwrap().source,
        "default",
        "assigned book lacks beta and future override is inactive"
    );
    assert_eq!(
        page.models[1]
            .sale_terms
            .as_ref()
            .unwrap()
            .terms
            .price
            .price()
            .input_price_per_million,
        120
    );
    assert!(
        page.models[2].sale_terms.is_none(),
        "wholesale route price is never a customer-sale fallback"
    );
    let second = db
        .application_customer_models(&reader, other, None, None, 50)
        .await
        .unwrap();
    assert_eq!(
        second.models[0]
            .sale_terms
            .as_ref()
            .unwrap()
            .terms
            .price
            .price()
            .input_price_per_million,
        100
    );
    let serialized = serde_json::to_string(&page).unwrap();
    for hidden in [
        "provider-private-key",
        "private-provider.example",
        "upstream",
        "777777",
        "888888",
        "999999",
        route.as_str(),
    ] {
        assert!(!serialized.contains(hidden), "leaked {hidden}");
    }
    let business = db.default_business().await.unwrap();
    for row in page.models.iter().take(2) {
        let pinned = db
            .resolve_sale_price(business, org, &row.model, page.as_of)
            .await
            .unwrap();
        assert_eq!(pinned.terms, row.sale_terms.as_ref().unwrap().terms);
        assert_eq!(
            pinned.schedule_id,
            Some(row.sale_terms.as_ref().unwrap().schedule_id)
        );
    }
}

async fn plan(db: &Db, org: Uuid, price: i64, quota: Option<i64>) -> oxsum_core::CustomerPlan {
    plan_with_policy(db, org, price, quota, false).await
}

async fn plan_with_policy(
    db: &Db,
    org: Uuid,
    price: i64,
    quota: Option<i64>,
    historical: bool,
) -> oxsum_core::CustomerPlan {
    let new = NewSoldPlan {
        plan_id: None,
        name: Uuid::new_v4().to_string(),
        price_minor: price,
        currency: "USD".into(),
        quota_minor: quota,
        period_kind: PeriodKind::CalendarMonth,
        overage_enabled: true,
        sort_order: 0,
    };
    let business = db.default_business().await.unwrap();
    let sold = if historical {
        legacy_plan::publish(db, business, &new).await.unwrap()
    } else {
        db.publish_sold_plan(business, &new).await.unwrap()
    };
    db.bind_customer_plan(
        org,
        sold.plan_version_id,
        if price == 0 {
            PeriodAuthorization::Complimentary(Uuid::new_v4())
        } else {
            PeriodAuthorization::PaymentOrder(Uuid::new_v4())
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn quota_pending_draw_ended_cancel_and_archive_are_read_without_transition() {
    let Some(db) = db().await else { return };
    let (reader, _, org, _) = setup(&db).await;
    let empty = db
        .application_customer_entitlement(&reader, org)
        .await
        .unwrap();
    assert!(empty.plan.is_none() && empty.quota.is_none());
    // A legacy period has no immutable daily-accounting anchor. Advancing its
    // fixture boundary preserves all production guards and historical terms.
    let plan = plan_with_policy(&db, org, 0, Some(100), true).await;
    db.admit_subscription_draw(org, &Uuid::new_v4().to_string(), 25)
        .await
        .unwrap();
    let before = facts(&db, org).await;
    let standing = db
        .application_customer_entitlement(&reader, org)
        .await
        .unwrap();
    let quota = standing.quota.unwrap();
    assert_eq!(quota.window_state, "current");
    assert_eq!(quota.consumed_minor, 25);
    assert_eq!(quota.remaining_recorded_minor, Some(75));
    assert!(quota.plan_period_id.is_some());
    assert_eq!(facts(&db, org).await, before);
    db.cancel_customer_plan(org).await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    let end: OffsetDateTime = sqlx::query_scalar("SELECT now()-interval '1 second'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.org_subscriptions SET current_period_end=$1 WHERE subscription_id=$2",
    )
    .bind(end)
    .bind(plan.subscription_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_end=$1 WHERE customer_plan_id=$2")
        .bind(end)
        .bind(plan.customer_plan_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = facts(&db, org).await;
    let ended = db
        .application_customer_entitlement(&reader, org)
        .await
        .unwrap();
    assert!(ended.plan.as_ref().unwrap().cancel_at_period_end);
    assert_eq!(ended.quota.as_ref().unwrap().window_state, "ended");
    assert_eq!(
        ended.quota.as_ref().unwrap().remaining_recorded_minor,
        Some(75)
    );
    assert_eq!(
        facts(&db, org).await,
        before,
        "an entitlement read cannot complete pending cancellation or renew quota"
    );
    db.archive_application_customer(
        &reader,
        org,
        &ApplicationCustomerArchiveRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "archive read fixture".into(),
        },
    )
    .await
    .unwrap();
    let archived = db
        .application_customer_entitlement(&reader, org)
        .await
        .unwrap();
    assert_eq!(archived.customer_state, "archived");
    sqlx::query("UPDATE oxsum.customer_plans SET state='cancelled' WHERE customer_plan_id=$1")
        .bind(plan.customer_plan_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET state='cancelled' WHERE subscription_id=$1")
        .bind(plan.subscription_id)
        .execute(db.pool())
        .await
        .unwrap();
    let terminal = db
        .application_customer_entitlement(&reader, org)
        .await
        .unwrap();
    assert_eq!(terminal.plan.unwrap().state, CustomerPlanState::Cancelled);
    assert_eq!(terminal.quota.unwrap().state, "cancelled");
}

#[tokio::test]
async fn pending_payment_unmetered_and_shared_debt_keep_their_exact_meanings() {
    let Some(db) = db().await else { return };
    let (reader, _, org, _) = setup(&db).await;
    let pending = plan(&db, org, 100, Some(100)).await;
    let status = db
        .application_customer_entitlement(&reader, org)
        .await
        .unwrap();
    assert_eq!(
        status.plan.unwrap().state,
        CustomerPlanState::PendingPayment
    );
    assert!(status.quota.is_none());
    let other = provision(&db, &reader).await;
    let active = plan(&db, other, 0, None).await;
    let unmetered = db
        .application_customer_entitlement(&reader, other)
        .await
        .unwrap()
        .quota
        .unwrap();
    assert!(unmetered.quota_minor.is_none() && unmetered.remaining_recorded_minor.is_none());
    sqlx::query("INSERT INTO oxsum.plan_statements(statement_id,customer_plan_id,organization_id,business_id,state,total_minor,paid_minor,currency,due_at) VALUES($1,$2,$3,$4,'issued',50,10,'USD',now()-interval '8 days')")
        .bind(Uuid::new_v4()).bind(active.customer_plan_id).bind(other).bind(db.default_business().await.unwrap()).execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.customer_plans SET credit_limit_minor=100,dunning_suspended_at=NULL WHERE customer_plan_id=$1").bind(active.customer_plan_id).execute(db.pool()).await.unwrap();
    let before = facts(&db, other).await;
    let debt = db
        .application_customer_entitlement(&reader, other)
        .await
        .unwrap();
    assert!(debt.plan_overage_suspended);
    assert!(!debt.manually_suspended && !debt.organization_dunning_suspended);
    assert_eq!(debt.outstanding_plan_debt_minor, 40);
    let credit = debt.plan.unwrap().credit;
    assert_eq!(credit.credit_limit_minor, Some(100));
    assert_eq!(credit.outstanding_minor, 40);
    assert!(credit.next_due_at.is_some());
    assert_eq!(
        serde_json::to_value(credit).unwrap(),
        serde_json::to_value(db.plan_credit_standing(other).await.unwrap().unwrap()).unwrap()
    );
    assert_eq!(
        facts(&db, other).await,
        before,
        "reading past-grace debt cannot stamp dunning or alter the plan"
    );
    assert_eq!(
        db.application_customer_entitlement(&reader, org)
            .await
            .unwrap()
            .plan
            .unwrap()
            .customer_plan_id,
        pending.customer_plan_id
    );
}

#[tokio::test]
async fn exhausted_quota_and_account_restrictions_remain_visible_without_mutations() {
    let Some(db) = db().await else { return };
    let (reader, _, org, _) = setup(&db).await;
    plan(&db, org, 0, Some(100)).await;
    db.admit_subscription_draw(org, &Uuid::new_v4().to_string(), 100)
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.organizations SET suspended_at=now(),dunning_suspended_at=now() WHERE organization_id=$1")
        .bind(org).execute(db.pool()).await.unwrap();
    let before = facts(&db, org).await;
    let snapshot = db
        .application_customer_entitlement(&reader, org)
        .await
        .unwrap();
    assert!(snapshot.manually_suspended && snapshot.organization_dunning_suspended);
    let quota = snapshot.quota.unwrap();
    assert_eq!(quota.remaining_recorded_minor, Some(0));
    assert_eq!(quota.consumed_minor, 100);
    assert_eq!(facts(&db, org).await, before);
}
