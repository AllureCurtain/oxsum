//! One fenced commit for financial closure; no late evidence can reopen it.
use super::store::{conflict, observation};
use super::{ObservedTextUsage, RecoveryClaim, price_observed_text};
use crate::{
    CountsProvenance, Db, Settlement, SettlementKind, SettlementSubscription, Tenants, UsageRow,
    WalletError, wallet,
};
use sqlx::Row;
use time::OffsetDateTime;

impl Db {
    /// Apply a correlated GET result or close existing evidence. `unavailable` closes early.
    /// Returns no amount while waiting; an expired/replaced claim writes nothing.
    pub async fn finish_usage_recovery(
        &self,
        tenants: &Tenants,
        claim: &RecoveryClaim,
        recovered: Option<&ObservedTextUsage>,
        unavailable: bool,
    ) -> Result<Option<i64>, WalletError> {
        let terms = self
            .operation_billing_terms(claim.organization_id, claim.operation_id)
            .await?
            .ok_or_else(conflict)?;
        let org = self.organization_by_id(claim.organization_id).await?;
        let wallet = tenants.get(&org.tenant_id).await?;
        let mut tx = self.pool().begin().await?;
        wallet.lock_key_settlement_in(&mut tx, terms.key_id).await?;
        let row =
            sqlx::query("SELECT * FROM oxsum.usage_recovery WHERE operation_id=$1 FOR UPDATE")
                .bind(claim.operation_id)
                .fetch_one(&mut *tx)
                .await?;
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        if row
            .try_get::<Option<OffsetDateTime>, _>("closed_at")?
            .is_some()
            || row.try_get::<Option<uuid::Uuid>, _>("claim_token")? != Some(claim.token)
            || row
                .try_get::<Option<OffsetDateTime>, _>("claim_until")?
                .is_none_or(|t| t <= now)
        {
            return Err(conflict());
        }
        let deadline: OffsetDateTime = row.try_get("deadline")?;
        let mut observed = observation(&row)?;
        if now <= deadline
            && let Some(report) = recovered
        {
            if report.completion_id != observed.completion_id || report.completion_id.is_none() {
                return Err(WalletError::InvalidInput(
                    "recovered completion identity mismatch".into(),
                ));
            }
            if report.eligible {
                observed.merge(report);
            }
        }
        let route_index = row.try_get::<Option<i64>, _>("route_index")?.unwrap_or(0) as usize;
        let route = terms.routes.get(route_index).ok_or_else(conflict)?;
        let hold = self
            .open_hold_in(&mut tx, &terms.hold_key)
            .await?
            .ok_or_else(conflict)?;
        let terminal: String = row.try_get("terminal_observation")?;
        let billable = matches!(terminal.as_str(), "answered" | "cancelled") && observed.eligible;
        let (mut usage, itemized, waived) = price_observed_text(
            &route.price,
            &observed,
            hold.service_tier.as_deref(),
            billable,
        )?;
        if billable && !waived.is_empty() && claim.needs_lookup && !unavailable && now < deadline {
            sqlx::query("UPDATE oxsum.usage_recovery SET observed=$2,claim_token=NULL,claim_until=NULL,next_lookup_at=LEAST(deadline,clock_timestamp()+interval '15 seconds'),lookup_attempts=lookup_attempts+1 WHERE operation_id=$1").bind(claim.operation_id).bind(serde_json::to_value(&observed).map_err(crate::error::invalid)?).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(None);
        }
        let (charged, capped) =
            itemized.capped_minor(terms.discount_percent.unwrap_or(0), terms.ceiling_minor)?;
        let kind = if capped {
            SettlementKind::Capped
        } else if terminal == "cancelled" {
            SettlementKind::ClientCancelled
        } else if billable {
            SettlementKind::Usage
        } else {
            SettlementKind::Released
        };
        let close_reason = if waived.is_empty() {
            "provider_components_complete"
        } else if now >= deadline {
            "evidence_deadline_expired_unresolved_waived"
        } else {
            "lookup_unavailable_unresolved_waived"
        };
        usage.end_user = hold.end_user.clone();
        usage.tags = hold.tags.clone();
        usage.usage_details = Some(
            serde_json::json!({"providerEvidence":{"observed":observed,"waived":waived,"terminalObservation":terminal,"closeReason":close_reason,"policyDigest":terms.selection.policy.as_ref().map(|p|&p.digest)}}),
        );
        let draw=sqlx::query("SELECT subscription_id,period_id,drawn_minor FROM oxsum.subscription_draws WHERE hold_key=$1").bind(&terms.hold_key).fetch_optional(&mut *tx).await?;
        let sub = draw
            .map(|d| -> Result<_, WalletError> {
                Ok((
                    d.try_get::<uuid::Uuid, _>("subscription_id")?,
                    d.try_get::<uuid::Uuid, _>("period_id")?,
                    d.try_get::<i64, _>("drawn_minor")?,
                ))
            })
            .transpose()?;
        let sub_charge = sub.as_ref().map_or(0, |s| s.2.min(charged));
        self.settle_subscription_draw_in(&mut tx, &terms.hold_key, sub_charge)
            .await?;
        let request = claim.operation_id.to_string();
        let reason = format!(
            "provider-reported-text-v1: {close_reason}; unresolved components are waived, not observed zero usage"
        );
        let attempt_id: Option<uuid::Uuid> = row.try_get("attempt_id")?;
        let mandate_hash: Option<String> = row.try_get("mandate_hash")?;
        let guardrails: serde_json::Value = row.try_get("guardrails")?;
        let guardrail_owned: Vec<crate::SettlementGuardrailOwned> = guardrails
            .as_array()
            .ok_or_else(conflict)?
            .iter()
            .map(|g| -> Result<_, WalletError> {
                Ok(crate::SettlementGuardrailOwned {
                    policy: g
                        .get("policy")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(conflict)?
                        .into(),
                    reason: g
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(conflict)?
                        .into(),
                    verdict: match g.get("verdict").and_then(serde_json::Value::as_str) {
                        Some("blocked") => "blocked",
                        Some("flagged") => "flagged",
                        _ => return Err(conflict()),
                    },
                    phase: match g.get("phase").and_then(serde_json::Value::as_str) {
                        Some("pre_call") => "pre_call",
                        Some("post_call") => "post_call",
                        _ => return Err(conflict()),
                    },
                    streamed: g
                        .get("streamed")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                })
            })
            .collect::<Result<_, _>>()?;
        crate::retain_reason_evidence(&mut usage, &guardrail_owned, Some(&reason));
        let experiment: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT experiment FROM oxsum.usage_recovery_attempts WHERE operation_id=$1",
        )
        .bind(claim.operation_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        let experiment_id = experiment
            .as_ref()
            .and_then(|e| e.get("experiment_id"))
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<uuid::Uuid>().ok());
        if let Some(id) = experiment_id
            && let Some(variant) = experiment
                .as_ref()
                .and_then(|e| e.get("variant"))
                .and_then(serde_json::Value::as_str)
            && let Some(details) = usage
                .usage_details
                .as_mut()
                .and_then(serde_json::Value::as_object_mut)
        {
            details.insert(
                "experiment".into(),
                serde_json::json!({"id":id,"variant":variant}),
            );
        }
        let description = Settlement {
            request: &request,
            channel: &route.channel,
            model: &route.model,
            requested_model: &terms.requested_model,
            served_model: &route.model,
            upstream_model: route.upstream_model.as_deref(),
            price_version: route.price_version,
            kind,
            usage: &usage,
            lines: &itemized.lines,
            matched_rule: itemized.matched_rule.as_ref(),
            discount_percent: terms.discount_percent.filter(|d| *d > 0),
            charged,
            freeze: terms.ceiling_minor,
            upstream_attempts: i64::from(attempt_id.is_some()),
            mandate: hold
                .mandate_id
                .as_ref()
                .zip(mandate_hash.as_ref())
                .map(|(id, hash)| crate::SettlementMandate { id, hash }),
            subscription: sub
                .as_ref()
                .map(|(id, period, drawn)| SettlementSubscription {
                    subscription_id: id,
                    period_id: period,
                    drawn_minor: *drawn,
                    charged_minor: sub_charge,
                }),
            cache: None,
            guardrail: guardrail_owned.iter().map(|g| g.borrowed()).collect(),
            reason: Some(&reason),
            experiment: experiment_id
                .as_ref()
                .zip(
                    experiment
                        .as_ref()
                        .and_then(|e| e.get("variant"))
                        .and_then(serde_json::Value::as_str),
                )
                .map(|(id, variant)| crate::SettlementExperiment { id, variant }),
            sale: terms
                .sale_digest
                .as_ref()
                .map(|d| crate::SettlementSale { digest: d }),
        }
        .description()?;
        crate::validate_gateway_description(&description)?;
        let held: i64 =
            sqlx::query_scalar("SELECT wallet_minor FROM oxsum.open_holds WHERE hold_key=$1")
                .bind(&terms.hold_key)
                .fetch_one(&mut *tx)
                .await?;
        if held > 0 {
            wallet
                .settle_in(
                    &mut tx,
                    &terms.hold_key,
                    &description,
                    charged - sub_charge,
                    now.date(),
                )
                .await?;
        } else {
            wallet
                .usage_receipt_in(
                    &mut tx,
                    &terms.hold_key,
                    &description,
                    Some(&terms.key_id.simple().to_string()),
                    hold.mandate_id.map(|id| id.simple().to_string()).as_deref(),
                    now.date(),
                )
                .await?;
        }
        let closed=sqlx::query("UPDATE oxsum.request_operations SET state=$3,charged_minor=$4,outcome_kind=$5,completed_at=$6,updated_at=$6,leased_until=NULL WHERE operation_id=$1 AND lease_owner=$2 AND state IN ('leased','running')").bind(claim.operation_id).bind(claim.token.to_string()).bind(if terminal=="cancelled" {"cancelled"}else if billable{"succeeded"}else{"failed"}).bind(charged).bind(kind.as_str()).bind(now).execute(&mut *tx).await?;
        if closed.rows_affected() != 1 {
            return Err(conflict());
        }
        let supplier = sqlx::query(
            "SELECT supplier_price FROM oxsum.usage_recovery_attempts WHERE operation_id=$1",
        )
        .bind(claim.operation_id)
        .fetch_optional(&mut *tx)
        .await?;
        let supplier_cost = if let Some(supplier) = supplier {
            let price: crate::Price = serde_json::from_value(supplier.try_get("supplier_price")?)
                .map_err(crate::error::invalid)?;
            super::evidence::supplier_cost(&price, &observed, hold.service_tier.as_deref()).unwrap_or_else(|error|{tracing::warn!(%error,"supplier estimate unavailable; customer closure continues");None})
        } else {
            None
        };
        let usage_row = UsageRow {
            request_id: request,
            tenant_id: org.tenant_id,
            key_id: Some(terms.key_id),
            mandate_id: hold.mandate_id,
            model: route.model.clone(),
            channel: route.channel.clone(),
            price_version: route.price_version,
            kind,
            entry_id: *wallet::entry_id_for(&wallet::settlement_key_for(&terms.hold_key)).as_uuid(),
            status: kind.status().into(),
            failure_reason: Some(reason),
            usage,
            counts: CountsProvenance::None,
            charged_minor: charged,
            freeze_minor: terms.ceiling_minor,
            upstream_cost_minor: supplier_cost,
            cost_basis: if supplier_cost.is_some() {
                crate::CostBasis::RealAttempt
            } else {
                crate::CostBasis::Missing
            },
            upstream_attempts: i64::from(attempt_id.is_some()),
        };
        let available = wallet.available_in(&mut tx).await?;
        self.record_usage_in(&mut tx, &usage_row, Some(available))
            .await?;
        // Presence-aware provenance does not manufacture zero for a missing side.
        sqlx::query("UPDATE oxsum.usage_records SET provider_input_tokens=$2,provider_output_tokens=$3 WHERE request_id=$1").bind(claim.operation_id.to_string()).bind(observed.input).bind(observed.output).execute(&mut *tx).await?;
        sqlx::query("UPDATE oxsum.usage_recovery SET observed=$2,closed_at=$3,close_reason=$4,waived=$5,supplier_cost_minor=$6,claim_until=NULL WHERE operation_id=$1").bind(claim.operation_id).bind(serde_json::to_value(&observed).map_err(crate::error::invalid)?).bind(now).bind(close_reason).bind(serde_json::to_value(&waived).map_err(crate::error::invalid)?).bind(supplier_cost).execute(&mut *tx).await?;
        sqlx::query("UPDATE oxsum.request_attempts SET completed_at=COALESCE(completed_at,$2),usage_json=$3,status=CASE WHEN status='refused' THEN status WHEN $4 THEN 'answered' ELSE 'unreachable' END,error=COALESCE(error,$5) WHERE attempt_id=$1").bind(attempt_id).bind(now).bind(serde_json::to_value(&observed).map_err(crate::error::invalid)?).bind(observed.eligible).bind(if terminal=="worker_lost" {Some("worker lost; external execution and consumption unknown")}else{None}).execute(&mut *tx).await?;
        self.clear_open_hold_in(&mut tx, &terms.hold_key).await?;
        tx.commit().await?;
        let log = crate::RequestLogRow {
            request_id: claim.operation_id.to_string(),
            tenant_id: usage_row.tenant_id.clone(),
            organization_id: claim.organization_id,
            entry_id: Some(usage_row.entry_id),
            key_id: Some(terms.key_id),
            model_requested: terms.requested_model.clone(),
            model_served: route.model.clone(),
            channel: route.channel.clone(),
            admission_outcome: "admitted".into(),
            status: usage_row.status.clone(),
            ttft_ms: None,
            total_ms: None,
            input_tokens: usage_row.usage.input_tokens,
            output_tokens: usage_row.usage.output_tokens,
            cached_tokens: usage_row.usage.cached_tokens,
            charge_minor: charged,
            upstream_attempts: usage_row.upstream_attempts,
            error_kind: (!waived.is_empty()).then(|| close_reason.into()),
            http_status: None,
            created_at: now,
        };
        if let Err(error) = self.record_request_log(&log).await {
            tracing::warn!(%error,"recovered request diagnostic write failed");
        }
        Ok(Some(charged))
    }
}
