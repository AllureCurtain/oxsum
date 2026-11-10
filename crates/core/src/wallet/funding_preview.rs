//! Read-only funding previews share admission constraints without taking a reservation.

use serde::Serialize;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::Wallet;
use crate::{ActingKey, CustomerCapability, Db, KeyScope, Principal, WalletError};

/// Funding for one server-priced request bound in the current snapshot.
/// Provider availability, rate windows and policy execution remain separate checks.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFundingPreview {
    /// Whether the recorded funding and shared key constraints cover this bound.
    pub allowed: bool,
    /// Stable refusal/uncertainty class, absent when this funding check passes.
    pub reason: Option<&'static str>,
    /// Total customer price bound supplied by the server's sale resolver.
    pub bound_minor: i64,
    /// Part covered by the current recorded quota window, never a new draw.
    pub quota_minor: i64,
    /// Remainder checked against wallet pools and monetary caps.
    pub wallet_minor: i64,
    /// Current recorded quota policy, including explicit pending boundary work.
    pub quota_state: &'static str,
}

impl Wallet {
    /// Preview one server-priced model bound under a fresh personal customer scope.
    /// This primitive performs no provider I/O, key/row/advisory lock, rollover,
    /// reservation, digest publication or accounting write. The caller must first
    /// open an existing wallet; it must resolve exact customer sale terms separately.
    pub async fn preview_gateway_funding(
        &self,
        db: &Db,
        principal: &Principal,
        key_id: Uuid,
        model: &str,
        bound_minor: i64,
    ) -> Result<GatewayFundingPreview, WalletError> {
        principal.require_customer(CustomerCapability::Personal)?;
        if bound_minor < 0 || crate::channels::model_name(model)? != model {
            return Err(WalletError::InvalidInput(
                "invalid funding preview bound or model".into(),
            ));
        }
        let organization = principal.organization();
        if organization.tenant_id != self.tenant_id {
            return Err(WalletError::NotFound("customer wallet not found".into()));
        }
        let mut tx = db.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let owner = match principal.key_scope() {
            KeyScope::Own(user) => Some(user),
            _ => None,
        };
        let row = sqlx::query(
            "SELECT key_id,spend_limit_minor,requests_per_minute,tokens_per_minute,required_policies \
             FROM oxsum.api_keys WHERE key_id=$1 AND organization_id=$2 \
             AND ($3::uuid IS NULL OR created_by=$3) AND revoked_at IS NULL \
             AND (expires_at IS NULL OR expires_at>now())",
        ).bind(key_id).bind(organization.id).bind(owner).fetch_optional(&mut *tx).await?
            .ok_or_else(|| WalletError::NotFound("live customer key not found".into()))?;
        let key = ActingKey {
            key_id,
            spend_limit_minor: row.try_get("spend_limit_minor")?,
            requests_per_minute: row.try_get("requests_per_minute")?,
            tokens_per_minute: row.try_get("tokens_per_minute")?,
            tokens_per_minute_scopes: None,
            required_policies: row.try_get("required_policies")?,
        };
        let access = db
            .serving_access_in(&mut tx, organization.id, Some(key_id))
            .await?;
        let alias =
            sqlx::query("SELECT channel_id,upstream_model FROM oxsum.model_aliases WHERE alias=$1")
                .bind(model)
                .fetch_optional(&mut *tx)
                .await?;
        let upstream = alias
            .as_ref()
            .map(|row| row.try_get::<String, _>("upstream_model"))
            .transpose()?;
        let mut result = GatewayFundingPreview {
            allowed: false,
            reason: None,
            bound_minor,
            quota_minor: 0,
            wallet_minor: bound_minor,
            quota_state: "none",
        };
        if !access.allows_path(model, upstream.as_deref())
            || alias
                .as_ref()
                .is_some_and(|row| !access.allows_channel(row.get("channel_id")))
        {
            result.reason = Some("modelNotAllowed");
            return Ok(result);
        }
        // An alias uses its own price when present, otherwise its target price.
        // Only a configured permitted route counts; provider health is not probed.
        let route: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.channel_prices p JOIN oxsum.channels c USING(channel_id) \
             WHERE c.business_id=$1 AND c.channel_id=ANY($2) \
             AND ($3::uuid IS NULL OR c.channel_id=$3) AND p.model=ANY($4) \
             AND (p.mode<>'event' OR c.protocol='tasks') AND NOT(c.protocol='tasks' AND $3 IS NOT NULL) \
             AND ($3 IS NULL OR p.model=$5 OR NOT EXISTS(SELECT 1 FROM oxsum.channel_prices own WHERE own.channel_id=p.channel_id AND own.model=$5)) \
             AND p.version=(SELECT MAX(v.version) FROM oxsum.channel_prices v WHERE v.channel_id=p.channel_id AND v.model=p.model))",
        ).bind(access.business_id).bind(access.channel_ids())
            .bind(alias.as_ref().map(|row| row.get::<Uuid,_>("channel_id")))
            .bind([Some(model),upstream.as_deref()].into_iter().flatten().collect::<Vec<_>>())
            .bind(model)
            .fetch_one(&mut *tx).await?;
        if !route {
            result.reason = Some("modelUnavailable");
            return Ok(result);
        }
        let now: OffsetDateTime = sqlx::query_scalar("SELECT now()")
            .fetch_one(&mut *tx)
            .await?;
        if bound_minor > 0 {
            let quota = quota_in(&mut tx, organization.id, bound_minor, now).await?;
            result.quota_minor = quota.drawn;
            result.wallet_minor = bound_minor - quota.drawn;
            result.quota_state = quota.state;
            if let Some(reason) = quota.reason {
                result.reason = Some(reason);
                return Ok(result);
            }
            if result.wallet_minor > 0
                && result.quota_state != "none"
                && let Err(error) = db
                    .check_plan_credit_in(&mut tx, organization.id, result.wallet_minor)
                    .await
            {
                match error {
                    WalletError::InsufficientFunds => {
                        result.reason = Some("planCreditExceeded");
                        return Ok(result);
                    }
                    error => return Err(error),
                }
            }
        }
        if let Err(error) = self
            .authorize_key_snapshot_in(
                &mut tx,
                &key,
                Some(model),
                result.wallet_minor,
                now.date(),
                None,
                false,
            )
            .await
        {
            result.reason = Some(match error {
                WalletError::Unauthenticated => "keyInactive",
                WalletError::Forbidden(_) => "restricted",
                WalletError::KeyLimitExceeded { .. } => "budgetExceeded",
                WalletError::TooManyHolds { .. } => "concurrencyExceeded",
                error => return Err(error),
            });
            return Ok(result);
        }
        if result.wallet_minor > 0 {
            let split = self.pool_split(&mut tx, result.wallet_minor).await?;
            let credit = self.pool_available_in(&mut tx, self.credit_line).await?;
            if split.credit > credit {
                result.reason = Some("insufficientFunds");
                return Ok(result);
            }
            if split.promo > 0 {
                let (live,due):(i64,i64)=sqlx::query_as(
                    "SELECT COALESCE(SUM(remaining_minor) FILTER(WHERE expires_at IS NULL OR expires_at>$2),0)::bigint, \
                     COALESCE(SUM(remaining_minor) FILTER(WHERE expires_at<=$2),0)::bigint FROM oxsum.promo_lots \
                     WHERE organization_id=$1 AND closed_at IS NULL AND remaining_minor>0",
                ).bind(organization.id).bind(now).fetch_one(&mut *tx).await?;
                // Admission pins due-but-unburned lots first. Never claim a
                // stable ready balance while an expiry transition is pending.
                if due > 0 {
                    result.reason = Some("promoExpiryPending");
                    return Ok(result);
                }
                if split.promo > live {
                    result.reason = Some("inconsistentFunding");
                    return Ok(result);
                }
            }
        }
        result.allowed = true;
        tx.commit().await?;
        Ok(result)
    }
}

