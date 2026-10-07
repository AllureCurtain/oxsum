//! Pricing discounts (roadmap P7-1, issue #158).
//!
//! A `pricing_discounts` row grants a percent off the priced sum to an
//! organization and/or a model — `NULL` scopes to all — inside a validity
//! window. Per docs/decisions.md, after a conditional rule picks the price set
//! and the gross line items are computed, the single most favorable applicable
//! discount applies; discounts never stack. The freeze stays the undiscounted
//! bound: a discount only ever lowers what the turn is charged.
//!
//! The applied percent is snapshotted into the settlement description
//! (`discountPercent`, schema v4), so a bill recomputes from its own bytes
//! after the row has changed or ended — the row is mutable-store data, the
//! snapshot is the credential. See D10's `price × multiplier` rule.

use serde::Serialize;
use sqlx::Row as _;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// A discount row, as the admin surface reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discount {
    pub discount_id: Uuid,
    /// The organization the discount applies to; `None` is every organization.
    pub organization_id: Option<Uuid>,
    /// The scoped organization's name, for the operator's list.
    pub organization: Option<String>,
    /// The model the discount applies to; `None` is every model.
    pub model: Option<String>,
    /// The percent taken off the priced sum — 100 settles at zero.
    pub percent: i32,
    /// Why the discount exists, in the operator's own words.
    pub label: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub valid_from: OffsetDateTime,
    /// The end of the window; `None` does not expire.
    #[serde(with = "time::serde::rfc3339::option")]
    pub valid_until: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The fields a discount write is idempotent over: a retried `POST` carries the
/// same key, and the row it created is the answer when every field still
/// matches — the same key under different fields is a conflict.
#[derive(Debug)]
pub struct NewDiscount {
    pub percent: i32,
    pub organization_id: Option<Uuid>,
    pub model: Option<String>,
    pub label: Option<String>,
    pub valid_from: Option<OffsetDateTime>,
    pub valid_until: Option<OffsetDateTime>,
}

const DISCOUNT_COLS: &str = "d.discount_id, d.organization_id, o.name AS organization, \
    d.model, d.percent, d.label, d.valid_from, d.valid_until, d.created_at";

fn discount_from_row(row: &sqlx::postgres::PgRow) -> Result<Discount, WalletError> {
    Ok(Discount {
        discount_id: row.try_get("discount_id")?,
        organization_id: row.try_get("organization_id")?,
        organization: row.try_get("organization")?,
        model: row.try_get("model")?,
        percent: row.try_get("percent")?,
        label: row.try_get("label")?,
        valid_from: row.try_get("valid_from")?,
        valid_until: row.try_get("valid_until")?,
        created_at: row.try_get("created_at")?,
    })
}

