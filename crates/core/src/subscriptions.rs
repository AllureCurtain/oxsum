//! Subscription quota plans (issue #496): a fixed-period entitlement that lives
//! outside the ledger and refreshes on a cadence, beside the prepaid wallet.
//!
//! Two pools pay for a turn. The prepaid wallet — top-ups, grants, the credit
//! line — is what `hold_for_key` splits a freeze across today. A subscription is
//! the other pool: a quota the plan renews on a schedule (`daily`, `weekly`,
//! `monthly`, `never`), drawn before the wallet when one is bound and has room.
//! Quota is not ledger money — it is a counter on the `org_subscriptions` row,
//! bounded by the plan's `quota_minor` — so the draw records itself on
//! `oxsum.subscription_draws` rather than as a ledger posting. The wallet only
//! ever sees the part the subscription could not cover.
//!
//! The period boundary is the unit of audit. Every rollover, cancel, expiry and
//! plan change writes one `oxsum.subscription_periods` row, so a month's burn
//! is provable against the quota that period carried — never a counter on the
//! live row that could have drifted. The `subscription-rollovers` job closes
//! the periods a quiet organization would not reach on its own; the lazy path
//! in [`Db::admit_subscription_draw`] rolls an ended window on the next hold
//! anyway, so the job is promptness, not correctness.

use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// One plan in the operator's catalogue.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionPlan {
    pub plan_id: Uuid,
    /// What the plan is called — the admin list and the bind target.
    pub name: String,
    /// Quota per period in credit minor units. `None` is unmetered: the
    /// subscription admits every draw, and the wallet never sees its traffic.
    pub quota_minor: Option<i64>,
    /// The cadence `consumed_minor` resets on. `never` keeps one running
    /// counter for the subscription's whole life: no boundary rolls and
    /// nothing expires on time — only a cancel or the rail's `deleted` event
    /// ends it.
    pub reset_period: ResetPeriod,
    /// Whether an exhausted plan may fall through to the wallet — the flag the
    /// subscription snapshots at bind time.
    pub allow_wallet_overflow: bool,
    /// What the plan sells for, minor units. Catalogue information for the
    /// admin surface; charging rides the deposit rails.
    pub price_minor: Option<i64>,
    /// The Stripe `price_…` a checkout names, when the plan sells on that rail.
    pub stripe_price_id: Option<String>,
    /// Whether new subscriptions may bind. A disabled plan keeps its history.
    pub enabled: bool,
    pub sort_order: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// How often a plan's quota resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResetPeriod {
    Daily,
    Weekly,
    Monthly,
    Never,
}

impl ResetPeriod {
    /// The spelling `subscription_plans.reset_period` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Never => "never",
        }
    }

    /// Reads a stored period back; an unknown one is a row this build cannot
    /// interpret, and the caller refuses it rather than guess.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// One organization's link to a plan, as the org and admin surfaces read it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgSubscription {
    pub subscription_id: Uuid,
    pub organization_id: Uuid,
    pub plan_id: Uuid,
    /// `active`, `cancelled` or `expired` — the state the index keys on.
    pub state: String,
    /// The window `consumed_minor` counts inside.
    #[serde(with = "time::serde::rfc3339")]
    pub current_period_start: OffsetDateTime,
    /// When the window closes; `None` on a never-resetting plan.
    #[serde(with = "time::serde::rfc3339::option")]
    pub current_period_end: Option<OffsetDateTime>,
    /// What this period has consumed, minor units.
    pub consumed_minor: i64,
    /// The plan's quota, snapshotted at bind — the bound `consumed_minor`
    /// runs against it, so a mid-life plan edit does not move a subscription
    /// that was sold under different terms.
    pub quota_minor: Option<i64>,
    /// The plan's overflow flag, snapshotted at bind.
    pub allow_wallet_overflow: bool,
    /// What opened the link: `admin` or a rail name.
    pub source: String,
    /// The Stripe subscription id the webhook renews against, when bound there.
    pub stripe_subscription_id: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// One closed period's record: the quota it carried and what it consumed. The
/// open period is the `org_subscriptions` row itself; this table is the
/// boundary's audit — every close lands a row here.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionPeriod {
    pub period_id: Uuid,
    pub subscription_id: Uuid,
    pub organization_id: Uuid,
    pub plan_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub period_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub period_end: Option<OffsetDateTime>,
    /// The quota the plan carried into the period, snapshotted.
    pub quota_minor: Option<i64>,
    pub consumed_minor: i64,
    /// How the period closed: `boundary`, `cancelled`, `expired`, `upgrade` or
    /// `downgrade`. `open` while it still runs.
    pub outcome: String,
    pub created_at: OffsetDateTime,
}

