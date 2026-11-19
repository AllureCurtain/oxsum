//! Source-verified purchased-credit corrections on the caller's transaction.

use doubleentry::{Direction, DocumentRef, Draft, Entry, EntryId, Layer, Provenance};
use sqlx::{Postgres, Transaction};
use time::Date;
use uuid::Uuid;

use super::{
    Credits, Receipt, SCALE, Wallet, currency, description_of, entry_id_for, idem,
    settlement_key_for,
};
use crate::{RequestCorrectionInput, RequestCorrectionPreview, SettlementRecord, WalletError};

impl Wallet {
    /// Read and verify pure purchased-credit funding and cumulative correction.
    pub(crate) async fn correction_source_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        organization: Uuid,
        source: Uuid,
    ) -> Result<RequestCorrectionPreview, WalletError> {
        let stored = self
            .entry_in(tx, EntryId::from_uuid(source))
            .await?
            .ok_or_else(|| WalletError::NotFound("source charge not found".into()))?;
        let record =
            SettlementRecord::parse(stored.entry.description().as_str()).ok_or_else(|| {
                WalletError::InvalidInput("source must be a gateway settlement".into())
            })?;
        stored.require_index().map_err(crate::error::invalid)?;
        let expected_key = settlement_key_for(&format!("req-{}:hold", record.request));
        let reversed: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM ledger_{}.entries WHERE reverses=$1)",
            self.tenant_id
        ))
        .bind(source)
        .fetch_one(&mut **tx)
        .await?;
        if stored.entry.id() != entry_id_for(&expected_key)
            || stored.entry.idempotency_key().as_bytes() != expected_key.as_bytes()
            || stored.entry.reverses().is_some()
            || reversed
            || record.service.is_some()
            || stored.entry.document().is_some()
            || stored
                .entry
                .postings()
                .iter()
                .any(|p| p.currency != currency())
        {
            return Err(WalletError::Conflict(
                "source is not an unreversed committed gateway charge".into(),
            ));
        }
        let mut wallet_debit = 0_i128;
        let mut revenue_credit = 0_i128;
        let mut unsupported = record
            .subscription
            .as_ref()
            .is_some_and(|s| s.charged_minor > 0);
        for p in stored
            .entry
            .postings()
            .iter()
            .filter(|p| p.layer == Layer::Settled)
        {
            if p.account == self.wallet && p.direction == Direction::Debit {
                wallet_debit += i128::from(p.amount.to_minor());
            } else if p.account == self.revenue && p.direction == Direction::Credit {
                revenue_credit += i128::from(p.amount.to_minor());
            } else {
                unsupported = true;
            }
        }
        if unsupported
            || record.charged <= 0
            || wallet_debit != i128::from(record.charged)
            || revenue_credit != wallet_debit
        {
            return Err(WalletError::Conflict("unsupported correction funding: only a fully purchased-credit charge is eligible; use the appropriate plan, promotion or debt workflow".into()));
        }
        let corrected: i64 = sqlx::query_scalar("SELECT COALESCE(sum(amount_minor),0)::bigint FROM oxsum.request_corrections WHERE organization_id=$1 AND source_entry_id=$2")
            .bind(organization).bind(source).fetch_one(&mut **tx).await?;
        let remaining = record
            .charged
            .checked_sub(corrected)
            .filter(|n| *n >= 0)
            .ok_or_else(|| WalletError::Misconfigured("correction total exceeds source".into()))?;
        Ok(RequestCorrectionPreview { source_entry_id: source, source_hash: stored.content_hash.to_hex(), request_id: record.request, charged_minor: record.charged, corrected_minor: corrected, remaining_minor: remaining, effect: "Purchased wallet credit only; no cash refund, debt change, promotional entitlement or key/mandate budget restoration".into() })
    }

    /// Append a correction after source/remaining checks under the append lock.
    pub(crate) async fn append_request_correction_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        correction: Uuid,
        source: &RequestCorrectionPreview,
        request: &RequestCorrectionInput,
        actor: Uuid,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if self.day_is_anchored_in(tx, on).await? {
            return Err(WalletError::DaySealed);
        }
        let key = format!("request-correction:{correction}");
        let description = serde_json::json!({"kind":"request_credit_correction","v":1,"sourceEntryId":source.source_entry_id,"sourceHash":source.source_hash,"requestId":source.request_id,"amountMinor":request.amount_minor,"actorId":actor,"reason":request.reason}).to_string();
        let draft = Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
            .with_description(description_of(&description)?)
            .with_document(
                DocumentRef::unverified("oxsum/request-credit-correction/v1")
                    .map_err(crate::error::invalid)?,
            )
            .with_provenance(
                Provenance::none()
                    .with_actor(&format!("correction:{actor}"))
                    .map_err(crate::error::invalid)?,
            )
            .debit(
                self.revenue,
                Credits::from_minor(request.amount_minor),
                currency(),
            )
            .credit(
                self.wallet,
                Credits::from_minor(request.amount_minor),
                currency(),
            );
        self.append_in(tx, draft).await
    }
}
