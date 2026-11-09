//! Immediate customer receipt readiness from scoped accounting and canonical evidence.

use doubleentry::storage::postgres::PostgresError;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    CustomerCapability, Db, EntryId, Principal, SettlementKind, SettlementRecord, Wallet,
    WalletError,
};

const CANDIDATES: i64 = 32;

/// A successful visible call whose canonical receipt can be opened immediately.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerReadyReceipt {
    /// Request identity committed by the receipt and its accounting row.
    pub request_id: String,
    /// Canonical entry to open on the verification page.
    pub entry_id: Uuid,
    /// Current canonical content hash; independent trust still requires retention.
    pub content_hash: String,
    /// Public model recorded on the successful call.
    pub model: String,
    /// Total priced usage, including the part paid by subscription quota.
    pub charged_minor: i64,
    /// Actual wallet charge after the recorded subscription allocation.
    pub wallet_charged_minor: i64,
}

/// A bounded existence check, not a completeness audit or external trust verdict.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerReceiptReadiness {
    /// `ready`, `missing` (no eligible row), or `incomplete` (evidence not established).
    pub state: &'static str,
    /// Present only after canonical identity, scope, inclusion and arithmetic checks.
    pub receipt: Option<CustomerReadyReceipt>,
}

impl Db {
    /// Read a private receipt for the verification page without creating an absent ledger.
    /// The caller must supply a freshly authenticated personal customer session.
    pub async fn customer_receipt_bundle(
        &self,
        principal: &Principal,
        entry: EntryId,
        signing: Option<&crate::HeadSigningKey>,
    ) -> Result<Option<crate::ProofBundle>, WalletError> {
        principal.require_customer(CustomerCapability::Personal)?;
        let scope = self.customer_record_scope(principal).await?;
        let tenant = &principal.organization().tenant_id;
        let exists = sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text")
            .bind(format!(
                "\"ledger_{}\".entries",
                tenant.replace('"', "\"\"")
            ))
            .fetch_one(self.pool())
            .await?
            .is_some();
        if !exists {
            return Ok(None);
        }
        let wallet = Wallet::open_existing(self.pool().clone(), tenant).await?;
        wallet.scoped_receipt_proof(entry, signing, &scope).await
    }

    /// Find a visible successful accounting row backed by recomputable canonical evidence.
    /// Personal authority and customer identity are derived from the authenticated principal.
    /// At most 32 candidates are verified; this read creates no accounting or report records.
    pub async fn customer_receipt_readiness(
        &self,
        principal: &Principal,
    ) -> Result<CustomerReceiptReadiness, WalletError> {
        principal.require_customer(CustomerCapability::Personal)?;
        let scope = self.customer_record_scope(principal).await?;
        let tenant = &principal.organization().tenant_id;
        let keys = scope
            .key_ids
            .as_ref()
            .map(|keys| {
                keys.iter()
                    .map(|key| {
                        Uuid::parse_str(key).map_err(|_| {
                            WalletError::Misconfigured("invalid scoped key identity".into())
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let rows = sqlx::query(
            "SELECT request_id,entry_id,key_id,model,channel,price_version,kind,charged_minor,freeze_minor \
             FROM oxsum.usage_records WHERE tenant_id=$1 AND status='settled' \
             AND kind IN ('usage','estimated','capped') AND failure_reason IS NULL \
             AND (($2::uuid[] IS NULL) OR key_id=ANY($2) OR (key_id IS NULL AND $3)) \
             ORDER BY settled_at DESC,request_id DESC LIMIT $4",
        )
        .bind(tenant)
        .bind(keys)
        .bind(scope.include_unattributed)
        .bind(CANDIDATES)
        .fetch_all(self.pool())
        .await?;
        if rows.is_empty() {
            return Ok(CustomerReceiptReadiness {
                state: "missing",
                receipt: None,
            });
        }
        let wallet = Wallet::open_existing(self.pool().clone(), tenant).await?;
        for row in rows {
            let entry_id: Uuid = row.try_get("entry_id")?;
            let bundle = match wallet
                .scoped_receipt_proof(EntryId::from_uuid(entry_id), None, &scope)
                .await
            {
                Ok(Some(bundle)) => bundle,
                Ok(None)
                | Err(WalletError::Storage(
                    PostgresError::Integrity(_) | PostgresError::Malformed(_),
                )) => continue,
                Err(error) => return Err(error),
            };
            let description = bundle.entry.description().as_str();
            let Some(record) = SettlementRecord::parse(description) else {
                continue;
            };
            let request_id: String = row.try_get("request_id")?;
            let key_id: Option<Uuid> = row.try_get("key_id")?;
            let actor = bundle
                .entry
                .provenance()
                .actor
                .as_ref()
                .map(|actor| actor.as_str());
            let key = key_id.map(|id| id.simple().to_string());
            let Ok(ledger_key) = std::str::from_utf8(bundle.entry.idempotency_key().as_bytes())
            else {
                continue;
            };
            let quota_charge = record
                .subscription
                .as_ref()
                .map_or(0, |quota| quota.charged_minor);
            let Some(wallet_charge) = record.charged.checked_sub(quota_charge) else {
                continue;
            };
            if quota_charge < 0
                || wallet_charge < 0
                || crate::entry_id_for(ledger_key).as_uuid() != &entry_id
                || bundle.entry.id().as_uuid() != &entry_id
                || actor != key.as_deref()
                || record.request != request_id
                || record.model != row.try_get::<String, _>("model")?
                || record.channel != row.try_get::<String, _>("channel")?
                || record.price_version != row.try_get::<i64, _>("price_version")?
                || record.kind.as_str() != row.try_get::<String, _>("kind")?
                || record.charged != row.try_get::<i64, _>("charged_minor")?
                || record.freeze != row.try_get::<i64, _>("freeze_minor")?
                || !matches!(
                    record.kind,
                    SettlementKind::Usage | SettlementKind::Estimated | SettlementKind::Capped
                )
                || !wallet.receipt_charge_matches(&bundle.entry, wallet_charge)
                || record.reason.is_some()
                || record.reason_commitment.is_some()
                || record
                    .guardrail
                    .iter()
                    .any(|verdict| verdict.verdict == "blocked" && !verdict.streamed)
                || oxsum_verify::verify_charge(description) != oxsum_verify::ChargeCheck::Recomputed
            {
                continue;
            }
            let hash = bundle.entry.content_hash();
            let json = serde_json::to_string(&bundle)
                .map_err(|error| WalletError::Misconfigured(error.to_string()))?;
            if !oxsum_verify::verify_bundle(&json, &hash).unwrap_or(false) {
                continue;
            }
            return Ok(CustomerReceiptReadiness {
                state: "ready",
                receipt: Some(CustomerReadyReceipt {
                    request_id,
                    entry_id,
                    content_hash: hash.to_hex(),
                    model: record.model,
                    charged_minor: record.charged,
                    wallet_charged_minor: wallet_charge,
                }),
            });
        }
        Ok(CustomerReceiptReadiness {
            state: "incomplete",
            receipt: None,
        })
    }
}
