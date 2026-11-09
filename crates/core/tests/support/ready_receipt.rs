//! Real canonical settlement and accounting fixtures for first-call readiness.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    BillLine, CostBasis, CountsProvenance, Db, NewUser, Principal, Settlement, SettlementKind,
    Tenants, UsageRecord, UsageRow,
};
use time::OffsetDateTime;
use uuid::Uuid;

pub struct Fixture {
    pub db: Db,
    pub tenants: Tenants,
    pub registration: oxsum_core::Registration,
    pub principal: Principal,
    pub cookie: String,
}

pub async fn fixture(suite: &str) -> Option<Fixture> {
    let url = oxsum_testkit::suite_db_url(suite)?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "ready receipt password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let session = db
        .login(&registration.user.email, "ready receipt password")
        .await
        .unwrap()
        .into_session();
    Some(Fixture {
        tenants: Tenants::new(db.pool().clone()),
        db,
        registration,
        principal: Principal::Session(session.principal.unwrap()),
        cookie: session.token,
    })
}

pub async fn turn(
    f: &Fixture,
    kind: SettlementKind,
    charged: i64,
    reason: Option<&str>,
) -> UsageRow {
    turn_for(
        f,
        kind,
        charged,
        reason,
        f.registration.api_key.key.id,
        None,
    )
    .await
}

pub async fn turn_for(
    f: &Fixture,
    kind: SettlementKind,
    charged: i64,
    reason: Option<&str>,
    key: Uuid,
    altered: Option<&str>,
) -> UsageRow {
    let request = Uuid::new_v4().simple().to_string();
    let hold = format!("req-{request}:hold");
    let day = OffsetDateTime::now_utc().date();
    let wallet = f
        .tenants
        .get(&f.registration.organization.tenant_id)
        .await
        .unwrap();
    let usage = UsageRecord::tokens(3, 2).unwrap();
    let lines = [
        BillLine {
            item: "input".into(),
            units: 3,
            price_per_m: if charged == 0 { 0 } else { 1_000_000 },
        },
        BillLine {
            item: "output".into(),
            units: 2,
            price_per_m: 0,
        },
    ];
    let mut description = Settlement {
        request: &request,
        channel: "fixture",
        model: "model",
        requested_model: "model",
        served_model: "model",
        upstream_model: None,
        price_version: 1,
        kind,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged,
        freeze: if charged == 0 { 0 } else { 100 },
        upstream_attempts: 1,
        mandate: None,
        subscription: None,
        cache: None,
        guardrail: Vec::new(),
        reason,
        experiment: None,
        sale: None,
    }
    .description()
    .unwrap();
    if let Some(altered) = altered {
        let mut value: serde_json::Value = serde_json::from_str(&description).unwrap();
        match altered {
            "arithmetic" => value["charged"] = 9.into(),
            "blocked" => {
                value["guardrail"] = serde_json::json!([{"policy":"post", "verdict":"blocked", "reason":"blocked", "phase":"post_call"}])
            }
            "request" => value["request"] = "other-request".into(),
            _ => panic!("unknown fixture alteration"),
        }
        description = value.to_string();
    }
    let receipt = if charged == 0 {
        wallet
            .usage_receipt(
                &hold,
                &description,
                Some(&key.simple().to_string()),
                None,
                day,
            )
            .await
            .unwrap()
    } else {
        wallet
            .top_up(&format!("fund-{request}"), 100, day)
            .await
            .unwrap();
        let acting =
            f.db.authenticate(&f.registration.api_key.secret)
                .await
                .unwrap()
                .unwrap()
                .1;
        wallet
            .hold_for_key(&acting, None, &hold, "model", 100, day, None)
            .await
            .unwrap();
        wallet
            .settle(&hold, &description, charged, day)
            .await
            .unwrap()
    };
    let row = UsageRow {
        request_id: request,
        tenant_id: f.registration.organization.tenant_id.clone(),
        key_id: Some(key),
        mandate_id: None,
        model: "model".into(),
        channel: "fixture".into(),
        price_version: 1,
        kind,
        entry_id: *receipt.entry_id.as_uuid(),
        status: kind.status().into(),
        failure_reason: reason.map(str::to_owned),
        usage,
        counts: CountsProvenance::Provider {
            input: 3,
            output: 2,
        },
        charged_minor: charged,
        freeze_minor: if charged == 0 { 0 } else { 100 },
        upstream_cost_minor: None,
        cost_basis: CostBasis::Missing,
        upstream_attempts: 1,
    };
    f.db.record_usage(&row).await.unwrap();
    row
}
