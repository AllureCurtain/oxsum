//! Reservation, durable dispatch, terminal handoff and claim fencing.
use super::{
    ObservedTextUsage, RecoveryClaim, RecoveryDispatch, RecoveryLookup, UsageRecoveryReceipt,
};
use crate::{Db, GatewayBillingTerms, SecretKey, WalletError};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

pub(super) fn conflict() -> WalletError {
    WalletError::Conflict("usage recovery ownership changed".into())
}
pub(super) fn observation(row: &sqlx::postgres::PgRow) -> Result<ObservedTextUsage, WalletError> {
    serde_json::from_value(row.try_get("observed")?).map_err(crate::error::invalid)
}
impl Db {
    pub(crate) async fn reserve_usage_recovery_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        terms: &GatewayBillingTerms,
        owner: &str,
        mandate: Option<&crate::PresentedMandate>,
    ) -> Result<(), WalletError> {
        if !terms.provider_reported() {
            return Ok(());
        }
        let window = terms
            .selection
            .policy
            .as_ref()
            .ok_or_else(conflict)?
            .evidence_window_secs;
        sqlx::query("INSERT INTO oxsum.usage_recovery(operation_id,organization_id,hold_key,execution_owner,window_secs,mandate_hash) VALUES($1,$2,$3,$4,$5,$6)").bind(terms.operation_id).bind(terms.organization_id).bind(&terms.hold_key).bind(owner).bind(window).bind(mandate.map(|m|&m.verified.hash)).execute(&mut **tx).await?;
        Ok(())
    }
    /// Persist exactly one attempted dispatch, before any paid POST can leave.
    pub async fn begin_recovery_attempt(
        &self,
        operation: Uuid,
        owner: &str,
        dispatch: &RecoveryDispatch<'_>,
        secret: &SecretKey,
    ) -> Result<Uuid, WalletError> {
        let mut tx = self.pool().begin().await?;
        let row =
            sqlx::query("SELECT * FROM oxsum.usage_recovery WHERE operation_id=$1 FOR UPDATE")
                .bind(operation)
                .fetch_one(&mut *tx)
                .await?;
        if row.try_get::<String, _>("execution_owner")? != owner
            || row.try_get::<Option<Uuid>, _>("attempt_id")?.is_some()
            || row
                .try_get::<Option<OffsetDateTime>, _>("terminal_at")?
                .is_some()
        {
            return Err(conflict());
        }
        let organization: Uuid = row.try_get("organization_id")?;
        let terms = self
            .operation_billing_terms_in(&mut tx, organization, operation)
            .await?
            .ok_or_else(conflict)?;
        let route = terms
            .routes
            .get(dispatch.route_index)
            .ok_or_else(conflict)?;
        if route.channel_id != dispatch.serving.channel_id
            || route.price_version != dispatch.serving.version
        {
            return Err(conflict());
        }
        crate::channels::lock_configuration(&mut tx).await?;
        let current: Option<(String, OffsetDateTime)> =
            sqlx::query_as("SELECT base_url,updated_at FROM oxsum.channels WHERE channel_id=$1")
                .bind(route.channel_id)
                .fetch_optional(&mut *tx)
                .await?;
        if current
            != Some((
                dispatch.serving.base_url.clone(),
                dispatch.serving.configuration_updated_at,
            ))
            || Self::pool_fingerprint(&mut tx, route.channel_id, secret).await?
                != dispatch.serving.credential_configuration
        {
            return Err(conflict());
        }
        let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.channel_credentials WHERE channel_id=$1 AND credential_id=$2 AND status='active')").bind(route.channel_id).bind(dispatch.credential_id).fetch_one(&mut *tx).await?;
        if !live {
            return Err(conflict());
        }
        let policy = crate::ProviderDestinationPolicy::from_settings(&dispatch.serving.settings)?;
        policy.endpoint(&dispatch.serving.base_url)?;
        let lease:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.request_operations WHERE operation_id=$1 AND lease_owner=$2 AND state IN ('leased','running') AND leased_until>clock_timestamp())").bind(operation).bind(owner).fetch_one(&mut *tx).await?;
        if !lease {
            return Err(conflict());
        }
        let attempt = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.request_attempts(attempt_id,operation_id,channel,credential_id) VALUES($1,$2,$3,$4)").bind(attempt).bind(operation).bind(&route.channel).bind(dispatch.credential_id).execute(&mut *tx).await?;
        let lookup = dispatch.stored
            && dispatch.serving.base_url.trim_end_matches('/') == "https://api.openai.com/v1";
        sqlx::query("INSERT INTO oxsum.usage_recovery_attempts(attempt_id,operation_id,route_index,channel_id,credential_id,base_url,configuration_updated_at,credential_configuration,destination_policy,supplier_price,lookup_supported,experiment) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)").bind(attempt).bind(operation).bind(dispatch.route_index as i64).bind(route.channel_id).bind(dispatch.credential_id).bind(&dispatch.serving.base_url).bind(dispatch.serving.configuration_updated_at).bind(&dispatch.serving.credential_configuration).bind(serde_json::to_value(policy).map_err(crate::error::invalid)?).bind(serde_json::to_value(&dispatch.serving.price).map_err(crate::error::invalid)?).bind(lookup).bind(dispatch.experiment.map(serde_json::to_value).transpose().map_err(crate::error::invalid)?).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE oxsum.usage_recovery SET route_index=$2,attempt_id=$3 WHERE operation_id=$1",
        )
        .bind(operation)
        .bind(dispatch.route_index as i64)
        .bind(attempt)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(attempt)
    }
    /// First terminal observation transfers execution to durable financial recovery.
    /// Repeated observations cannot move the deadline or replace earlier evidence.
    pub async fn observe_recovery_terminal(
        &self,
        operation: Uuid,
        owner: &str,
        kind: &str,
        observed: &ObservedTextUsage,
        guardrails: &[crate::SettlementGuardrailOwned],
    ) -> Result<(), WalletError> {
        if ![
            "answered",
            "cancelled",
            "upstream_error",
            "withheld",
            "worker_lost",
        ]
        .contains(&kind)
        {
            return Err(WalletError::InvalidInput(
                "invalid terminal observation".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let row =
            sqlx::query("SELECT * FROM oxsum.usage_recovery WHERE operation_id=$1 FOR UPDATE")
                .bind(operation)
                .fetch_one(&mut *tx)
                .await?;
        if row
            .try_get::<Option<OffsetDateTime>, _>("terminal_at")?
            .is_some()
        {
            return Ok(());
        }
        if row.try_get::<String, _>("execution_owner")? != owner
            || row.try_get::<Option<Uuid>, _>("claim_token")?.is_some()
        {
            return Err(conflict());
        }
        let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.request_operations WHERE operation_id=$1 AND lease_owner=$2 AND state IN ('leased','running'))").bind(operation).bind(owner).fetch_one(&mut *tx).await?;
        if !live {
            return Err(conflict());
        }
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("UPDATE oxsum.usage_recovery SET terminal_at=$2,deadline=$2+window_secs*interval '1 second',terminal_observation=$3,observed=$4,next_lookup_at=$2,guardrails=$5 WHERE operation_id=$1").bind(operation).bind(now).bind(kind).bind(serde_json::to_value(observed).map_err(crate::error::invalid)?).bind(serde_json::to_value(guardrails.iter().map(|g|serde_json::json!({"policy":g.policy,"verdict":g.verdict,"reason":g.reason,"phase":g.phase,"streamed":g.streamed})).collect::<Vec<_>>()).map_err(crate::error::invalid)?).execute(&mut *tx).await?;
        sqlx::query("UPDATE oxsum.request_attempts SET external_id=$2,usage_json=$3,completed_at=$4,status=CASE WHEN status='refused' THEN status ELSE $5 END WHERE attempt_id=$1").bind(row.try_get::<Option<Uuid>,_>("attempt_id")?).bind(&observed.completion_id).bind(serde_json::to_value(observed).map_err(crate::error::invalid)?).bind(now).bind(if observed.eligible {"answered"} else {"unreachable"}).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Bounded due queue; executing requests enter only after their heartbeat lease expires.
    pub async fn due_usage_recovery(&self, limit: i64) -> Result<Vec<Uuid>, WalletError> {
        Ok(sqlx::query_scalar("SELECT r.operation_id FROM oxsum.usage_recovery r JOIN oxsum.request_operations o USING(operation_id) WHERE r.closed_at IS NULL AND (r.claim_until IS NULL OR r.claim_until<clock_timestamp()) AND ((r.terminal_at IS NOT NULL AND (r.next_lookup_at<=clock_timestamp() OR r.deadline<=clock_timestamp())) OR (r.terminal_at IS NULL AND o.leased_until<clock_timestamp())) ORDER BY COALESCE(r.deadline,o.leased_until) LIMIT $1").bind(limit.clamp(1,64)).fetch_all(self.pool()).await?)
    }
    /// Acquire a fresh generation; a worker returning after expiry cannot settle.
    pub async fn claim_usage_recovery(
        &self,
        operation: Uuid,
    ) -> Result<Option<RecoveryClaim>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT * FROM oxsum.usage_recovery WHERE operation_id=$1 FOR UPDATE SKIP LOCKED",
        )
        .bind(operation)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        if row
            .try_get::<Option<OffsetDateTime>, _>("closed_at")?
            .is_some()
            || row
                .try_get::<Option<OffsetDateTime>, _>("claim_until")?
                .is_some_and(|t| t > now)
        {
            return Ok(None);
        }
        let mut deadline: Option<OffsetDateTime> = row.try_get("deadline")?;
        if deadline.is_none() {
            let expired:bool=sqlx::query_scalar("SELECT leased_until<clock_timestamp() AND state IN ('leased','running') FROM oxsum.request_operations WHERE operation_id=$1 FOR UPDATE").bind(operation).fetch_one(&mut *tx).await?;
            if !expired {
                return Ok(None);
            }
            let end = now + time::Duration::seconds(row.try_get("window_secs")?);
            sqlx::query("UPDATE oxsum.usage_recovery SET terminal_at=$2,deadline=$3,terminal_observation='worker_lost' WHERE operation_id=$1").bind(operation).bind(now).bind(end).execute(&mut *tx).await?;
            deadline = Some(end);
        }
        let deadline = deadline.ok_or_else(conflict)?;
        let token = Uuid::new_v4();
        sqlx::query("UPDATE oxsum.usage_recovery SET claim_token=$2,claim_until=clock_timestamp()+interval '45 seconds' WHERE operation_id=$1").bind(operation).bind(token).execute(&mut *tx).await?;
        // Keep the normal open state so usage-anchor watermarks retain pending finance.
        sqlx::query("UPDATE oxsum.request_operations SET state='running',lease_owner=$2,leased_until=clock_timestamp()+interval '45 seconds' WHERE operation_id=$1 AND state IN ('leased','running')").bind(operation).bind(token.to_string()).execute(&mut *tx).await?;
        let observed = observation(&row)?;
        let supported:bool=sqlx::query_scalar("SELECT COALESCE((SELECT lookup_supported FROM oxsum.usage_recovery_attempts WHERE operation_id=$1),false)").bind(operation).fetch_one(&mut *tx).await?;
        let result = RecoveryClaim {
            operation_id: operation,
            organization_id: row.try_get("organization_id")?,
            token,
            deadline,
            needs_lookup: supported && observed.completion_id.is_some() && now < deadline,
            observed,
        };
        tx.commit().await?;
        Ok(Some(result))
    }
    /// Resolve only the original configuration and credential, never a replacement.
    pub async fn recovery_lookup(
        &self,
        claim: &RecoveryClaim,
        secret: &SecretKey,
    ) -> Result<Option<RecoveryLookup>, WalletError> {
        if !claim.needs_lookup {
            return Ok(None);
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let row=sqlx::query("SELECT a.*,c.base_url AS current_url,c.updated_at,c.settings,cr.api_key_sealed,cr.status FROM oxsum.usage_recovery_attempts a JOIN oxsum.channels c USING(channel_id) JOIN oxsum.channel_credentials cr ON cr.credential_id=a.credential_id AND cr.channel_id=a.channel_id WHERE a.operation_id=$1").bind(claim.operation_id).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let base: String = row.try_get("base_url")?;
        if base.trim_end_matches('/') != "https://api.openai.com/v1"
            || base != row.try_get::<String, _>("current_url")?
            || row.try_get::<OffsetDateTime, _>("configuration_updated_at")?
                != row.try_get::<OffsetDateTime, _>("updated_at")?
            || row.try_get::<String, _>("status")? != "active"
            || Self::pool_fingerprint(&mut tx, row.try_get("channel_id")?, secret).await?
                != row.try_get::<String, _>("credential_configuration")?
        {
            return Ok(None);
        }
        Ok(Some(RecoveryLookup {
            base_url: base,
            policy: crate::ProviderDestinationPolicy::from_settings(&row.try_get("settings")?)?,
            api_key: secret.open(&row.try_get::<String, _>("api_key_sealed")?)?,
            completion_id: claim.observed.completion_id.clone().ok_or_else(conflict)?,
        }))
    }
    /// ReportsRead queue scoped through the Business's durable customer ownership.
    pub async fn business_usage_recovery(
        &self,
        token: &str,
        closed_unknown: bool,
        before: Option<Uuid>,
    ) -> Result<Vec<super::UsageRecoveryQueueRow>, WalletError> {
        let principal = self
            .business_capability(token, crate::BusinessCapability::ReportsRead)
            .await?;
        let rows = sqlx::query("SELECT r.operation_id,r.organization_id FROM oxsum.usage_recovery r JOIN oxsum.customer_profiles c ON c.organization_id=r.organization_id WHERE c.business_id=$1 AND (($2 AND r.closed_at IS NOT NULL AND r.supplier_cost_minor IS NULL) OR (NOT $2 AND r.closed_at IS NULL)) AND ($3::uuid IS NULL OR r.operation_id<$3) ORDER BY r.operation_id DESC LIMIT 50").bind(principal.business_id).bind(closed_unknown).bind(before).fetch_all(self.pool()).await?;
        let mut result = Vec::new();
        for row in rows {
            let operation = row.try_get("operation_id")?;
            let organization = row.try_get("organization_id")?;
            let terms = self
                .operation_billing_terms(organization, operation)
                .await?
                .ok_or_else(conflict)?;
            if let Some(recovery) = self.usage_recovery_receipt(organization, operation).await? {
                result.push(super::UsageRecoveryQueueRow {
                    operation_id: operation,
                    organization_id: organization,
                    model: terms.requested_model,
                    ceiling_minor: terms.ceiling_minor,
                    recovery,
                });
            }
        }
        Ok(result)
    }
    /// Customer-safe state, available after authorization of the original request.
    pub async fn usage_recovery_receipt(
        &self,
        organization: Uuid,
        operation: Uuid,
    ) -> Result<Option<UsageRecoveryReceipt>, WalletError> {
        let row = sqlx::query(
            "SELECT * FROM oxsum.usage_recovery WHERE operation_id=$1 AND organization_id=$2",
        )
        .bind(operation)
        .bind(organization)
        .fetch_optional(self.pool())
        .await?;
        row.map(|r| {
            let terminal: Option<OffsetDateTime> = r.try_get("terminal_at")?;
            let closed: Option<OffsetDateTime> = r.try_get("closed_at")?;
            Ok(UsageRecoveryReceipt {
                state: if closed.is_some() {
                    "closed"
                } else if terminal.is_some() {
                    "pending"
                } else {
                    "executing"
                }
                .into(),
                terminal_observation: r.try_get("terminal_observation")?,
                terminal_at: terminal.map(|t| t.to_string()),
                deadline: r
                    .try_get::<Option<OffsetDateTime>, _>("deadline")?
                    .map(|t| t.to_string()),
                closed_at: closed.map(|t| t.to_string()),
                close_reason: r.try_get("close_reason")?,
                observed: observation(&r)?,
                waived: serde_json::from_value(r.try_get("waived")?)
                    .map_err(crate::error::invalid)?,
                supplier_cost_known: r
                    .try_get::<Option<i64>, _>("supplier_cost_minor")?
                    .is_some(),
            })
        })
        .transpose()
    }
}