/// What a hold-time draw answered: the draw row and the amount still owed the
/// wallet, which is what `hold_for_key` splits across its pools.
#[derive(Debug)]
pub struct SubscriptionAdmission {
    /// How much of the freeze the subscription covered; the rest overflows.
    pub drawn_minor: i64,
    /// The draw's own id, so settle can release or charge against it.
    pub draw_id: Uuid,
    /// The period the draw counted inside — the settlement description names
    /// it, so a bill proves which entitlement window paid.
    pub period_id: Uuid,
    /// The subscription it drew on.
    pub subscription_id: Uuid,
}

/// The rollover pass's report, for the worker's log line.
#[derive(Debug, Default)]
pub struct RolloverReport {
    /// Subscriptions whose period ended and were rolled forward.
    pub rolled: usize,
    /// Subscriptions the pass found already lapsed.
    pub expired: usize,
}

impl Db {
    /// The catalogue of plans, ordered for the admin list.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn subscription_plans(&self) -> Result<Vec<SubscriptionPlan>, WalletError> {
        let rows = sqlx::query(
            "SELECT plan_id, name, quota_minor, reset_period, allow_wallet_overflow, \
                    price_minor, stripe_price_id, enabled, sort_order, created_at, updated_at \
             FROM oxsum.subscription_plans \
             ORDER BY sort_order, created_at",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(plan_from_row).collect()
    }

