//! Historical published versions for v1 regression/upgrade fixtures.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, NewSoldPlan, SoldPlan};
use uuid::Uuid;

/// Insert the pre-policy publisher's original version through normal guards.
/// Tests retain legacy v1 semantics without changing today's v2 publisher.
pub async fn publish(
    db: &Db,
    business: Uuid,
    new: &NewSoldPlan,
) -> Result<SoldPlan, oxsum_core::WalletError> {
    let plan = new.plan_id.unwrap_or_else(Uuid::new_v4);
    let mut tx = db.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("sold-plan:{plan}"))
        .execute(&mut *tx)
        .await?;
    let version: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version),0)+1 FROM oxsum.sold_plans WHERE plan_id=$1",
    )
    .bind(plan)
    .fetch_one(&mut *tx)
    .await?;
    let id = Uuid::new_v4();
    // No new policy field appears in this historical INSERT. Its migration
    // default identifies it as v1; no published row is rewritten.
    sqlx::query("INSERT INTO oxsum.sold_plans(plan_version_id,plan_id,business_id,version,name,price_minor,currency,quota_minor,period_kind,overage_enabled,sort_order,published_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,now())")
        .bind(id).bind(plan).bind(business).bind(version).bind(&new.name).bind(new.price_minor).bind(&new.currency).bind(new.quota_minor).bind(new.period_kind.as_str()).bind(new.overage_enabled).bind(new.sort_order).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(SoldPlan {
        accounting_policy_version: 1,
        plan_version_id: id,
        plan_id: plan,
        business_id: business,
        version,
        name: new.name.clone(),
        price_minor: new.price_minor,
        currency: new.currency.clone(),
        quota_minor: new.quota_minor,
        period_kind: new.period_kind,
        overage_enabled: new.overage_enabled,
        sort_order: new.sort_order,
        published_at: Some(time::OffsetDateTime::now_utc()),
        retired_at: None,
        created_at: time::OffsetDateTime::now_utc(),
    })
}
