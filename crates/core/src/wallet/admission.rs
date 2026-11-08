//! Current-key authority and atomic gateway reservations, including no-money turns.

use doubleentry::Provenance;
use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::{ORG_ADMISSION_LOCK, Wallet};
use crate::{
    ActingKey, BudgetDuration, Db, NewOperation, OpenHold, PresentedMandate, RequestOperation,
    ServingAccess, SubscriptionAdmission, WalletError,
};

/// A committed gateway reservation. A zero wallet amount still owns a concurrency slot.
#[derive(Debug)]
pub struct GatewayAdmission {
    /// Durable parent for this particular request, already bound to its watch.
    pub operation: RequestOperation,
    /// The authorized subscription share, if any.
    pub subscription: SubscriptionAdmission,
    /// The monetary reservation made in the wallet pools; zero writes no monetary hold.
    pub wallet_minor: i64,
}

impl Wallet {
    pub(super) async fn lock_key_admission_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        key: Uuid,
    ) -> Result<(), WalletError> {
        self.lock_key_settlement_in(tx, key).await?;
        sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended($2 || organization_id::text,0)) FROM oxsum.api_keys WHERE key_id=$1")
            .bind(key).bind(ORG_ADMISSION_LOCK).execute(&mut **tx).await?;
        Ok(())
    }

    /// Serialize a gateway settlement with this key's admission before taking
    /// subscription, ledger or usage-forecast row locks.
    pub async fn lock_key_settlement_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        key: Uuid,
    ) -> Result<(), WalletError> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(super::limit_lock_key(&self.tenant_id, &key))
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Reserve the quota, monetary share, concurrency slot and operation together.
    /// The caller supplies authenticated coordinates and a server-priced bound.
    /// A rejected admission leaves none of those writes behind.
    #[allow(clippy::too_many_arguments)]
    pub async fn admit_gateway(
        &self,
        db: &Db,
        key: &ActingKey,
        watch: &OpenHold,
        operation: &NewOperation,
        access: &ServingAccess,
        description: &str,
        lease_owner: &str,
        on: Date,
        mandate: Option<&PresentedMandate>,
    ) -> Result<GatewayAdmission, WalletError> {
        if watch.freeze_minor < 0
            || watch.tenant_id != self.tenant_id
            || watch.key_id != Some(key.key_id)
            || operation.key_id != Some(key.key_id)
            || operation.bound_minor != watch.freeze_minor
            || operation.model != watch.model
            || operation.operation_id.to_string() != watch.request_id
            || watch.hold_key != format!("req-{}:hold", operation.operation_id)
            || watch.mandate_id != mandate.map(|mandate| mandate.verified.mandate_id)
            || access.customer_id != operation.organization_id
            || access.key_id != Some(key.key_id)
        {
            return Err(WalletError::InvalidInput(
                "inconsistent gateway reservation".into(),
            ));
        }
        let _queue = self.key_queue_wait(&key.key_id).await?;
        let mut tx = db.pool().begin().await?;
        // All acquisition precedes row locks. Subscription lifecycle may own a
        // plan/journal/ledger, so it runs before the shared key guard's rows.
        self.lock_key_admission_in(&mut tx, key.key_id).await?;
        let belongs: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.api_keys k JOIN oxsum.organizations o USING(organization_id) WHERE k.key_id=$1 AND o.organization_id=$2 AND o.tenant_id=$3)")
            .bind(key.key_id).bind(operation.organization_id).bind(&self.tenant_id)
            .fetch_one(&mut *tx).await?;
        if !belongs {
            return Err(WalletError::Unauthenticated);
        }
        let subscription = db
            .admit_subscription_draw_in(
                &mut tx,
                operation.organization_id,
                &watch.hold_key,
                watch.freeze_minor,
                Some(self),
            )
            .await?;
        let wallet_minor = watch.freeze_minor - subscription.drawn_minor;
        // The subscription's lazy rollover can already own the ledger append
        // lock. Every admission therefore takes append before budget rows;
        // settlements follow the same order before their usage forecast.
        self.lock_append_in(&mut tx).await?;
        self.authorize_key_in(&mut tx, key, Some(&watch.model), wallet_minor, on, mandate)
            .await?;
        if &db
            .serving_access_in(&mut tx, operation.organization_id, Some(key.key_id))
            .await?
            != access
        {
            return Err(WalletError::Forbidden(
                "serving permissions changed during admission; retry the request".into(),
            ));
        }
        let (parent, fresh) = db.insert_operation_in(&mut tx, operation).await?;
        if !fresh {
            // HTTP idempotency resolves replays before this boundary. A second
            // attempt must not execute an already reserved or completed parent.
            return Err(WalletError::Conflict(
                "the request operation already exists".into(),
            ));
        }
        let occupied: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.open_holds WHERE hold_key=$1)")
                .bind(&watch.hold_key)
                .fetch_one(&mut *tx)
                .await?;
        if occupied {
            return Err(WalletError::Conflict(
                "the reservation is already watched".into(),
            ));
        }
        if wallet_minor > 0 {
            // Pool balances and promo reservations share the append lock;
            // concurrent other-key holds cannot invalidate this split.
            let split = self.pool_split(&mut tx, wallet_minor).await?;
            if split.promo > 0 {
                self.pin_promo_draws_in(&mut tx, &watch.hold_key, split.promo)
                    .await?;
            }
            let mut actor = Provenance::none()
                .with_actor(&key.key_id.simple().to_string())
                .map_err(super::invalid)?;
            if let Some(mandate) = mandate {
                actor = actor
                    .with_correlation(&mandate.verified.mandate_id.simple().to_string())
                    .map_err(super::invalid)?;
            }
            let draft = self.hold_entry(
                &watch.hold_key,
                &super::description_of(description)?,
                split,
                Some(actor),
                on,
            )?;
            self.append_in(&mut tx, draft).await?;
        }
        db.note_open_hold_in(&mut tx, watch).await?;
        sqlx::query("UPDATE oxsum.open_holds SET wallet_minor=$2 WHERE hold_key=$1")
            .bind(&watch.hold_key)
            .bind(wallet_minor)
            .execute(&mut *tx)
            .await?;
        if !db
            .bind_hold_in(&mut tx, parent.operation_id, &watch.hold_key, lease_owner)
            .await?
        {
            return Err(WalletError::Conflict(
                "the request operation could not bind its reservation".into(),
            ));
        }
        let parent = db
            .operation_in(&mut tx, operation.organization_id, parent.operation_id)
            .await?
            .ok_or_else(|| WalletError::Conflict("the reserved operation disappeared".into()))?;
        tx.commit().await?;
        Ok(GatewayAdmission {
            operation: parent,
            subscription,
            wallet_minor,
        })
    }

    /// The common authority boundary for monetary and zero-wallet reservations.
    /// Monetary caps retain their existing posting basis; quota is not counted twice.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn authorize_key_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        key: &ActingKey,
        model: Option<&str>,
        minor: i64,
        on: Date,
        mandate: Option<&PresentedMandate>,
    ) -> Result<(), WalletError> {
        self.lock_key_admission_in(tx, key.key_id).await?;
        let row = sqlx::query(
            "SELECT k.spend_limit_minor,k.budget_duration,k.model_allowlist,k.max_concurrent_holds,k.organization_id, \
             k.revoked_at IS NOT NULL OR (k.expires_at IS NOT NULL AND k.expires_at<=clock_timestamp()) AS inactive, \
             (o.suspended_at IS NOT NULL OR o.dunning_suspended_at IS NOT NULL OR EXISTS(SELECT 1 FROM oxsum.customer_profiles c WHERE c.organization_id=o.organization_id AND c.state='archived')) AS suspended, \
             m.user_id AS member FROM oxsum.api_keys k JOIN oxsum.organizations o USING(organization_id) \
             LEFT JOIN oxsum.memberships m ON m.organization_id=k.organization_id AND m.user_id=k.created_by \
             WHERE k.key_id=$1 AND o.tenant_id=$2 FOR UPDATE OF k",
        ).bind(key.key_id).bind(&self.tenant_id).fetch_optional(&mut **tx).await?
            .ok_or(WalletError::Unauthenticated)?;
        if row.try_get::<bool, _>("inactive")? {
            return Err(WalletError::Unauthenticated);
        }
        if row.try_get::<bool, _>("suspended")? {
            return Err(WalletError::Forbidden(
                "the organization is suspended".into(),
            ));
        }
        let organization: Uuid = row.try_get("organization_id")?;
        let sold_plans: bool =
            sqlx::query_scalar("SELECT to_regclass('oxsum.customer_plans') IS NOT NULL")
                .fetch_one(&mut **tx)
                .await?;
        if minor > 0
            && sold_plans
            && crate::plan_debt::admission_suspended_in(tx, organization).await?
        {
            return Err(WalletError::Forbidden(
                "the organization is suspended".into(),
            ));
        }
        let allowlist: Option<Vec<String>> = row.try_get("model_allowlist")?;
        if let (Some(model), Some(allowlist)) = (model, allowlist)
            && !allowlist.iter().any(|allowed| allowed == model)
        {
            return Err(WalletError::Forbidden(format!(
                "this key may not call model {model:?}"
            )));
        }
        if let Some(limit) = row.try_get::<Option<i64>, _>("spend_limit_minor")? {
            let since = row
                .try_get::<Option<String>, _>("budget_duration")?
                .as_deref()
                .map(BudgetDuration::parse)
                .transpose()?
                .map(|period| period.period_start(on));
            let committed = self.key_committed_in(tx, &key.key_id, since).await?;
            check_budget(committed, minor, limit)?;
        }
        let mut team = None;
        if let Some(member) = row.try_get::<Option<Uuid>, _>("member")?
            && let Some(current) = sqlx::query("SELECT budget_limit_minor,team_id FROM oxsum.memberships WHERE organization_id=$1 AND user_id=$2 FOR UPDATE")
                .bind(organization).bind(member).fetch_optional(&mut **tx).await?
        {
            team = current.try_get::<Option<Uuid>, _>("team_id")?;
            if let Some(cap) = current.try_get::<Option<i64>, _>("budget_limit_minor")? {
                check_budget(
                    self.member_committed_in(tx, &organization, &member).await?,
                    minor,
                    cap,
                )?;
            }
        }
        if let Some(team) = team {
            let cap: Option<i64> = sqlx::query_scalar("SELECT budget_limit_minor FROM oxsum.teams WHERE organization_id=$1 AND team_id=$2 FOR UPDATE")
                .bind(organization).bind(team).fetch_one(&mut **tx).await?;
            if let Some(cap) = cap {
                check_budget(
                    self.team_committed_in(tx, &organization, &team).await?,
                    minor,
                    cap,
                )?;
            }
        }
        if let Some(cap) = row.try_get::<Option<i32>, _>("max_concurrent_holds")? {
            let open = self.key_open_holds_in(tx, &key.key_id).await?
                + self.zero_wallet_reservations_in(tx, key.key_id).await?;
            if open >= i64::from(cap) {
                return Err(WalletError::TooManyHolds {
                    limit: i64::from(cap),
                    open,
                });
            }
        }
        if let Some(mandate) = mandate {
            if mandate.verified.key_id != key.key_id
                || mandate.verified.organization_id != organization
            {
                return Err(WalletError::InvalidMandate);
            }
            let now = OffsetDateTime::now_utc().unix_timestamp();
            if now < mandate.not_before_ts() {
                return Err(WalletError::MandateOutOfWindow { not_yet: true });
            }
            if now > mandate.not_after_ts() {
                return Err(WalletError::MandateOutOfWindow { not_yet: false });
            }
            if let (Some(model), Some(models)) = (model, &mandate.verified.models)
                && !models.iter().any(|allowed| allowed == model)
            {
                return Err(WalletError::Forbidden(format!(
                    "the mandate does not delegate model {model:?}"
                )));
            }
            let committed = self
                .mandate_committed_in(tx, &mandate.verified.mandate_id)
                .await?;
            if i128::from(committed) + i128::from(minor) > i128::from(mandate.verified.cap_minor) {
                return Err(WalletError::MandateExceeded {
                    cap_minor: mandate.verified.cap_minor,
                    committed_minor: committed,
                });
            }
        }
        Ok(())
    }

    pub(super) async fn zero_wallet_reservations_in(
        &self,
        connection: &mut sqlx::PgConnection,
        key: Uuid,
    ) -> Result<i64, WalletError> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM oxsum.open_holds WHERE tenant_id=$1 AND key_id=$2 AND wallet_minor=0")
            .bind(&self.tenant_id).bind(key).fetch_one(connection).await?)
    }
}

fn check_budget(committed: i64, requested: i64, limit: i64) -> Result<(), WalletError> {
    if i128::from(committed) + i128::from(requested) > i128::from(limit) {
        return Err(WalletError::KeyLimitExceeded {
            limit_minor: limit,
            committed_minor: committed,
        });
    }
    Ok(())
}