struct QuotaPreview {
    drawn: i64,
    state: &'static str,
    reason: Option<&'static str>,
}

async fn quota_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    bound: i64,
    now: OffsetDateTime,
) -> Result<QuotaPreview, WalletError> {
    let row=sqlx::query(
        "SELECT s.subscription_id,s.current_period_start,s.current_period_end,s.consumed_minor,s.allow_wallet_overflow,p.quota_minor, \
         cp.state AS plan_state,cp.cancel_at_period_end FROM oxsum.org_subscriptions s \
         JOIN oxsum.subscription_plans p USING(plan_id) LEFT JOIN oxsum.customer_plans cp USING(subscription_id) \
         WHERE s.organization_id=$1 AND s.state='active'",
    ).bind(organization).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(QuotaPreview {
            drawn: 0,
            state: "none",
            reason: None,
        });
    };
    let start: OffsetDateTime = row.try_get("current_period_start")?;
    let end: Option<OffsetDateTime> = row.try_get("current_period_end")?;
    let plan: Option<String> = row.try_get("plan_state")?;
    let cancelled: Option<bool> = row.try_get("cancel_at_period_end")?;
    if start > now {
        return Ok(QuotaPreview {
            drawn: 0,
            state: "notStarted",
            reason: Some("quotaTransitionRequired"),
        });
    }
    let ended = end.is_some_and(|end| end <= now);
    if ended && cancelled == Some(true) {
        return Ok(QuotaPreview {
            drawn: 0,
            state: "cancelledBoundary",
            reason: Some("quotaTransitionRequired"),
        });
    }
    // A fresh boundary close can write a journal and change the debt cap. Only
    // an already recorded past-due/renewal-pending window proves retained quota.
    if ended && !matches!(plan.as_deref(), Some("past_due" | "renewal_pending")) {
        return Ok(QuotaPreview {
            drawn: 0,
            state: "ended",
            reason: Some("quotaTransitionRequired"),
        });
    }
    if plan
        .as_deref()
        .is_some_and(|state| !["active", "past_due", "renewal_pending"].contains(&state))
    {
        return Ok(QuotaPreview {
            drawn: 0,
            state: "inactive",
            reason: Some("inconsistentFunding"),
        });
    }
    let consumed: i64 = row.try_get("consumed_minor")?;
    let quota: Option<i64> = row.try_get("quota_minor")?;
    let drawn = quota.map_or(bound, |quota| {
        quota.saturating_sub(consumed).max(0).min(bound)
    });
    let overflow: bool = row.try_get("allow_wallet_overflow")?;
    Ok(QuotaPreview {
        drawn,
        state: if ended { "retained" } else { "current" },
        reason: if drawn < bound && !overflow {
            Some("quotaExhausted")
        } else {
            None
        },
    })
}