    /// One plan by id, for the bind path.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn subscription_plan(
        &self,
        plan_id: Uuid,
    ) -> Result<Option<SubscriptionPlan>, WalletError> {
        let row = sqlx::query(
            "SELECT plan_id, name, quota_minor, reset_period, allow_wallet_overflow, \
                    price_minor, stripe_price_id, enabled, sort_order, created_at, updated_at \
             FROM oxsum.subscription_plans WHERE plan_id = $1",
        )
        .bind(plan_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(plan_from_row).transpose()
    }

    /// Creates or replaces a plan. A `PUT` by name is naturally idempotent:
    /// replaying the same body writes the same row, so the call carries no
    /// idempotency key.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a negative quota or price; storage
    /// failures surface as [`WalletError`].
    #[allow(clippy::too_many_arguments)]
    pub async fn set_subscription_plan(
        &self,
        name: &str,
        quota_minor: Option<i64>,
        reset_period: ResetPeriod,
        allow_wallet_overflow: bool,
        price_minor: Option<i64>,
        stripe_price_id: Option<&str>,
        enabled: bool,
        sort_order: i32,
    ) -> Result<SubscriptionPlan, WalletError> {
        if let Some(quota) = quota_minor
            && quota <= 0
        {
            return Err(WalletError::InvalidInput(
                "quotaMinor must be positive or absent".into(),
            ));
        }
        if let Some(price) = price_minor
            && price < 0
        {
            return Err(WalletError::InvalidInput(
                "priceMinor must be zero or more".into(),
            ));
        }
        let row = sqlx::query(
            "INSERT INTO oxsum.subscription_plans \
                 (plan_id, name, quota_minor, reset_period, allow_wallet_overflow, \
                  price_minor, stripe_price_id, enabled, sort_order) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (name) DO UPDATE SET \
                 quota_minor = EXCLUDED.quota_minor, \
                 reset_period = EXCLUDED.reset_period, \
                 allow_wallet_overflow = EXCLUDED.allow_wallet_overflow, \
                 price_minor = EXCLUDED.price_minor, \
                 stripe_price_id = EXCLUDED.stripe_price_id, \
                 enabled = EXCLUDED.enabled, \
                 sort_order = EXCLUDED.sort_order, \
                 updated_at = now() \
             RETURNING plan_id, name, quota_minor, reset_period, allow_wallet_overflow, \
                       price_minor, stripe_price_id, enabled, sort_order, created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(quota_minor)
        .bind(reset_period.as_str())
        .bind(allow_wallet_overflow)
        .bind(price_minor)
        .bind(stripe_price_id)
        .bind(enabled)
        .bind(sort_order)
        .fetch_one(self.pool())
        .await?;
        plan_from_row(&row)
    }

    /// One organization's active subscription, when it has one.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn active_subscription(
        &self,
        organization_id: Uuid,
    ) -> Result<Option<OrgSubscription>, WalletError> {
        let row = sqlx::query(
            "SELECT subscription_id, organization_id, plan_id, state, \
                    current_period_start, current_period_end, consumed_minor, quota_minor, \
                    allow_wallet_overflow, source, stripe_subscription_id, \
                    created_at, updated_at \
             FROM oxsum.org_subscriptions \
             WHERE organization_id = $1 AND state = 'active'",
        )
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(subscription_from_row).transpose()
    }

    /// Binds an organization to a plan, or moves it between plans.
    ///
    /// The same `(organization_id, plan_id)` replay is a no-op — the answer is
    /// the standing subscription. A different plan than the active one is a
    /// plan change: the old period closes as `upgrade` or `downgrade` (higher
    /// `sort_order` is the upgrade direction), the new one opens under the new
    /// plan's terms, and a Stripe-bound subscription is replaced, not amended —
    /// its cancel rides the rail's own webhook.
    ///
    /// The whole transition runs in one transaction against the org's
    /// serialization row (`pg_advisory_xact_lock` on the organization id), so a
    /// racing bind or rollover cannot interleave a half-moved subscription.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for a plan that does not exist or is disabled;
    /// [`WalletError::InvalidInput`] for a cancelled or expired target;
    /// storage failures surface as [`WalletError`].
    pub async fn bind_subscription(
        &self,
        organization_id: Uuid,
        plan_id: Uuid,
        source: &str,
        stripe_subscription_id: Option<&str>,
    ) -> Result<OrgSubscription, WalletError> {
        let plan = self
            .subscription_plan(plan_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("no such plan".into()))?;
        if !plan.enabled {
            return Err(WalletError::NotFound(
                "the plan is not accepting subscriptions".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        // Serialize on the organization: two binds, or a bind and a rollover,
        // cannot interleave a half-moved subscription.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text, 0))",
        )
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await?;
        let existing = sqlx::query(
            "SELECT s.subscription_id, s.plan_id, s.current_period_start, s.current_period_end, \
                    s.consumed_minor, s.allow_wallet_overflow, p.sort_order \
             FROM oxsum.org_subscriptions s \
             JOIN oxsum.subscription_plans p ON p.plan_id = s.plan_id \
             WHERE s.organization_id = $1 AND s.state = 'active' FOR UPDATE OF s",
        )
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(current) = existing {
            let current_plan: Uuid = current.try_get("plan_id")?;
            if current_plan == plan_id {
                // A replayed bind: the answer is the standing subscription.
                tx.rollback().await?;
                return self
                    .active_subscription(organization_id)
                    .await?
                    .ok_or_else(|| {
                        WalletError::InvalidInput("the subscription did not land".into())
                    });
            }
            // The old period closes under its outcome; the direction a plan
            // change takes is `sort_order`'s — higher is the upgrade.
            let outcome = if plan.sort_order > current.try_get::<i32, _>("sort_order")? {
                "upgrade"
            } else {
                "downgrade"
            };
            close_period(&mut tx, &current, outcome).await?;
            sqlx::query(
                "UPDATE oxsum.org_subscriptions \
                 SET state = 'cancelled', updated_at = now() \
                 WHERE subscription_id = $1",
            )
            .bind(current.try_get::<Uuid, _>("subscription_id")?)
            .execute(&mut *tx)
            .await?;
        }
        let now = OffsetDateTime::now_utc();
        // `Never` binds with no end: there is no boundary to roll and no
        // lapse to expire — only a cancel or the rail's `deleted` event ends
        // it. `NULL` keeps both due scans away rather than inventing an
        // instant expiry.
        let period_end = period_window(plan.reset_period, now);
        let subscription_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.org_subscriptions \
                 (subscription_id, organization_id, plan_id, state, \
                  current_period_start, current_period_end, consumed_minor, quota_minor, \
                  allow_wallet_overflow, source, stripe_subscription_id) \
             VALUES ($1, $2, $3, 'active', $4, $5, 0, $6, $7, $8, $9)",
        )
        .bind(subscription_id)
        .bind(organization_id)
        .bind(plan_id)
        .bind(now)
        .bind(period_end)
        .bind(plan.quota_minor)
        .bind(plan.allow_wallet_overflow)
        .bind(source)
        .bind(stripe_subscription_id)
        .execute(&mut *tx)
        .await?;
        // The open period's audit row.
        sqlx::query(
            "INSERT INTO oxsum.subscription_periods \
                 (period_id, subscription_id, organization_id, plan_id, \
                  period_start, period_end, quota_minor, outcome) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'open')",
        )
        .bind(Uuid::new_v4())
        .bind(subscription_id)
        .bind(organization_id)
        .bind(plan_id)
        .bind(now)
        .bind(period_end)
        .bind(plan.quota_minor)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.active_subscription(organization_id)
            .await?
            .ok_or_else(|| WalletError::InvalidInput("the subscription did not land".into()))
    }

    /// Ends an organization's active subscription, if it has one: the period
    /// closes as `cancelled`, the row flips state, and the draws it opened are
    /// still settleable — the wallet never sees a subscription share again.
    ///
    /// Returns whether a subscription was active. `false` is the answer for an
    /// organization with nothing to end, not an error.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn cancel_subscription(&self, organization_id: Uuid) -> Result<bool, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text, 0))",
        )
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await?;
        let existing = sqlx::query(
            "SELECT s.subscription_id, s.plan_id, s.current_period_start, s.current_period_end, \
                    s.consumed_minor \
             FROM oxsum.org_subscriptions s \
             WHERE s.organization_id = $1 AND s.state = 'active' FOR UPDATE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(current) = existing else {
            tx.rollback().await?;
            return Ok(false);
        };
        close_period(&mut tx, &current, "cancelled").await?;
        sqlx::query(
            "UPDATE oxsum.org_subscriptions \
             SET state = 'cancelled', updated_at = now() \
             WHERE subscription_id = $1",
        )
        .bind(current.try_get::<Uuid, _>("subscription_id")?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// The subscription's period history, newest first — the audit the bills
    /// page and the admin surface read. The open window leads; the closed ones
    /// follow by when they ended, which `close_period` stamps at write time.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn subscription_periods(
        &self,
        subscription_id: Uuid,
    ) -> Result<Vec<SubscriptionPeriod>, WalletError> {
        let rows = sqlx::query(
            "SELECT period_id, subscription_id, organization_id, plan_id, \
                    period_start, period_end, quota_minor, consumed_minor, outcome, created_at \
             FROM oxsum.subscription_periods \
             WHERE subscription_id = $1 \
             ORDER BY (outcome = 'open') DESC, period_end DESC, period_id DESC",
        )
        .bind(subscription_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(period_from_row).collect()
    }

    /// How much of `minor` the organization's active subscription covers —
    /// drawn atomically against the period's quota, rolled forward first when
    /// the window has already ended.
    ///
    /// The draw lands on `oxsum.subscription_draws` under `hold_key`, so a
    /// replayed hold answers its own draw rather than debiting twice. The part
    /// the subscription could not cover is what the caller owes the wallet —
    /// `None` when the plan disallows overflow and the quota is exhausted, in
    /// which case the hold must refuse rather than charge the wallet.
    ///
    /// The whole read-and-draw runs in one transaction against the org's
    /// advisory lock, so a racing rollover or a second hold cannot observe a
    /// consumed figure the first had not written.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive `minor`; storage
    /// failures surface as [`WalletError`].
    pub async fn admit_subscription_draw(
        &self,
        organization_id: Uuid,
        hold_key: &str,
        minor: i64,
    ) -> Result<SubscriptionAdmission, WalletError> {
        if minor <= 0 {
            return Err(WalletError::InvalidInput("minor must be positive".into()));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text, 0))",
        )
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await?;
        // A replayed hold answers its own draw — the unique index on hold_key
        // is the idempotency anchor, same as the ledger's.
        if let Some(existing) = sqlx::query(
            "SELECT draw_id, subscription_id, period_id, drawn_minor \
             FROM oxsum.subscription_draws WHERE hold_key = $1",
        )
        .bind(hold_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            let drawn: i64 = existing.try_get("drawn_minor")?;
            tx.rollback().await?;
            return Ok(SubscriptionAdmission {
                drawn_minor: drawn,
                draw_id: existing.try_get("draw_id")?,
                period_id: existing.try_get("period_id")?,
                subscription_id: existing.try_get("subscription_id")?,
            });
        }
        let subscription = sqlx::query(
            "SELECT s.subscription_id, s.plan_id, s.current_period_start, s.current_period_end, \
                    s.consumed_minor, s.allow_wallet_overflow, p.quota_minor \
             FROM oxsum.org_subscriptions s \
             JOIN oxsum.subscription_plans p ON p.plan_id = s.plan_id \
             WHERE s.organization_id = $1 AND s.state = 'active' FOR UPDATE OF s",
        )
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(subscription) = subscription else {
            // No active subscription: the whole freeze is the wallet's, and no
            // draw row is needed — the settlement description simply carries no
            // subscription field.
            tx.rollback().await?;
            return Ok(SubscriptionAdmission {
                drawn_minor: 0,
                draw_id: Uuid::nil(),
                period_id: Uuid::nil(),
                subscription_id: Uuid::nil(),
            });
        };
        // Roll the period forward first when the window has ended — the lazy
        // rollover. The boundary is decided inside the locked read, so a racing
        // pass sees either the old window or the new one, never a mix.
        let quota: Option<i64> = subscription.try_get("quota_minor")?;
        let plan_id: Uuid = subscription.try_get("plan_id")?;
        let mut consumed: i64 = subscription.try_get("consumed_minor")?;
        let period_end: Option<OffsetDateTime> = subscription.try_get("current_period_end")?;
        let subscription_id: Uuid = subscription.try_get("subscription_id")?;
        if let Some(mut end) = period_end
            && end <= OffsetDateTime::now_utc()
        {
            let reset = reset_period_of(&mut tx, plan_id).await?;
            // Roll past every boundary the quiet gap spanned, so the draw
            // lands inside the window that is actually open now — one pass
            // would leave `end` still in the past after a multi-period gap.
            while end <= OffsetDateTime::now_utc() {
                close_period(&mut tx, &subscription, "boundary").await?;
                sqlx::query(
                    "UPDATE oxsum.org_subscriptions \
                     SET current_period_start = $2, current_period_end = $3, \
                         consumed_minor = 0, updated_at = now() \
                     WHERE subscription_id = $1",
                )
                .bind(subscription_id)
                .bind(end)
                .bind(period_window(reset, end))
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO oxsum.subscription_periods \
                         (period_id, subscription_id, organization_id, plan_id, \
                          period_start, period_end, quota_minor, outcome) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, 'open')",
                )
                .bind(Uuid::new_v4())
                .bind(subscription_id)
                .bind(organization_id)
                .bind(plan_id)
                .bind(end)
                .bind(period_window(reset, end))
                .bind(quota)
                .execute(&mut *tx)
                .await?;
                consumed = 0;
                // The window rolls forward one boundary at a time: a `daily`
                // plan three days quiet closes three windows and opens the
                // fourth, each starting where the previous ended — no period
                // is invented from `now`, which would skip the audit.
                let Some(next) = period_window(reset, end) else {
                    break;
                };
                end = next;
            }
        }
        let remaining = quota.map(|q| (q - consumed).max(0));
        let drawn = remaining.map_or(minor, |r| r.min(minor));
        let period_id = open_period_id(&mut tx, subscription_id).await?;
        let draw_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.subscription_draws \
                 (draw_id, subscription_id, period_id, hold_key, drawn_minor) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(draw_id)
        .bind(subscription_id)
        .bind(period_id)
        .bind(hold_key)
        .bind(drawn)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.org_subscriptions \
             SET consumed_minor = consumed_minor + $2, updated_at = now() \
             WHERE subscription_id = $1",
        )
        .bind(subscription_id)
        .bind(drawn)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.subscription_periods \
             SET consumed_minor = consumed_minor + $2 \
             WHERE period_id = $1",
        )
        .bind(period_id)
        .bind(drawn)
        .execute(&mut *tx)
        .await?;
        let allow_overflow: bool = subscription.try_get("allow_wallet_overflow")?;
        if !allow_overflow && drawn < minor {
            // The plan refuses overflow and could not cover the freeze: the
            // hold must not reach the wallet at all.
            tx.rollback().await?;
            return Err(WalletError::InsufficientFunds);
        }
        tx.commit().await?;
        Ok(SubscriptionAdmission {
            drawn_minor: drawn,
            draw_id,
            period_id,
            subscription_id,
        })
    }

    /// Charges what the settlement consumed against a hold's draw, and
    /// releases the rest back to the period's counter.
    ///
    /// `actual_minor` is the wallet-settled figure, minor units: the draw's
    /// consumed part is `min(actual_minor, drawn_minor)`, the released part is
    /// the remainder. Calling under the org's advisory lock so the counter and
    /// the draw row agree — a racing draw or rollover cannot observe a
    /// consumed figure the release had not written.
    ///
    /// A replayed settlement sees `settled_minor` already written and answers
    /// it back — the idempotency anchor is the draw's `hold_key`, matching the
    /// ledger settlement's own derived key.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when `actual_minor` exceeds the draw or no
    /// draw exists under `hold_key`; storage failures surface as
    /// [`WalletError`].
    pub async fn settle_subscription_draw(
        &self,
        hold_key: &str,
        actual_minor: i64,
    ) -> Result<Option<i64>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let draw = sqlx::query(
            "SELECT d.draw_id, d.subscription_id, d.period_id, d.drawn_minor, d.settled_minor, \
                    s.organization_id \
             FROM oxsum.subscription_draws d \
             JOIN oxsum.org_subscriptions s ON s.subscription_id = d.subscription_id \
             WHERE d.hold_key = $1 FOR UPDATE OF d",
        )
        .bind(hold_key)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(draw) = draw else {
            tx.rollback().await?;
            return Ok(None);
        };
        let drawn: i64 = draw.try_get("drawn_minor")?;
        let organization_id: Uuid = draw.try_get("organization_id")?;
        let subscription_id: Uuid = draw.try_get("subscription_id")?;
        let period_id: Uuid = draw.try_get("period_id")?;
        if let Some(settled) = draw.try_get::<Option<i64>, _>("settled_minor")? {
            tx.rollback().await?;
            return Ok(Some(settled));
        }
        if actual_minor < 0 || actual_minor > drawn {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=drawn".into(),
            ));
        }
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text, 0))",
        )
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.subscription_draws \
             SET settled_minor = $2, settled_at = now() \
             WHERE draw_id = $1",
        )
        .bind(draw.try_get::<Uuid, _>("draw_id")?)
        .bind(actual_minor)
        .execute(&mut *tx)
        .await?;
        // Give back what the draw reserved but the charge did not use — the
        // period's counter and the subscription's live one move together.
        let released = drawn - actual_minor;
        if released > 0 {
            sqlx::query(
                "UPDATE oxsum.org_subscriptions \
                 SET consumed_minor = consumed_minor - $2, updated_at = now() \
                 WHERE subscription_id = $1",
            )
            .bind(subscription_id)
            .bind(released)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE oxsum.subscription_periods \
                 SET consumed_minor = consumed_minor - $2 \
                 WHERE period_id = $1",
            )
            .bind(period_id)
            .bind(released)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Some(actual_minor))
    }

    /// The rollover pass: every active subscription whose period has ended is
    /// rolled forward — the old period closes `boundary`, a fresh window opens.
    /// A `never`-reset row carries no `current_period_end`, so the scan never
    /// sees it: with no boundary to roll there is nothing for the pass to do.
    ///
    /// Returns what the pass did, for the worker's log.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn rollover_due_subscriptions(&self) -> Result<RolloverReport, WalletError> {
        let due = sqlx::query(
            "SELECT s.subscription_id, s.organization_id, s.plan_id, \
                    s.current_period_start, s.current_period_end, s.consumed_minor, \
                    p.reset_period, p.quota_minor \
             FROM oxsum.org_subscriptions s \
             JOIN oxsum.subscription_plans p ON p.plan_id = s.plan_id \
             WHERE s.state = 'active' \
               AND s.current_period_end IS NOT NULL \
               AND s.current_period_end <= now() \
             ORDER BY s.current_period_end",
        )
        .fetch_all(self.pool())
        .await?;
        let mut report = RolloverReport::default();
        for row in due {
            let subscription_id: Uuid = row.try_get("subscription_id")?;
            let organization_id: Uuid = row.try_get("organization_id")?;
            let plan_id: Uuid = row.try_get("plan_id")?;
            let quota_minor: Option<i64> = row.try_get("quota_minor")?;
            let reset = ResetPeriod::parse(row.try_get::<&str, _>("reset_period")?)
                .unwrap_or(ResetPeriod::Never);
            let mut tx = self.pool().begin().await?;
            sqlx::query(
                "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text, 0))",
            )
            .bind(organization_id.to_string())
            .execute(&mut *tx)
            .await?;
            // Re-read under the lock: a lazy rollover may have closed the
            // window already, and this pass must not roll a period twice.
            let current = sqlx::query(
                "SELECT subscription_id, organization_id, plan_id, \
                        current_period_start, current_period_end, consumed_minor \
                 FROM oxsum.org_subscriptions \
                 WHERE subscription_id = $1 AND state = 'active' FOR UPDATE",
            )
            .bind(subscription_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(current) = current else {
                tx.rollback().await?;
                continue;
            };
            let period_end: Option<OffsetDateTime> = current.try_get("current_period_end")?;
            let Some(mut end) = period_end else {
                tx.rollback().await?;
                continue;
            };
            if end > OffsetDateTime::now_utc() {
                tx.rollback().await?;
                continue;
            }
            if reset == ResetPeriod::Never {
                // Unreachable while the scan filters `current_period_end IS
                // NOT NULL` — a never-reset row has no end to be due on — but
                // the row exists by construction, so the pass answers
                // defensively rather than rolling a window it does not have.
                tx.rollback().await?;
                continue;
            } else {
                // Roll forward one boundary at a time until the open window
                // holds `now`: a subscription quiet for several periods closes
                // each of them, rather than skipping straight to a window
                // invented from `now` and losing the audit.
                while end <= OffsetDateTime::now_utc() {
                    close_period(&mut tx, &current, "boundary").await?;
                    sqlx::query(
                        "UPDATE oxsum.org_subscriptions \
                         SET current_period_start = $2, current_period_end = $3, \
                             consumed_minor = 0, updated_at = now() \
                         WHERE subscription_id = $1",
                    )
                    .bind(subscription_id)
                    .bind(end)
                    .bind(period_window(reset, end))
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO oxsum.subscription_periods \
                             (period_id, subscription_id, organization_id, plan_id, \
                              period_start, period_end, quota_minor, outcome) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, 'open')",
                    )
                    .bind(Uuid::new_v4())
                    .bind(subscription_id)
                    .bind(organization_id)
                    .bind(plan_id)
                    .bind(end)
                    .bind(period_window(reset, end))
                    .bind(quota_minor)
                    .execute(&mut *tx)
                    .await?;
                    report.rolled += 1;
                    let Some(next) = period_window(reset, end) else {
                        break;
                    };
                    end = next;
                }
            }
            tx.commit().await?;
        }
        Ok(report)
    }

    /// Renews a Stripe-bound subscription on `invoice.paid`: the period rolls
    /// forward to the boundary the invoice opened, and a lapsed subscription
    /// reactivates — the rail's `customer.subscription.deleted` is the only
    /// event that ends it.
    ///
    /// The `payment_ref` is the Stripe subscription id, so the unique index on
    /// `stripe_subscription_id` is the idempotency anchor: a replayed webhook
    /// answers the standing row.
    ///
    /// Returns the renewed subscription, or `None` when the id names nothing.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when the subscription is not Stripe-bound;
    /// storage failures surface as [`WalletError`].
    pub async fn renew_stripe_subscription(
        &self,
        stripe_subscription_id: &str,
        new_period_end: OffsetDateTime,
    ) -> Result<Option<OrgSubscription>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let subscription = sqlx::query(
            "SELECT s.subscription_id, s.organization_id, s.plan_id, s.state, \
                    s.current_period_start, s.current_period_end, s.consumed_minor, \
                    p.reset_period, p.quota_minor \
             FROM oxsum.org_subscriptions s \
             JOIN oxsum.subscription_plans p ON p.plan_id = s.plan_id \
             WHERE s.stripe_subscription_id = $1 FOR UPDATE OF s",
        )
        .bind(stripe_subscription_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(subscription) = subscription else {
            tx.rollback().await?;
            return Ok(None);
        };
        let organization_id: Uuid = subscription.try_get("organization_id")?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text, 0))",
        )
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await?;
        let subscription_id: Uuid = subscription.try_get("subscription_id")?;
        let plan_id: Uuid = subscription.try_get("plan_id")?;
        let state: String = subscription.try_get("state")?;
        if state != "active" {
            // A cancelled or expired subscription does not renew — only
            // `customer.subscription.deleted` ends it, and the rail's own
            // record is the truth there.
            tx.rollback().await?;
            return self.subscription_by_stripe(stripe_subscription_id).await;
        }
        close_period(&mut tx, &subscription, "boundary").await?;
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            "UPDATE oxsum.org_subscriptions \
             SET current_period_start = $2, current_period_end = $3, \
                 consumed_minor = 0, updated_at = now() \
             WHERE subscription_id = $1",
        )
        .bind(subscription_id)
        .bind(now)
        .bind(new_period_end)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO oxsum.subscription_periods \
                 (period_id, subscription_id, organization_id, plan_id, \
                  period_start, period_end, quota_minor, outcome) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'open')",
        )
        .bind(Uuid::new_v4())
        .bind(subscription_id)
        .bind(organization_id)
        .bind(plan_id)
        .bind(now)
        .bind(new_period_end)
        .bind(subscription.try_get::<Option<i64>, _>("quota_minor")?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.subscription_by_stripe(stripe_subscription_id).await
    }

    /// Ends a Stripe-bound subscription on `customer.subscription.deleted`.
    /// Returns whether one was active; `None` for an id nothing bound.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn cancel_stripe_subscription(
        &self,
        stripe_subscription_id: &str,
    ) -> Result<Option<bool>, WalletError> {
        let Some(subscription) = self.subscription_by_stripe(stripe_subscription_id).await? else {
            return Ok(None);
        };
        Ok(Some(
            self.cancel_subscription(subscription.organization_id)
                .await?,
        ))
    }

    /// The subscription a Stripe id names, active or not — the webhook's read.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn subscription_by_stripe(
        &self,
        stripe_subscription_id: &str,
    ) -> Result<Option<OrgSubscription>, WalletError> {
        let row = sqlx::query(
            "SELECT subscription_id, organization_id, plan_id, state, \
                    current_period_start, current_period_end, consumed_minor, quota_minor, \
                    allow_wallet_overflow, source, stripe_subscription_id, \
                    created_at, updated_at \
             FROM oxsum.org_subscriptions \
             WHERE stripe_subscription_id = $1",
        )
        .bind(stripe_subscription_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(subscription_from_row).transpose()
    }
}

