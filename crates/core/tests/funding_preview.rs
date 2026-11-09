//! The read-only funding snapshot shares live admission's caps and pool semantics.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{
    Db, GatewayFundingPreview, KeyConstraints, KeyPrincipal, MembershipActor, NewUser, Principal,
    ResetPeriod, Role, SecretKey, Tenants, WalletError,
};
use time::OffsetDateTime;
use uuid::Uuid;

struct Fixture {
    db: Db,
    tenants: Tenants,
    reg: oxsum_core::Registration,
    principal: Principal,
    model: String,
    channel: String,
}
async fn fixture() -> Option<Fixture> {
    let url = oxsum_testkit::suite_db_url("funding_preview")?;
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let reg = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "funding preview password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let principal = Principal::Session(
        db.login(&reg.user.email, "funding preview password")
            .await
            .unwrap()
            .into_session()
            .principal
            .unwrap(),
    );
    let channel = format!("preview-{}", Uuid::new_v4().simple());
    let model = channel.clone();
    db.set_channel(
        &channel,
        "https://provider.example/v1",
        "fixture-secret",
        "openai",
        &SecretKey::from_bytes([7; 32]),
    )
    .await
    .unwrap();
    let book=oxsum_core::PriceBook::from_json(&channel,&format!(r#"{{"{model}":{{"inputPricePerMillion":0,"outputPricePerMillion":0,"maxOutputTokens":32}}}}"#)).unwrap();
    db.append_price(
        &channel,
        &model,
        book.models().next().unwrap().1.clone(),
        100,
    )
    .await
    .unwrap();
    let tenants = Tenants::new(db.pool().clone());
    tenants.get(&reg.organization.tenant_id).await.unwrap();
    Some(Fixture {
        db,
        tenants,
        reg,
        principal,
        model,
        channel,
    })
}
async fn preview(f: &Fixture, bound: i64) -> GatewayFundingPreview {
    f.tenants
        .get(&f.reg.organization.tenant_id)
        .await
        .unwrap()
        .preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &f.model, bound)
        .await
        .unwrap()
}
async fn quota(f: &Fixture, limit: Option<i64>, overflow: bool) -> Uuid {
    let plan =
        f.db.set_subscription_plan(
            &format!("quota-{}", Uuid::new_v4()),
            limit,
            ResetPeriod::Monthly,
            overflow,
            None,
            None,
            true,
            0,
        )
        .await
        .unwrap();
    f.db.bind_subscription(f.reg.organization.id, plan.plan_id, "admin", None)
        .await
        .unwrap()
        .subscription_id
}
async fn facts(f: &Fixture) -> serde_json::Value {
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    let row:serde_json::Value=sqlx::query_scalar("SELECT jsonb_build_object('subscriptions',(SELECT jsonb_agg(to_jsonb(s)) FROM oxsum.org_subscriptions s WHERE organization_id=$1),'draws',(SELECT count(*) FROM oxsum.subscription_draws d JOIN oxsum.org_subscriptions s USING(subscription_id) WHERE s.organization_id=$1),'holds',(SELECT count(*) FROM oxsum.open_holds WHERE tenant_id=$2),'operations',(SELECT count(*) FROM oxsum.request_operations WHERE organization_id=$1))").bind(f.reg.organization.id).bind(&f.reg.organization.tenant_id).fetch_one(f.db.pool()).await.unwrap();
    serde_json::json!({"rows":row,"log":w.log_size().await.unwrap(),"available":w.available().await.unwrap(),"reserved":w.reserved().await.unwrap()})
}

#[tokio::test]
async fn free_and_fully_funded_calls_need_no_wallet_and_preview_writes_nothing() {
    let Some(f) = fixture().await else { return };
    assert!(preview(&f, 0).await.allowed);
    assert_eq!(preview(&f, 30).await.reason, Some("insufficientFunds"));
    quota(&f, Some(100), true).await;
    let before = facts(&f).await;
    for _ in 0..2 {
        let result = preview(&f, 100).await;
        assert!(result.allowed, "{result:?}");
        assert_eq!(result.quota_minor, 100);
        assert_eq!(result.wallet_minor, 0);
    }
    assert_eq!(before, facts(&f).await);
    let result = preview(&f, 101).await;
    assert_eq!(result.quota_minor, 100);
    assert_eq!(result.wallet_minor, 1);
    assert_eq!(result.reason, Some("insufficientFunds"));
    f.db.admit_subscription_draw(f.reg.organization.id, "pending", 80)
        .await
        .unwrap();
    let result = preview(&f, 30).await;
    assert_eq!(result.quota_minor, 20);
    assert!(!result.allowed);
}