impl Db {
    /// Every discount, newest first, ended or not — the full history, because a
    /// settlement snapshots only the percent and this list is how an operator
    /// reads back why a period billed what it did.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn discounts(&self) -> Result<Vec<Discount>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {DISCOUNT_COLS} FROM oxsum.pricing_discounts d \
             LEFT JOIN oxsum.organizations o USING (organization_id) \
             ORDER BY d.created_at DESC, d.discount_id"
        ))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(discount_from_row).collect()
    }

    /// Creates a discount row. The `idempotency_key` makes a retry the same
    /// write: replayed with the same fields it answers the row it created;
    /// under different fields it is [`WalletError::Conflict`].
    ///
    /// # Errors
    ///
    /// A percent outside 1..=100, a window that ends before it starts or an
    /// organization that does not exist is [`WalletError::InvalidInput`];
    /// storage failures surface as [`WalletError`].
    pub async fn create_discount(
        &self,
        idempotency_key: &str,
        discount: &NewDiscount,
    ) -> Result<Discount, WalletError> {
        if idempotency_key.is_empty() || idempotency_key.len() > 255 {
            return Err(WalletError::InvalidInput(
                "an idempotency key is 1..=255 characters".into(),
            ));
        }
        if !(1..=100).contains(&discount.percent) {
            return Err(WalletError::InvalidInput(
                "percent must be between 1 and 100".into(),
            ));
        }
        if let (Some(from), Some(until)) = (discount.valid_from, discount.valid_until)
            && until <= from
        {
            return Err(WalletError::InvalidInput(
                "validUntil must be after validFrom".into(),
            ));
        }
        if let Some(label) = &discount.label
            && label.len() > 200
        {
            return Err(WalletError::InvalidInput(
                "label must be at most 200 bytes".into(),
            ));
        }
        let inserted = sqlx::query(
            "INSERT INTO oxsum.pricing_discounts \
                 (idempotency_key, organization_id, model, percent, label, valid_from, valid_until) \
             VALUES ($1, $2, $3, $4, $5, COALESCE($6, now()), $7) \
             ON CONFLICT (idempotency_key) DO NOTHING \
             RETURNING discount_id",
        )
        .bind(idempotency_key)
        .bind(discount.organization_id)
        .bind(&discount.model)
        .bind(discount.percent)
        .bind(&discount.label)
        .bind(discount.valid_from)
        .bind(discount.valid_until)
        .fetch_optional(self.pool())
        .await
        .map_err(|error| {
            // The scope's FK names an organization that does not exist: a
            // request problem, not a storage one.
            let unknown = matches!(&error, sqlx::Error::Database(db)
                if db.constraint() == Some("pricing_discounts_organization_id_fkey"));
            if unknown {
                WalletError::InvalidInput("the organization does not exist".into())
            } else {
                WalletError::from(error)
            }
        })?;
        match inserted {
            Some(row) => self.discount_by_id(row.try_get("discount_id")?).await,
            // The key was used before: the row it wrote is the answer when the
            // request still matches it field for field — otherwise the key is
            // claimed by a different create.
            None => {
                let existing = self.discount_by_key(idempotency_key).await?;
                match existing {
                    Some(row) if same_discount(&row, discount) => Ok(row),
                    _ => Err(WalletError::Conflict(
                        "the idempotency key already created a different discount".into(),
                    )),
                }
            }
        }
    }

    /// One discount by id — `None` when no row carries it.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn discount_by_id(&self, id: Uuid) -> Result<Discount, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {DISCOUNT_COLS} FROM oxsum.pricing_discounts d \
             LEFT JOIN oxsum.organizations o USING (organization_id) \
             WHERE d.discount_id = $1"
        ))
        .bind(id)
        .fetch_one(self.pool())
        .await?;
        discount_from_row(&row)
    }

    /// The percent the settlement applies for this (organization, model) turn:
    /// the single most favorable row whose scope and window match — discounts
    /// never stack. `None` when nothing applies.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn discount_percent(
        &self,
        organization_id: Uuid,
        model: &str,
    ) -> Result<Option<i32>, WalletError> {
        let percent = sqlx::query_scalar(
            "SELECT percent FROM oxsum.pricing_discounts \
             WHERE (organization_id IS NULL OR organization_id = $1) \
               AND (model IS NULL OR model = $2) \
               AND valid_from <= now() \
               AND (valid_until IS NULL OR valid_until > now()) \
             ORDER BY percent DESC LIMIT 1",
        )
        .bind(organization_id)
        .bind(model)
        .fetch_optional(self.pool())
        .await?;
        Ok(percent)
    }

    /// Ends a discount early: `valid_until` becomes now, so turns starting
    /// after the call no longer qualify. `None` means no row carries the id;
    /// ending an already-ended or expired row answers it unchanged — a retried
    /// delete is the same state.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn end_discount(&self, id: Uuid) -> Result<Option<Discount>, WalletError> {
        sqlx::query(
            "UPDATE oxsum.pricing_discounts SET valid_until = now() \
             WHERE discount_id = $1 AND (valid_until IS NULL OR valid_until > now())",
        )
        .bind(id)
        .execute(self.pool())
        .await?;
        let row = sqlx::query(&format!(
            "SELECT {DISCOUNT_COLS} FROM oxsum.pricing_discounts d \
             LEFT JOIN oxsum.organizations o USING (organization_id) \
             WHERE d.discount_id = $1"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(discount_from_row).transpose()
    }

    /// The row an idempotency key already wrote, for the replay path.
    async fn discount_by_key(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<Discount>, WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {DISCOUNT_COLS} FROM oxsum.pricing_discounts d \
             LEFT JOIN oxsum.organizations o USING (organization_id) \
             WHERE d.idempotency_key = $1"
        ))
        .bind(idempotency_key)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(discount_from_row).transpose()
    }
}

/// Whether the stored row is the request replayed — every field equal, with
/// `valid_from` compared loosely: an absent `valid_from` wrote `now()` on the
/// first call, so the replay accepts it whenever the stored start is what the
/// insert would have produced. A caller that needs the field compared exactly
/// sends it explicitly; timestamps compare at the microsecond precision
/// PostgreSQL stores, so a nanosecond-shaped request is not a false conflict.
fn same_discount(row: &Discount, request: &NewDiscount) -> bool {
    let micros = |t: OffsetDateTime| t.unix_timestamp_nanos() / 1_000;
    row.percent == request.percent
        && row.organization_id == request.organization_id
        && row.model == request.model
        && row.label == request.label
        && row.valid_until.map(micros) == request.valid_until.map(micros)
        && request
            .valid_from
            .is_none_or(|from| micros(from) == micros(row.valid_from))
}