/// Reads the open period's id for a subscription — the period whose draw a new
/// hold must count inside.
async fn open_period_id(
    tx: &mut sqlx::PgConnection,
    subscription_id: Uuid,
) -> Result<Uuid, WalletError> {
    let row = sqlx::query_scalar::<_, Uuid>(
        "SELECT period_id FROM oxsum.subscription_periods \
         WHERE subscription_id = $1 AND outcome = 'open' \
         ORDER BY period_start DESC LIMIT 1",
    )
    .bind(subscription_id)
    .fetch_optional(&mut *tx)
    .await?;
    row.ok_or_else(|| WalletError::InvalidInput("the subscription has no open period".into()))
}

/// The plan's `reset_period` as its row stores it — read inside the caller's
/// transaction so the rollover's window is computed on the same snapshot.
async fn reset_period_of(
    tx: &mut sqlx::PgConnection,
    plan_id: Uuid,
) -> Result<ResetPeriod, WalletError> {
    let text = sqlx::query_scalar::<_, String>(
        "SELECT reset_period FROM oxsum.subscription_plans WHERE plan_id = $1",
    )
    .bind(plan_id)
    .fetch_one(&mut *tx)
    .await?;
    ResetPeriod::parse(&text)
        .ok_or_else(|| WalletError::InvalidInput(format!("unknown reset period {text:?}")))
}