#[tokio::test]
async fn overflow_exhaustion_cancel_and_pending_boundary_are_explicit() {
    let Some(f) = fixture().await else { return };
    let subscription = quota(&f, Some(5), false).await;
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    w.top_up("fund", 100, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    assert_eq!(preview(&f, 6).await.reason, Some("quotaExhausted"));
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE subscription_id=$1").bind(subscription).execute(f.db.pool()).await.unwrap();
    let before = facts(&f).await;
    assert_eq!(preview(&f, 6).await.reason, Some("quotaTransitionRequired"));
    assert_eq!(before, facts(&f).await);
    f.db.cancel_subscription(f.reg.organization.id)
        .await
        .unwrap();
    let result = preview(&f, 6).await;
    assert!(result.allowed);
    assert_eq!(result.quota_minor, 0);
    assert_eq!(result.wallet_minor, 6);
}

#[tokio::test]
async fn key_member_team_caps_and_pending_holds_apply_to_wallet_remainder() {
    for scope in ["key", "member", "team"] {
        let Some(f) = fixture().await else { return };
        let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
        w.top_up("fund", 100, OffsetDateTime::now_utc().date())
            .await
            .unwrap();
        let (_, key) =
            f.db.authenticate(&f.reg.api_key.secret)
                .await
                .unwrap()
                .unwrap();
        w.hold_for_key(
            &key,
            None,
            "held",
            &f.model,
            5,
            OffsetDateTime::now_utc().date(),
            None,
        )
        .await
        .unwrap();
        match scope {
            "key" => {
                f.db.update_key_constraints_as(
                    &f.principal,
                    key.key_id,
                    KeyConstraints {
                        spend_limit_minor: Some(5),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            }
            "member" => {
                sqlx::query("UPDATE oxsum.memberships SET budget_limit_minor=5 WHERE organization_id=$1 AND user_id=$2").bind(f.reg.organization.id).bind(f.reg.user.id).execute(f.db.pool()).await.unwrap();
            }
            _ => {
                let team =
                    f.db.create_team(
                        f.reg.organization.id,
                        MembershipActor {
                            user_id: f.reg.user.id,
                            role: Role::Owner,
                        },
                        "budget",
                        Some(5),
                    )
                    .await
                    .unwrap();
                sqlx::query("UPDATE oxsum.memberships SET team_id=$1 WHERE organization_id=$2 AND user_id=$3").bind(team.team_id).bind(f.reg.organization.id).bind(f.reg.user.id).execute(f.db.pool()).await.unwrap();
            }
        }
        assert!(preview(&f, 0).await.allowed, "{scope}");
        assert_eq!(
            preview(&f, 1).await.reason,
            Some("budgetExceeded"),
            "{scope}"
        );
        quota(&f, Some(20), true).await;
        assert!(preview(&f, 20).await.allowed, "{scope}");
        assert_eq!(
            preview(&f, 21).await.reason,
            Some("budgetExceeded"),
            "{scope}"
        );
    }
}

#[tokio::test]
async fn scope_alias_group_customer_and_current_key_state_are_enforced() {
    let Some(f) = fixture().await else { return };
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    let alias = format!("alias-{}", Uuid::new_v4().simple());
    f.db.create_alias(&f.channel, &alias, &f.model)
        .await
        .unwrap();
    f.db.update_key_constraints_as(
        &f.principal,
        f.reg.api_key.key.id,
        KeyConstraints {
            model_allowlist: Some(vec![alias.clone()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &alias, 0)
            .await
            .unwrap()
            .reason,
        Some("modelNotAllowed")
    );
    f.db.update_key_constraints_as(
        &f.principal,
        f.reg.api_key.key.id,
        KeyConstraints::default(),
    )
    .await
    .unwrap();
    let group =
        f.db.put_group(f.reg.organization.id, "empty", &[])
            .await
            .unwrap();
    f.db.update_key_constraints_as(
        &f.principal,
        f.reg.api_key.key.id,
        KeyConstraints {
            group_id: Some(Some(group.id)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(preview(&f, 0).await.reason, Some("modelUnavailable"));
    let (organization, key) =
        f.db.authenticate(&f.reg.api_key.secret)
            .await
            .unwrap()
            .unwrap();
    assert!(matches!(
        w.preview_gateway_funding(
            &f.db,
            &Principal::Key(KeyPrincipal { organization, key }),
            f.reg.api_key.key.id,
            &f.model,
            0
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    f.db.revoke_key_as(&f.principal, f.reg.api_key.key.id)
        .await
        .unwrap();
    assert!(matches!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &f.model, 0)
            .await,
        Err(WalletError::NotFound(_))
    ));
}

#[tokio::test]
async fn suspension_and_concurrency_restrict_even_free_calls() {
    let Some(f) = fixture().await else { return };
    for column in ["suspended_at", "dunning_suspended_at"] {
        sqlx::query(&format!(
            "UPDATE oxsum.organizations SET {column}=now() WHERE organization_id=$1"
        ))
        .bind(f.reg.organization.id)
        .execute(f.db.pool())
        .await
        .unwrap();
        assert_eq!(preview(&f, 0).await.reason, Some("restricted"));
        sqlx::query(&format!(
            "UPDATE oxsum.organizations SET {column}=NULL WHERE organization_id=$1"
        ))
        .bind(f.reg.organization.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    }
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    w.top_up("fund", 100, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    let (_, key) =
        f.db.authenticate(&f.reg.api_key.secret)
            .await
            .unwrap()
            .unwrap();
    w.hold_for_key(
        &key,
        None,
        "held",
        &f.model,
        1,
        OffsetDateTime::now_utc().date(),
        None,
    )
    .await
    .unwrap();
    f.db.update_key_constraints_as(
        &f.principal,
        key.key_id,
        KeyConstraints {
            max_concurrent_holds: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(preview(&f, 0).await.reason, Some("concurrencyExceeded"));
}

#[path = "support/legacy_plan.rs"]
mod legacy_plan;

#[tokio::test]
async fn retained_sold_quota_preserves_debt_restrictions_without_closing_or_renewing() {
    let Some(f) = fixture().await else { return };
    let terms = oxsum_core::NewSoldPlan {
        plan_id: None,
        name: Uuid::new_v4().to_string(),
        price_minor: 0,
        currency: "USD".into(),
        quota_minor: Some(10),
        period_kind: oxsum_core::PeriodKind::CalendarMonth,
        overage_enabled: true,
        sort_order: 0,
    };
    let sold = legacy_plan::publish(&f.db, f.db.default_business().await.unwrap(), &terms)
        .await
        .unwrap();
    let plan =
        f.db.bind_customer_plan(
            f.reg.organization.id,
            sold.plan_version_id,
            oxsum_core::PeriodAuthorization::Complimentary(Uuid::new_v4()),
        )
        .await
        .unwrap();
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    w.top_up("fund", 100, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.customer_plans SET credit_limit_minor=1,dunning_suspended_at=now() WHERE customer_plan_id=$1").bind(plan.customer_plan_id).execute(f.db.pool()).await.unwrap();
    assert!(preview(&f, 10).await.allowed);
    assert_eq!(preview(&f, 12).await.reason, Some("planCreditExceeded"));
    sqlx::query(
        "UPDATE oxsum.customer_plans SET credit_limit_minor=NULL WHERE customer_plan_id=$1",
    )
    .bind(plan.customer_plan_id)
    .execute(f.db.pool())
    .await
    .unwrap();
    assert_eq!(preview(&f, 12).await.reason, Some("restricted"));
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE organization_id=$1").bind(f.reg.organization.id).execute(f.db.pool()).await.unwrap();
    assert_eq!(
        preview(&f, 10).await.reason,
        Some("quotaTransitionRequired")
    );
    sqlx::query("UPDATE oxsum.customer_plans SET state='past_due' WHERE customer_plan_id=$1")
        .bind(plan.customer_plan_id)
        .execute(f.db.pool())
        .await
        .unwrap();
    let before = facts(&f).await;
    let result = preview(&f, 10).await;
    assert!(result.allowed, "{result:?}");
    assert_eq!(result.quota_state, "retained");
    assert_eq!(before, facts(&f).await);
    f.db.cancel_customer_plan(f.reg.organization.id)
        .await
        .unwrap();
    assert_eq!(
        preview(&f, 10).await.reason,
        Some("quotaTransitionRequired")
    );
    assert!(
        preview(&f, 0).await.allowed,
        "free turn requires no quota transition"
    );
}

#[tokio::test]
async fn foreign_member_and_expired_keys_never_expose_funding() {
    let Some(mut f) = fixture().await else { return };
    let other = fixture().await.unwrap();
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    assert!(matches!(
        w.preview_gateway_funding(&f.db, &other.principal, f.reg.api_key.key.id, &f.model, 0)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        w.preview_gateway_funding(&f.db, &f.principal, other.reg.api_key.key.id, &f.model, 0)
            .await,
        Err(WalletError::NotFound(_))
    ));
    sqlx::query("UPDATE oxsum.api_keys SET created_by=$1 WHERE key_id=$2")
        .bind(other.reg.user.id)
        .bind(f.reg.api_key.key.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    if let Principal::Session(ref mut session) = f.principal {
        session.role = Role::Member;
    }
    assert!(matches!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &f.model, 0)
            .await,
        Err(WalletError::NotFound(_))
    ));
    if let Principal::Session(ref mut session) = f.principal {
        session.role = Role::Owner;
    }
    sqlx::query("UPDATE oxsum.api_keys SET expires_at=now()-interval '1 minute' WHERE key_id=$1")
        .bind(f.reg.api_key.key.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    assert!(matches!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &f.model, 0)
            .await,
        Err(WalletError::NotFound(_))
    ));
}

#[tokio::test]
async fn live_promo_funds_but_due_unburned_promo_requires_a_transition() {
    let Some(f) = fixture().await else { return };
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    let _lot =
        f.db.grant_promo_lot(
            &w,
            f.reg.organization.id,
            "grant",
            "preview",
            10,
            Some(OffsetDateTime::now_utc() + time::Duration::hours(1)),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let before = facts(&f).await;
    assert!(preview(&f, 10).await.allowed);
    assert_eq!(before, facts(&f).await);
    f.db.grant_promo_lot(
        &w,
        f.reg.organization.id,
        "grant",
        "due-preview",
        1,
        Some(OffsetDateTime::now_utc() - time::Duration::minutes(1)),
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    let before = facts(&f).await;
    assert_eq!(preview(&f, 1).await.reason, Some("promoExpiryPending"));
    assert_eq!(before, facts(&f).await);
    assert!(preview(&f, 0).await.allowed);
}

#[tokio::test]
async fn a_preexisting_excess_remains_refused_even_when_the_new_turn_is_free() {
    let Some(f) = fixture().await else { return };
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    w.top_up("fund", 100, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    let (_, key) =
        f.db.authenticate(&f.reg.api_key.secret)
            .await
            .unwrap()
            .unwrap();
    w.hold_for_key(
        &key,
        None,
        "held",
        &f.model,
        5,
        OffsetDateTime::now_utc().date(),
        None,
    )
    .await
    .unwrap();
    f.db.update_key_constraints_as(
        &f.principal,
        key.key_id,
        KeyConstraints {
            spend_limit_minor: Some(4),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(preview(&f, 0).await.reason, Some("budgetExceeded"));
    quota(&f, None, true).await;
    assert_eq!(preview(&f, 10).await.reason, Some("budgetExceeded"));
}

#[tokio::test]
async fn preview_does_not_wait_for_an_uncommitted_key_editor() {
    let Some(f) = fixture().await else { return };
    let mut editor = f.db.pool().begin().await.unwrap();
    sqlx::query("UPDATE oxsum.api_keys SET revoked_at=now() WHERE key_id=$1")
        .bind(f.reg.api_key.key.id)
        .execute(&mut *editor)
        .await
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), preview(&f, 0))
        .await
        .expect("a snapshot must not wait for an editor's key row lock");
    assert!(result.allowed);
    editor.commit().await.unwrap();
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    assert!(matches!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &f.model, 0)
            .await,
        Err(WalletError::NotFound(_))
    ));
}

#[tokio::test]
async fn customer_alias_target_scope_and_alias_price_shadowing_match_routes() {
    let Some(f) = fixture().await else { return };
    let alias = format!("alias-{}", Uuid::new_v4().simple());
    let tier = format!("tier-{}", Uuid::new_v4().simple());
    f.db.create_alias(&f.channel, &alias, &f.model)
        .await
        .unwrap();
    let w = f.tenants.get(&f.reg.organization.tenant_id).await.unwrap();
    assert!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &alias, 0)
            .await
            .unwrap()
            .allowed
    );
    f.db.set_tier(&tier, None, Some(vec![alias.clone()]))
        .await
        .unwrap();
    f.db.set_organization_tier(f.reg.organization.id, Some(&tier))
        .await
        .unwrap();
    assert_eq!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &alias, 0)
            .await
            .unwrap()
            .reason,
        Some("modelNotAllowed")
    );
    f.db.set_tier(&tier, None, Some(vec![alias.clone(), f.model.clone()]))
        .await
        .unwrap();
    assert!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &alias, 0)
            .await
            .unwrap()
            .allowed
    );
    // An unsupported event price on the alias shadows its usable token target.
    let price = oxsum_core::PriceBook::from_json(&f.channel, &format!(r#"{{"{alias}":{{"mode":"event","inputPricePerMillion":1000000,"outputPricePerMillion":0,"maxOutputTokens":0}}}}"#)).unwrap();
    f.db.append_price(
        &f.channel,
        &alias,
        price.models().next().unwrap().1.clone(),
        100,
    )
    .await
    .unwrap();
    assert_eq!(
        w.preview_gateway_funding(&f.db, &f.principal, f.reg.api_key.key.id, &alias, 0)
            .await
            .unwrap()
            .reason,
        Some("modelUnavailable")
    );
}
