//! A captured UTC attempt cohort; incomplete historical costs are never guessed.
use super::{SupplierStatementImport, decimal, parser};
use crate::WalletError;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

pub(super) async fn capture(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    source: Uuid,
    request: &SupplierStatementImport,
    parsed: &parser::Parsed,
) -> Result<Value, WalletError> {
    let start = parsed.day.midnight().assume_utc();
    let end = start + time::Duration::days(1);
    // Both durable new-policy attempts and retained legacy attempts are included.
    // A legacy operation's winning usage cannot stand in for each retry's cost.
    let rows=sqlx::query("SELECT a.attempt_id,a.operation_id,a.started_at,a.channel_id,a.base_url,r.supplier_cost_minor,r.closed_at,COALESCE(o.state,'diagnostics_unavailable') AS state FROM oxsum.usage_recovery_attempts a JOIN oxsum.usage_recovery r USING(operation_id) JOIN oxsum.customer_profiles c ON c.organization_id=r.organization_id LEFT JOIN oxsum.request_operations o USING(operation_id) WHERE c.business_id=$1 AND a.channel_id=ANY($2) AND a.started_at >= $3 AND a.started_at < $4 ORDER BY a.started_at,a.attempt_id LIMIT 10001").bind(business).bind(&request.channel_ids).bind(start).bind(end).fetch_all(&mut **tx).await?;
    if rows.len() > 10000 {
        return Err(parser::bad(
            "attempt evidence exceeds 10000 rows; narrow the scope",
        ));
    }
    let mut known = 0_i128;
    let mut known_count = 0_usize;
    let mut unknown = 0_usize;
    let mut pending = 0_usize;
    let mut failed = 0_usize;
    let mut evidence = Vec::new();
    for row in rows {
        let cost: Option<i64> = if row.try_get::<String, _>("base_url")?.trim_end_matches('/')
            == "https://api.openai.com/v1"
        {
            row.try_get("supplier_cost_minor")?
        } else {
            None
        };
        let state: String = row.try_get("state")?;
        if let Some(cost) = cost {
            known = known
                .checked_add(i128::from(cost))
                .ok_or_else(decimal::invalid)?;
            known_count += 1;
        } else {
            unknown += 1;
        }
        if row
            .try_get::<Option<OffsetDateTime>, _>("closed_at")?
            .is_none()
        {
            pending += 1;
        }
        if matches!(state.as_str(), "failed" | "cancelled") {
            failed += 1;
        }
        evidence.push(json!({"attemptId":row.try_get::<Uuid,_>("attempt_id")?,"operationId":row.try_get::<Uuid,_>("operation_id")?,"channelId":row.try_get::<Uuid,_>("channel_id")?,"startedAt":row.try_get::<OffsetDateTime,_>("started_at")?.to_string(),"costMinor":cost.map(|c|c.to_string()),"basis":if cost.is_some(){"pinned_supplier_rates_and_valid_report"}else{"unknown"},"state":state}));
    }
    let legacy=sqlx::query("SELECT a.attempt_id,a.operation_id,a.started_at,a.status,ch.channel_id FROM oxsum.request_attempts a JOIN oxsum.request_operations o USING(operation_id) JOIN oxsum.customer_profiles c ON c.organization_id=o.organization_id JOIN oxsum.channels ch ON ch.name=a.channel AND ch.business_id=c.business_id WHERE c.business_id=$1 AND ch.channel_id=ANY($2) AND a.started_at >= $3 AND a.started_at < $4 AND NOT EXISTS(SELECT 1 FROM oxsum.usage_recovery_attempts r WHERE r.attempt_id=a.attempt_id) ORDER BY a.started_at,a.attempt_id LIMIT 10001").bind(business).bind(&request.channel_ids).bind(start).bind(end).fetch_all(&mut **tx).await?;
    if evidence.len() + legacy.len() > 10000 {
        return Err(parser::bad("attempt evidence exceeds 10000 rows"));
    }
    let legacy_count = legacy.len();
    for row in legacy {
        unknown += 1;
        let status: String = row.try_get("status")?;
        if matches!(status.as_str(), "refused" | "unreachable") {
            failed += 1;
        }
        if status == "started" {
            pending += 1;
        }
        evidence.push(json!({"attemptId":row.try_get::<Uuid,_>("attempt_id")?,"operationId":row.try_get::<Uuid,_>("operation_id")?,"channelId":row.try_get::<Uuid,_>("channel_id")?,"startedAt":row.try_get::<OffsetDateTime,_>("started_at")?.to_string(),"costMinor":Value::Null,"basis":"legacy_attempt_has_no_durable_supplier_rate_context","state":status}));
    }
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM oxsum.channels WHERE business_id=$1 AND channel_id=ANY($2)",
    )
    .bind(business)
    .bind(&request.channel_ids)
    .fetch_all(&mut **tx)
    .await?;
    // Missing attempts and cache rows have only a diagnostic settlement-time
    // cohort, explicitly separate from attempt-day matching. Deletion gaps
    // cannot be proven absent, so aggregate completeness is never asserted.
    let (missing,cache):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER (WHERE u.upstream_attempts>0 AND NOT EXISTS(SELECT 1 FROM oxsum.request_attempts a WHERE a.operation_id::text=u.request_id) AND NOT EXISTS(SELECT 1 FROM oxsum.usage_recovery_attempts a WHERE a.operation_id::text=u.request_id)),count(*) FILTER (WHERE u.cost_basis='cache_reuse') FROM oxsum.usage_records u JOIN oxsum.organizations o USING(tenant_id) JOIN oxsum.customer_profiles c USING(organization_id) WHERE c.business_id=$1 AND u.channel=ANY($2) AND u.settled_at >= $3 AND u.settled_at < $4").bind(business).bind(&names).bind(start).bind(end).fetch_one(&mut **tx).await?;
    let mut operations = std::collections::BTreeMap::<Uuid, usize>::new();
    for attempt in &evidence {
        let id = attempt["operationId"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| parser::bad("invalid captured identity"))?;
        *operations.entry(id).or_default() += 1;
    }
    let retries: usize = operations
        .values()
        .map(|count| count.saturating_sub(1))
        .sum();
    let missing_operations:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.request_operations o JOIN oxsum.customer_profiles c USING(organization_id) WHERE c.business_id=$1 AND o.channel=ANY($2) AND o.created_at >= $3 AND o.created_at < $4 AND COALESCE(o.outcome_kind,'')!='cache_hit' AND NOT EXISTS(SELECT 1 FROM oxsum.usage_records u WHERE u.request_id=o.operation_id::text AND u.cost_basis='cache_reuse') AND NOT EXISTS(SELECT 1 FROM oxsum.usage_recovery r WHERE r.operation_id=o.operation_id AND r.attempt_id IS NULL AND r.closed_at IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM oxsum.request_attempts a WHERE a.operation_id=o.operation_id) AND NOT EXISTS(SELECT 1 FROM oxsum.usage_recovery_attempts a WHERE a.operation_id=o.operation_id)").bind(business).bind(&names).bind(start).bind(end).fetch_one(&mut **tx).await?;
    let missing_attempts:i64=sqlx::query_scalar("SELECT COALESCE(sum(GREATEST(u.upstream_attempts-(SELECT count(*) FROM (SELECT attempt_id FROM oxsum.request_attempts a WHERE a.operation_id::text=u.request_id UNION SELECT attempt_id FROM oxsum.usage_recovery_attempts a WHERE a.operation_id::text=u.request_id) retained),0)),0)::bigint FROM oxsum.usage_records u JOIN oxsum.organizations o USING(tenant_id) JOIN oxsum.customer_profiles c USING(organization_id) WHERE c.business_id=$1 AND u.channel=ANY($2) AND u.settled_at >= $3 AND u.settled_at < $4").bind(business).bind(&names).bind(start).bind(end).fetch_one(&mut **tx).await?;
    // Local minor is credit/1e6. Keep conversion remainder instead of losing it.
    let numerator = known
        .checked_mul(parsed.conversion)
        .ok_or_else(decimal::invalid)?;
    let converted = numerator / 1_000_000;
    let conversion_remainder = numerator % 1_000_000;
    let delta = parsed
        .total
        .checked_sub(converted)
        .ok_or_else(decimal::invalid)?;
    let captured: OffsetDateTime = sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    Ok(
        json!({"schemaVersion":1,"algorithmVersion":"attempt-utc-day-known-cost-exact18-v1","sourceId":source,"capturedAt":captured.to_string(),"billDate":request.bill_date,"providerAmountUsd":decimal::text(parsed.total),"knownLocalCostMinor":known.to_string(),"usdPerCredit":request.usd_per_credit,"knownLocalCostUsd":decimal::text(converted),"conversionRemainderNumerator":conversion_remainder.to_string(),"conversionRemainderDenominator":"1000000000000000000000000","deltaResidualNumerator":(-conversion_remainder).to_string(),"deltaAgainstKnownUsd":decimal::text(delta),"amountRelation":if delta==0 && conversion_remainder==0{"equal_to_known_estimate"}else{"different_from_known_estimate"},"coverage":{"knownAttempts":known_count,"unknownAttempts":unknown,"legacyAttempts":legacy_count,"pendingAttempts":pending,"failedOrCancelledAttempts":failed,"diagnosticRowsMissingAttempts":missing,"diagnosticCacheRows":cache,"retryAttempts":retries,"operationsWithoutAttempts":missing_operations,"diagnosticUnrecordedAttempts":missing_attempts,"complete":false,"scopeBasis":"operator-attested channel mapping; attempt-start UTC cohort; diagnostic missing/cache counts use settlement UTC cohort","limitations":["Daily aggregate comparison does not prove request matching or complete provider coverage","Historical diagnostic deletion and traffic outside oxsum cannot be ruled out","Legacy attempts lack durable supplier rates; their costs remain unknown","Retry count covers additional attempts within the selected channel/day cohort only","Amounts may agree while unknown exposure remains"]},"attempts":evidence}),
    )
}
