//! Transaction ownership, immutable attempts and authority for independent reconciliation.

use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::types::{Comparison, ReconciliationCoverage, ReconciliationSnapshot, Scope};
use super::{compare, types};
use crate::business_staff::{self as authority, BusinessCapability};
use crate::{Db, ReconciliationReport, ReconciliationRun, TimeBasis, WalletError, error};

fn basis_name(basis: TimeBasis) -> &'static str {
    match basis {
        TimeBasis::Booking => "booking",
        TimeBasis::Service => "service",
        TimeBasis::Payment => "payment",
    }
}

fn validate(kind: &str, start: Date, end: Date) -> Result<(), WalletError> {
    if !matches!(kind, "daily" | "period_close" | "manual") {
        return Err(error::invalid(
            "reconciliation kind must be daily|period_close|manual",
        ));
    }
    if end < start || (end - start).whole_days() > 365 {
        return Err(error::invalid(
            "reconciliation interval must contain 1 to 366 days",
        ));
    }
    Ok(())
}

async fn claim_in(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    scope: Scope,
) -> Result<(), WalletError> {
    let identity = format!(
        "oxsum/reconciliation/{kind}/{}/{}/{}",
        scope.start,
        scope.end,
        basis_name(scope.basis)
    );
    let claimed: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(identity)
            .fetch_one(&mut **tx)
            .await?;
    if claimed {
        Ok(())
    } else {
        Err(WalletError::PoolBusy)
    }
}

