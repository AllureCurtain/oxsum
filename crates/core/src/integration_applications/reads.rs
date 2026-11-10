//! Coherent application entitlement and model/sale reads with no domain writes.

use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    ActingService, ApplicationCustomerEntitlement, ApplicationCustomerModel,
    ApplicationCustomerModelPage, ApplicationCustomerSaleTerms, ApplicationPlanStanding,
    ApplicationQuotaStanding, CustomerPlanState, Db, IntegrationScope, PlanCreditStanding,
    WalletError, channels, integration_credentials, plan_debt, sale_books,
};

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    organization: Uuid,
) -> Result<(Uuid, OffsetDateTime), WalletError> {
    // Authorization retains the existing revocation lock. PostgreSQL prohibits
    // FOR SHARE in READ ONLY transactions; this transaction issues no domain DML.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut **tx)
        .await?;
    integration_credentials::require_integration(
        tx,
        service,
        IntegrationScope::EntitlementsRead,
        Some(organization),
    )
    .await?;
    let application = service
        .application_id
        .ok_or_else(|| WalletError::Forbidden("application credential required".into()))?;
    let now = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut **tx)
        .await?;
    Ok((application, now))
}

async fn plan_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
) -> Result<(Option<ApplicationPlanStanding>, Option<Uuid>), WalletError> {
    let row = sqlx::query(
        "SELECT customer_plan_id,plan_version_id,subscription_id,state,\
         cancel_at_period_end,scheduled_change,credit_limit_minor FROM oxsum.customer_plans \
         WHERE organization_id=$1 ORDER BY \
         (state IN ('pending_payment','active','renewal_pending','past_due')) DESC,\
         created_at DESC,customer_plan_id DESC LIMIT 1",
    )
    .bind(organization)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok((None, None));
    };
    let id = row.try_get("customer_plan_id")?;
    let (outstanding_minor, next_due_at) = plan_debt::standing_in(tx, id).await?;
    Ok((
        Some(ApplicationPlanStanding {
            customer_plan_id: id,
            plan_version_id: row.try_get("plan_version_id")?,
            state: CustomerPlanState::parse(row.try_get("state")?)
                .ok_or_else(|| WalletError::Misconfigured("unknown customer plan state".into()))?,
            cancel_at_period_end: row.try_get("cancel_at_period_end")?,
            scheduled_change: row.try_get("scheduled_change")?,
            credit: PlanCreditStanding {
                credit_limit_minor: row.try_get("credit_limit_minor")?,
                outstanding_minor,
                next_due_at,
            },
        }),
        row.try_get("subscription_id")?,
    ))
}

async fn quota_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    retained_subscription: Option<Uuid>,
    at: OffsetDateTime,
) -> Result<Option<ApplicationQuotaStanding>, WalletError> {
    let row = sqlx::query(
        "SELECT s.subscription_id,s.state,s.current_period_start,s.current_period_end,\
         s.quota_minor,s.consumed_minor,s.allow_wallet_overflow,\
         (SELECT p.plan_period_id FROM oxsum.plan_periods p \
          JOIN oxsum.customer_plans c USING(customer_plan_id) \
          WHERE c.subscription_id=s.subscription_id AND c.organization_id=s.organization_id \
            AND p.period_start=s.current_period_start AND p.period_end=s.current_period_end \
          ORDER BY p.period_index DESC LIMIT 1) AS plan_period_id \
         FROM oxsum.org_subscriptions s WHERE s.organization_id=$1 \
         AND (s.state='active' OR s.subscription_id=$2) \
         ORDER BY (s.state='active') DESC,s.created_at DESC,s.subscription_id DESC LIMIT 1",
    )
    .bind(organization)
    .bind(retained_subscription)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let period_start = row.try_get("current_period_start")?;
    let period_end: Option<OffsetDateTime> = row.try_get("current_period_end")?;
    let quota: Option<i64> = row.try_get("quota_minor")?;
    let consumed: i64 = row.try_get("consumed_minor")?;
    Ok(Some(ApplicationQuotaStanding {
        subscription_id: row.try_get("subscription_id")?,
        plan_period_id: row.try_get("plan_period_id")?,
        state: row.try_get("state")?,
        window_state: if period_start > at {
            "notStarted"
        } else if period_end.is_some_and(|end| end <= at) {
            "ended"
        } else {
            "current"
        },
        period_start,
        period_end,
        quota_minor: quota,
        consumed_minor: consumed,
        remaining_recorded_minor: quota.map(|limit| limit.saturating_sub(consumed).max(0)),
        allow_wallet_overflow: row.try_get("allow_wallet_overflow")?,
    }))
}

