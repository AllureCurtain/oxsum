//! Immutable source fences for later refund and chargeback cash outcomes.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::WalletError;

pub(crate) async fn corrected_order_journals_in(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<Vec<Uuid>, WalletError> {
    Ok(sqlx::query_scalar("SELECT journal_id FROM oxsum.plan_usage_correction_sources WHERE order_id=$1 ORDER BY journal_id")
        .bind(order_id).fetch_all(&mut **tx).await?)
}