async fn run_in(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    scope: Scope,
    actor: Option<Uuid>,
) -> Result<ReconciliationRun, WalletError> {
    validate(kind, scope.start, scope.end)?;
    sqlx::query("SET LOCAL TIME ZONE 'UTC'")
        .execute(&mut **tx)
        .await?;
    let captured: OffsetDateTime = sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let organizations:Vec<(Uuid,String)>=sqlx::query_as("SELECT organization_id,tenant_id FROM oxsum.organizations ORDER BY organization_id LIMIT $1")
        .bind((types::ORGANIZATION_LIMIT+1) as i64).fetch_all(&mut **tx).await?;
    if organizations.len() > types::ORGANIZATION_LIMIT {
        return Err(error::invalid("reconciliation organization limit exceeded"));
    }
    let mut result = Comparison::new(scope.basis);
    for (organization, tenant) in organizations {
        compare::customer(tx, organization, &tenant, scope, &mut result).await?;
        if serde_json::to_vec(&result.evidence)
            .map_err(error::invalid)?
            .len()
            > types::BYTE_LIMIT
        {
            return Err(error::invalid(
                "reconciliation evidence byte limit exceeded",
            ));
        }
        let missing:Vec<String>=sqlx::query_scalar("SELECT request_id FROM oxsum.usage_records WHERE tenant_id=$1 AND status='settled' AND upstream_cost_minor IS NULL AND cost_basis IS DISTINCT FROM 'cache_reuse' AND (settled_at AT TIME ZONE 'UTC')::date BETWEEN $2 AND $3 ORDER BY request_id LIMIT $4")
            .bind(&tenant).bind(scope.start).bind(scope.end).bind((types::SOURCE_LIMIT+1) as i64).fetch_all(&mut **tx).await?;
        if missing.len() > types::SOURCE_LIMIT {
            return Err(error::invalid(
                "reconciliation missing-cost source limit exceeded",
            ));
        }
        if result.differences.len() + missing.len() > types::SOURCE_LIMIT {
            return Err(error::invalid(
                "reconciliation global difference limit exceeded",
            ));
        }
        for request in missing {
            result.differences.push(types::Difference {organization_id:organization,kind:"missing_cost".into(),source_table:"usage_records".into(),source_ref:request,fact:"upstreamCost".into(),expected:None,actual:None,detail:"Settled turn has no tracked upstream cost; financial source comparison does not certify provider cost".into()});
        }
    }
    let expected = serde_json::to_value(&result.expected).map_err(error::invalid)?;
    let actual = serde_json::to_value(&result.actual).map_err(error::invalid)?;
    let coverage = serde_json::to_value(&result.coverage).map_err(error::invalid)?;
    let evidence = serde_json::json!({"version":1,"capturedAt":captured.format(&time::format_description::well_known::Rfc3339).map_err(error::invalid)?,"sources":result.evidence,"differences":result.differences});
    if serde_json::to_vec(&evidence).map_err(error::invalid)?.len() > types::BYTE_LIMIT {
        return Err(error::invalid(
            "reconciliation evidence byte limit exceeded",
        ));
    }
    let count = i64::try_from(result.differences.len()).map_err(error::invalid)?;
    let state = if count == 0 && result.coverage.complete {
        "matched"
    } else {
        "exceptions"
    };
    let run:Uuid=sqlx::query_scalar("INSERT INTO oxsum.reconciliation_runs(run_id,kind,period_start,period_end,time_basis,state,requested_by,started_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(kind,period_start,period_end,time_basis) DO UPDATE SET state=EXCLUDED.state,requested_by=EXCLUDED.requested_by,started_at=EXCLUDED.started_at,last_error=NULL RETURNING run_id")
        .bind(Uuid::new_v4()).bind(kind).bind(scope.start).bind(scope.end).bind(basis_name(scope.basis)).bind(state).bind(actor).bind(captured).fetch_one(&mut **tx).await?;
    let snapshot = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.reconciliation_snapshots(snapshot_id,run_id,state,expected_totals,actual_totals,coverage,exception_count,evidence,captured_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(snapshot).bind(run).bind(state).bind(&expected).bind(&actual).bind(&coverage).bind(count).bind(&evidence).bind(captured).execute(&mut **tx).await?;
    for difference in &result.differences {
        sqlx::query("INSERT INTO oxsum.reconciliation_exceptions(exception_id,run_id,organization_id,kind,source_table,source_ref,fact,expected_minor,actual_minor,detail) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(organization_id,kind,source_table,source_ref,fact) WHERE state='open' AND source_ref IS NOT NULL DO UPDATE SET run_id=EXCLUDED.run_id,expected_minor=EXCLUDED.expected_minor,actual_minor=EXCLUDED.actual_minor,detail=EXCLUDED.detail")
            .bind(Uuid::new_v4()).bind(run).bind(difference.organization_id).bind(&difference.kind).bind(&difference.source_table).bind(&difference.source_ref).bind(&difference.fact).bind(difference.expected).bind(difference.actual).bind(&difference.detail).execute(&mut **tx).await?;
    }
    let row=sqlx::query("UPDATE oxsum.reconciliation_runs SET expected_totals=$2,actual_totals=$3,coverage=$4,exception_count=$5,latest_snapshot_id=$6,finished_at=clock_timestamp() WHERE run_id=$1 RETURNING *")
        .bind(run).bind(expected).bind(actual).bind(coverage).bind(count).bind(snapshot).fetch_one(&mut **tx).await?;
    crate::financial_facts::run_from_row(&row)
}

impl Db {
    /// Capture a complete independent comparison on one repeatable-read transaction.
    /// A busy identical run refuses without retaining a connection waiting on its lock.
    /// Storage/bounds failures roll back without replacing previously retained evidence.
    pub async fn run_reconciliation_with_basis(
        &self,
        kind: &str,
        start: Date,
        end: Date,
        basis: TimeBasis,
        actor: Option<Uuid>,
    ) -> Result<ReconciliationReport, WalletError> {
        validate(kind, start, end)?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        claim_in(&mut tx, kind, Scope { start, end, basis }).await?;
        let run = run_in(&mut tx, kind, Scope { start, end, basis }, actor).await?;
        tx.commit().await?;
        Ok(ReconciliationReport {
            run_id: run.run_id,
            exceptions: run.exception_count,
            matched: run.state == "matched",
        })
    }

    /// Run a manually requested comparison with current finance authority and exact replay.
    /// One committed request returns its original captured result, even after later runs.
    pub async fn request_reconciliation_run_with_basis(
        &self,
        token: &str,
        start: Date,
        end: Date,
        basis: TimeBasis,
        key: &str,
    ) -> Result<ReconciliationRun, WalletError> {
        validate("manual", start, end)?;
        authority::validate(key, None)?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        claim_in(&mut tx, "manual", Scope { start, end, basis }).await?;
        let business: Uuid = sqlx::query_scalar(
            "SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(WalletError::PoolBusy)?;
        let actor =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        if let Some(row)=sqlx::query("SELECT period_start,period_end,time_basis,result FROM oxsum.reconciliation_requests WHERE business_id=$1 AND idempotency_key=$2").bind(business).bind(key).fetch_optional(&mut *tx).await? {
            if row.try_get::<Date,_>("period_start")?!=start || row.try_get::<Date,_>("period_end")?!=end || row.try_get::<String,_>("time_basis")?!=basis_name(basis) {return Err(WalletError::Conflict("reconciliation request key already binds different terms".into()));}
            return serde_json::from_value(row.try_get("result")?).map_err(error::invalid);
        }
        let result = run_in(
            &mut tx,
            "manual",
            Scope { start, end, basis },
            Some(actor.user_id),
        )
        .await?;
        let audit = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES($1,$2,$3,'reconciliation.repair',$4,$5,$6)")
            .bind(audit).bind(business).bind(actor.user_id).bind(result.run_id.to_string()).bind(serde_json::json!({"kind":"manual","periodStart":start.to_string(),"periodEnd":end.to_string(),"timeBasis":basis_name(basis),"snapshotId":result.latest_snapshot_id})).bind(format!("reconciliation-run/{key}")).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO oxsum.reconciliation_requests(business_id,idempotency_key,period_start,period_end,time_basis,run_id,snapshot_id,audit_ref,result) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(business).bind(key).bind(start).bind(end).bind(basis_name(basis)).bind(result.run_id).bind(result.latest_snapshot_id).bind(audit).bind(serde_json::to_value(&result).map_err(error::invalid)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Read retained attempts under current ReportsRead authority, newest first.
    pub async fn reconciliation_snapshots(
        &self,
        token: &str,
        run: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<ReconciliationSnapshot>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let rows=sqlx::query("SELECT snapshot_id,run_id,state,expected_totals,actual_totals,coverage,exception_count,captured_at,NULL::jsonb AS evidence FROM oxsum.reconciliation_snapshots WHERE run_id=$1 AND ($2::uuid IS NULL OR (captured_at,snapshot_id)<(SELECT captured_at,snapshot_id FROM oxsum.reconciliation_snapshots WHERE run_id=$1 AND snapshot_id=$2)) ORDER BY captured_at DESC,snapshot_id DESC LIMIT $3")
            .bind(run).bind(after).bind(limit.clamp(1,200)).fetch_all(&mut *tx).await?;
        let snapshots = rows
            .iter()
            .map(snapshot_from_row)
            .collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(snapshots)
    }

    /// Read the exact sources, differences and verified postings of a retained attempt.
    pub async fn reconciliation_snapshot(
        &self,
        token: &str,
        run: Uuid,
        snapshot: Uuid,
    ) -> Result<Option<ReconciliationSnapshot>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let row = sqlx::query(
            "SELECT * FROM oxsum.reconciliation_snapshots WHERE run_id=$1 AND snapshot_id=$2",
        )
        .bind(run)
        .bind(snapshot)
        .fetch_optional(&mut *tx)
        .await?;
        let snapshot = row.as_ref().map(snapshot_from_row).transpose()?;
        tx.commit().await?;
        Ok(snapshot)
    }
}

fn snapshot_from_row(row: &PgRow) -> Result<ReconciliationSnapshot, WalletError> {
    Ok(ReconciliationSnapshot {
        snapshot_id: row.try_get("snapshot_id")?,
        run_id: row.try_get("run_id")?,
        state: row.try_get("state")?,
        expected_totals: row.try_get("expected_totals")?,
        actual_totals: row.try_get("actual_totals")?,
        coverage: serde_json::from_value::<ReconciliationCoverage>(row.try_get("coverage")?)
            .map_err(error::invalid)?,
        exception_count: row.try_get("exception_count")?,
        captured_at: row.try_get("captured_at")?,
        evidence: row.try_get("evidence")?,
    })
}