/// What a period boundary looks like at `now` for `reset`: the window opens on
/// `now` itself (an immediate bind) and closes on the cadence's next UTC
/// boundary — midnight for daily, Monday 00:00 for weekly, the first of the
/// month for monthly. `never` carries no end.
fn period_window(reset: ResetPeriod, now: OffsetDateTime) -> Option<OffsetDateTime> {
    match reset {
        ResetPeriod::Daily => {
            let tomorrow = now.date() + time::Duration::days(1);
            Some(tomorrow.midnight().assume_utc())
        }
        ResetPeriod::Weekly => {
            // Next Monday 00:00 UTC. `number_from_monday` is 1..=7; a call on
            // Monday itself lands next Monday, not today.
            let weekday = i64::from(now.weekday().number_from_monday());
            let days = 8 - weekday;
            let next = now.date() + time::Duration::days(days);
            Some(next.midnight().assume_utc())
        }
        ResetPeriod::Monthly => {
            let date = now.date();
            let year = date.year();
            let month = date.month();
            let (next_year, next_month) = if month == time::Month::December {
                (year + 1, time::Month::January)
            } else {
                (year, month.next())
            };
            // `date!` takes literals, so the first-of-next-month is built
            // through `Date::from_calendar_date` — always day 1, always valid.
            let first = time::Date::from_calendar_date(next_year, next_month, 1)
                .map_err(|e| WalletError::InvalidInput(format!("bad period boundary: {e}")))
                .ok();
            first.map(|d| d.midnight().assume_utc())
        }
        ResetPeriod::Never => None,
    }
}

