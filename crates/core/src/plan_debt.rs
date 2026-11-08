//! Shared plan debt projections, including retained document-only voids.

use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::WalletError;

// A live statement surfaces its own debt. Old document-only voids left the
// journal posted: retain that exact remaining source rather than hiding it from
// admission. Its latest retained due date is authoritative; a missing document
// falls back to the journal's posted time plus the original plan's net terms.
const DEBTS: &str = "WITH plan_debts AS (\
    SELECT s.customer_plan_id,s.organization_id,\
           GREATEST(s.total_minor-s.paid_minor,0) AS outstanding,s.due_at \
      FROM oxsum.plan_statements s WHERE s.state IN ('issued','past_due') \
    UNION ALL \
    SELECT j.customer_plan_id,j.organization_id,\
           GREATEST(j.total_minor-j.settled_minor,0),\
           COALESCE((SELECT s.due_at FROM oxsum.plan_statements s \
                     WHERE s.journal_id=j.journal_id AND s.state='void' \
                     ORDER BY s.created_at DESC,s.statement_id DESC LIMIT 1),\
                    COALESCE(j.posted_at,j.created_at)+(p.net_terms_days*interval '1 day')) \
      FROM oxsum.invoice_journals j \
      JOIN oxsum.customer_plans p USING(customer_plan_id) \
     WHERE j.state='posted' AND NOT EXISTS(\
         SELECT 1 FROM oxsum.plan_statements s WHERE s.journal_id=j.journal_id \
           AND s.state<>'void')\
) ";

pub(crate) async fn standing_in(
    connection: &mut sqlx::PgConnection,
    plan: Uuid,
) -> Result<(i64, Option<OffsetDateTime>), WalletError> {
    let row = sqlx::query(&format!(
        "{DEBTS} SELECT COALESCE(SUM(outstanding),0)::bigint AS outstanding,\
         MIN(due_at) FILTER(WHERE outstanding>0) AS next_due \
         FROM plan_debts WHERE customer_plan_id=$1"
    ))
    .bind(plan)
    .fetch_one(connection)
    .await?;
    Ok((row.try_get("outstanding")?, row.try_get("next_due")?))
}

pub(crate) async fn organization_outstanding_in(
    connection: &mut sqlx::PgConnection,
    organization: Uuid,
) -> Result<i64, WalletError> {
    Ok(sqlx::query_scalar(&format!(
        "{DEBTS} SELECT COALESCE(SUM(outstanding),0)::bigint FROM plan_debts \
         WHERE organization_id=$1"
    ))
    .bind(organization)
    .fetch_one(connection)
    .await?)
}

/// The caller owns the plan before touching a journal or statement. Only this
/// debt flag is changed: manual restrictions and renewal never change here.
pub(crate) async fn refresh_in(
    tx: &mut Transaction<'_, Postgres>,
    plan: Uuid,
) -> Result<(), WalletError> {
    sqlx::query(&format!(
        "{DEBTS} UPDATE oxsum.customer_plans p SET dunning_suspended_at=\
         CASE WHEN EXISTS(SELECT 1 FROM plan_debts d WHERE d.customer_plan_id=p.customer_plan_id \
           AND d.outstanding>0 AND d.due_at+(p.grace_days*interval '1 day')<=now()) \
         THEN COALESCE(p.dunning_suspended_at,now()) ELSE NULL END,updated_at=now() \
         WHERE p.customer_plan_id=$1"
    ))
    .bind(plan)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn admission_suspended_in(
    connection: &mut sqlx::PgConnection,
    organization: Uuid,
) -> Result<bool, WalletError> {
    Ok(sqlx::query_scalar(&format!(
        "{DEBTS} SELECT EXISTS(SELECT 1 FROM oxsum.customer_plans p \
         WHERE p.organization_id=$1 AND p.state IN ('pending_payment','active','renewal_pending','past_due') \
           AND (p.dunning_suspended_at IS NOT NULL OR EXISTS(\
             SELECT 1 FROM plan_debts d WHERE d.customer_plan_id=p.customer_plan_id \
               AND d.outstanding>0 AND d.due_at+(p.grace_days*interval '1 day')<=now())))"
    ))
    .bind(organization)
    .fetch_one(connection)
    .await?)
}