impl Db {
    /// Read recorded customer entitlement and shared debt without rolling or reserving quota.
    pub async fn application_customer_entitlement(
        &self,
        service: &ActingService,
        organization: Uuid,
    ) -> Result<ApplicationCustomerEntitlement, WalletError> {
        let mut tx = self.pool().begin().await?;
        let (application_id, as_of) = authorize(&mut tx, service, organization).await?;
        let customer = sqlx::query(
            "SELECT c.state,o.suspended_at IS NOT NULL AS manually_suspended,\
             o.dunning_suspended_at IS NOT NULL AS dunning_suspended \
             FROM oxsum.customer_profiles c JOIN oxsum.organizations o USING(organization_id) \
             WHERE c.organization_id=$1 AND c.business_id=$2",
        )
        .bind(organization)
        .bind(service.business_id)
        .fetch_one(&mut *tx)
        .await?;
        let (plan, retained_subscription) = plan_in(&mut tx, organization).await?;
        let quota = quota_in(&mut tx, organization, retained_subscription, as_of).await?;
        let result = ApplicationCustomerEntitlement {
            application_id,
            organization_id: organization,
            as_of,
            customer_state: customer.try_get("state")?,
            manually_suspended: customer.try_get("manually_suspended")?,
            organization_dunning_suspended: customer.try_get("dunning_suspended")?,
            plan_overage_suspended: plan_debt::admission_suspended_in(&mut tx, organization)
                .await?,
            outstanding_plan_debt_minor: plan_debt::organization_outstanding_in(
                &mut tx,
                organization,
            )
            .await?,
            plan,
            quota,
        };
        tx.commit().await?;
        Ok(result)
    }

    /// Read bounded configured public names and their actual published customer sale terms.
    /// An optional key must be live and issued within this exact application/customer.
    pub async fn application_customer_models(
        &self,
        service: &ActingService,
        organization: Uuid,
        key: Option<Uuid>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<ApplicationCustomerModelPage, WalletError> {
        if !(1..=50).contains(&limit) {
            return Err(WalletError::InvalidInput("limit must be 1 to 50".into()));
        }
        if let Some(after) = after
            && channels::model_name(after)? != after
        {
            return Err(WalletError::InvalidInput(
                "after must be a normalized model name".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let (application_id, as_of) = authorize(&mut tx, service, organization).await?;
        if let Some(key) = key {
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM oxsum.application_customer_keys a \
                 JOIN oxsum.api_keys k USING(key_id,organization_id) \
                 WHERE a.business_id=$1 AND a.application_id=$2 AND a.organization_id=$3 \
                 AND a.key_id=$4 AND k.revoked_at IS NULL \
                 AND (k.expires_at IS NULL OR k.expires_at>clock_timestamp()))",
            )
            .bind(service.business_id)
            .bind(application_id)
            .bind(organization)
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
            if !owned {
                return Err(WalletError::NotFound(
                    "application key is not known or live".into(),
                ));
            }
        }
        let access = self.serving_access_in(&mut tx, organization, key).await?;
        let names = access.public_models_in(&mut tx, after, limit + 1).await?;
        let more = names.len() > limit as usize;
        let mut models = Vec::with_capacity(limit as usize);
        for model in names.into_iter().take(limit as usize) {
            let terms = sale_books::read_effective_in(
                &mut tx,
                service.business_id,
                organization,
                &model,
                as_of,
            )
            .await?;
            let sale_terms = terms
                .map(|terms| {
                    Ok::<_, WalletError>(ApplicationCustomerSaleTerms {
                        source: terms.source,
                        schedule_id: terms.schedule_id.ok_or_else(|| {
                            WalletError::Misconfigured("published sale lacks schedule".into())
                        })?,
                        book_id: terms.book_id,
                        version: terms.version,
                        effective_from: terms.effective_from,
                        effective_until: terms.effective_until,
                        terms: terms.terms,
                    })
                })
                .transpose()?;
            models.push(ApplicationCustomerModel { model, sale_terms });
        }
        let next_cursor = more
            .then(|| models.last().map(|row| row.model.clone()))
            .flatten();
        tx.commit().await?;
        Ok(ApplicationCustomerModelPage {
            application_id,
            organization_id: organization,
            key_id: key,
            as_of,
            models,
            next_cursor,
        })
    }
}