/// Closes a subscription's open period: stamps the period row's end and
/// outcome so the closed window stands as the audit record.
async fn close_period(
    tx: &mut sqlx::PgConnection,
    subscription: &sqlx::postgres::PgRow,
    outcome: &str,
) -> Result<(), WalletError> {
    let subscription_id: Uuid = subscription.try_get("subscription_id")?;
    let consumed: i64 = subscription.try_get("consumed_minor")?;
    sqlx::query(
        "UPDATE oxsum.subscription_periods \
         SET period_end = now(), consumed_minor = $2, outcome = $3 \
         WHERE subscription_id = $1 AND outcome = 'open'",
    )
    .bind(subscription_id)
    .bind(consumed)
    .bind(outcome)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

fn plan_from_row(row: &sqlx::postgres::PgRow) -> Result<SubscriptionPlan, WalletError> {
    Ok(SubscriptionPlan {
        plan_id: row.try_get("plan_id")?,
        name: row.try_get("name")?,
        quota_minor: row.try_get("quota_minor")?,
        reset_period: ResetPeriod::parse(row.try_get::<&str, _>("reset_period")?)
            .ok_or_else(|| WalletError::InvalidInput("unknown reset period".into()))?,
        allow_wallet_overflow: row.try_get("allow_wallet_overflow")?,
        price_minor: row.try_get("price_minor")?,
        stripe_price_id: row.try_get("stripe_price_id")?,
        enabled: row.try_get("enabled")?,
        sort_order: row.try_get("sort_order")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn subscription_from_row(row: &sqlx::postgres::PgRow) -> Result<OrgSubscription, WalletError> {
    Ok(OrgSubscription {
        subscription_id: row.try_get("subscription_id")?,
        organization_id: row.try_get("organization_id")?,
        plan_id: row.try_get("plan_id")?,
        state: row.try_get("state")?,
        current_period_start: row.try_get("current_period_start")?,
        current_period_end: row.try_get("current_period_end")?,
        consumed_minor: row.try_get("consumed_minor")?,
        quota_minor: row.try_get("quota_minor")?,
        allow_wallet_overflow: row.try_get("allow_wallet_overflow")?,
        source: row.try_get("source")?,
        stripe_subscription_id: row.try_get("stripe_subscription_id")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn period_from_row(row: &sqlx::postgres::PgRow) -> Result<SubscriptionPeriod, WalletError> {
    Ok(SubscriptionPeriod {
        period_id: row.try_get("period_id")?,
        subscription_id: row.try_get("subscription_id")?,
        organization_id: row.try_get("organization_id")?,
        plan_id: row.try_get("plan_id")?,
        period_start: row.try_get("period_start")?,
        period_end: row.try_get("period_end")?,
        quota_minor: row.try_get("quota_minor")?,
        consumed_minor: row.try_get("consumed_minor")?,
        outcome: row.try_get("outcome")?,
        created_at: row.try_get("created_at")?,
    })
}
