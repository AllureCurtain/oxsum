//! Credit notes (issue #352, decision D12): a reasoned credit the operator
//! issues that moves no money.
//!
//! The ledger entry is the fact — a settlement's charge reversed in its own
//! direction (Revenue debited, Wallet credited) with the reason as its
//! description — and `oxsum.credit_notes` is the lookup the admin surface
//! lists. A note is never a rewrite: it is a new entry, so history keeps both
//! the charge and the credit.
//!
//! A credit note is **not** a refund: no PSP is called and no deposit is
//! reversed. The refund and chargeback halves of D12 are separate issues.

use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::wallet::Wallet;

/// One issued credit note, as the admin list answers it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditNote {
    /// The note's own id.
    pub credit_note_id: Uuid,
    /// The organization whose wallet was credited.
    pub organization_id: Uuid,
    /// The ledger entry that carries the credit.
    pub entry_id: Uuid,
    /// The amount credited, minor units, always positive.
    pub amount_minor: i64,
    /// Why the operator issued it — also the entry's description.
    pub reason: String,
    /// When it was issued.
    pub created_at: OffsetDateTime,
}

impl Db {
    /// Issues a credit note into one organization's wallet.
    ///
    /// The wallet validates the shape (a positive amount, a non-empty reason)
    /// and appends the entry; the row and the entry share one idempotency key,
    /// so a retried request answers the standing note instead of posting a
    /// second one.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a zero or negative amount, an empty
    /// reason, or a malformed key; storage and ledger failures surface as
    /// [`WalletError`].
    pub async fn issue_credit_note(
        &self,
        wallet: &Wallet,
        organization_id: Uuid,
        amount_minor: i64,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<CreditNote, WalletError> {
        let receipt = wallet
            .credit_note(
                idempotency_key,
                reason,
                amount_minor,
                OffsetDateTime::now_utc().date(),
            )
            .await?;
        let entry_id = *receipt.entry_id.as_uuid();
        // A replayed key lands on the standing entry; answer its row.
        if let Some(existing) = self.credit_note_by_entry(entry_id).await? {
            return Ok(existing);
        }
        sqlx::query(
            "INSERT INTO oxsum.credit_notes \
                 (credit_note_id, organization_id, entry_id, amount_minor, reason) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (organization_id, entry_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(organization_id)
        .bind(entry_id)
        .bind(amount_minor)
        .bind(reason)
        .execute(self.pool())
        .await?;
        self.credit_note_by_entry(entry_id)
            .await?
            .ok_or_else(|| WalletError::InvalidInput("the credit note did not land".into()))
    }

    /// One organization's credit notes, newest first.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn credit_notes(
        &self,
        organization_id: Uuid,
    ) -> Result<Vec<CreditNote>, WalletError> {
        let rows = sqlx::query(
            "SELECT credit_note_id, organization_id, entry_id, amount_minor, reason, created_at \
             FROM oxsum.credit_notes \
             WHERE organization_id = $1 \
             ORDER BY created_at DESC, credit_note_id DESC",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(credit_note_from_row).collect()
    }

    async fn credit_note_by_entry(
        &self,
        entry_id: Uuid,
    ) -> Result<Option<CreditNote>, WalletError> {
        let row = sqlx::query(
            "SELECT credit_note_id, organization_id, entry_id, amount_minor, reason, created_at \
             FROM oxsum.credit_notes WHERE entry_id = $1",
        )
        .bind(entry_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(credit_note_from_row).transpose()
    }
}

/// Reads one row into the wire shape.
fn credit_note_from_row(row: &sqlx::postgres::PgRow) -> Result<CreditNote, WalletError> {
    Ok(CreditNote {
        credit_note_id: row.try_get("credit_note_id")?,
        organization_id: row.try_get("organization_id")?,
        entry_id: row.try_get("entry_id")?,
        amount_minor: row.try_get("amount_minor")?,
        reason: row.try_get("reason")?,
        created_at: row.try_get("created_at")?,
    })
}
