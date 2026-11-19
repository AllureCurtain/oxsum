//! Readable customer-safe receipts assembled from immutable ledger evidence.

use doubleentry::{Direction, EntryId, Layer, LedgerStore, StoredEntry};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use super::{SCALE, Wallet};
use crate::{CustomerRecordScope, Db, SettlementRecord, WalletError, wallet};

/// One historical price line, in integer credit minor units per million units.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptLine {
    /// Named billable dimension.
    pub item: String,
    /// Recorded quantity.
    pub units: i64,
    /// Historical rate, never today's catalog.
    pub price_per_million: i64,
}

/// Actual pool movement at a position in the customer's ledger.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptMovement {
    /// Immutable entry identity.
    pub entry_id: String,
    /// Actual ledger ordering; intervening unrelated entries may exist.
    pub index: u64,
    /// Ledger posting day, separate from execution time.
    pub booked_on: String,
    /// Purchased credit, bonus, promotion or credit line.
    pub pool: String,
    /// Pending debit taken by this entry.
    pub reserved_minor: i64,
    /// Pending credit released by this entry, including the subsequently charged share.
    pub released_minor: i64,
    /// Signed effect on settled funds; a charge is negative.
    pub settled_effect_minor: i64,
}

/// An allowlisted attempt timeline without provider payloads or credentials.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptAttempt {
    /// Durable attempt identity.
    pub attempt_id: Uuid,
    /// Provider's recorded request identity, if available.
    pub external_id: Option<String>,
    /// Observed route outcome; not proof of customer delivery.
    pub status: String,
    /// Attempt start.
    pub started_at: String,
    /// Observed end, if recorded.
    pub completed_at: Option<String>,
}

/// Customer-safe correction facts authorized through their original charge.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptCorrection {
    /// Immutable correction identity.
    pub id: Uuid,
    /// Credited entry, separate from the original.
    pub entry_id: Uuid,
    /// Verified credited entry hash.
    pub content_hash: String,
    /// Purchased wallet credit.
    pub amount_minor: i64,
    /// Customer-visible explanation supplied by staff.
    pub reason: String,
    /// UTC correction time.
    pub created_at: String,
}

/// A readable receipt. Financial facts come from verified ledger entries;
/// operation diagnostics are explicitly separate and may be absent.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargeReceipt {
    /// Gateway request identity.
    pub request_id: String,
    /// Durable operation when retained.
    pub operation_id: Option<Uuid>,
    /// Observed execution lifecycle, not a delivery guarantee.
    pub execution_state: String,
    /// Stored operation lifecycle, which may describe financial closure only.
    pub operation_state: Option<String>,
    /// Immutable financial classification, separate from usage provenance.
    pub settlement_kind: Option<String>,
    /// Whether an immutable settlement exists.
    pub financial_state: String,
    /// Requested public model.
    pub requested_model: String,
    /// Served public model, unknown before settlement.
    pub served_model: Option<String>,
    /// Admission time, absent for older ledger-only history.
    pub created_at: Option<String>,
    /// Terminal observation time, when retained.
    pub completed_at: Option<String>,
    /// Settlement identity for independent verification.
    pub entry_id: Option<String>,
    /// Immutable settlement hash.
    pub content_hash: Option<String>,
    /// Historical channel price version.
    pub price_version: Option<i64>,
    /// Committed historical customer sale snapshot digest.
    pub sale_digest: Option<String>,
    /// Historical sale book version, if preserved.
    pub sale_version: Option<i64>,
    /// Historical exception policy interpretation.
    pub policy: String,
    /// Recorded metering provenance; missing is not zero consumption.
    pub usage_basis: String,
    /// Original customer charge ceiling.
    pub ceiling_minor: i64,
    /// Total retail charge including entitlement coverage.
    pub charged_minor: Option<i64>,
    /// Actual settled debit across wallet pools.
    pub wallet_charged_minor: Option<i64>,
    /// Charged subscription entitlement, separate from wallet pools.
    pub subscription_charged_minor: Option<i64>,
    /// Unused original ceiling, not a refund of an earlier settled debit.
    pub released_minor: Option<i64>,
    /// Purchased credit returned by source-linked corrections.
    pub corrected_minor: i64,
    /// Original charge less source-linked credits.
    pub net_charged_minor: Option<i64>,
    /// Safe linked facts; scoped callers do not gain access to whole credit entries.
    pub corrections: Vec<ReceiptCorrection>,
    /// Exact historical priced dimensions.
    pub lines: Vec<ReceiptLine>,
    /// Recorded discount, applied before the customer ceiling.
    pub discount_percent: Option<i64>,
    /// Recorded arithmetic interpretation.
    pub rounding: String,
    /// Pool movements in actual ledger order, never hypothetical balances.
    pub ledger: Vec<ReceiptMovement>,
    /// Bounded, customer-safe attempt timeline.
    pub attempts: Vec<ReceiptAttempt>,
    /// Missing data and interpretation boundaries.
    pub diagnostics: Vec<String>,
}

impl Wallet {
    /// Find one authorized receipt without consulting request analytics. UUIDs
    /// resolve operations or immutable entries; other identifiers resolve exact
    /// provider identities. Ambiguous identifiers refuse instead of choosing.
    pub async fn charge_receipt(
        &self,
        db: &Db,
        organization_id: Uuid,
        identifier: &str,
        scope: &CustomerRecordScope,
    ) -> Result<Option<ChargeReceipt>, WalletError> {
        if identifier.is_empty() || identifier.len() > 256 {
            return Err(WalletError::InvalidInput(
                "identifier must be 1 to 256 bytes".into(),
            ));
        }
        let parsed = Uuid::parse_str(identifier).ok();
        let rows = sqlx::query(
            "SELECT DISTINCT o.operation_id, o.key_id FROM oxsum.request_operations o \
             LEFT JOIN oxsum.request_attempts a USING (operation_id) \
             WHERE o.organization_id=$1 AND (o.operation_id=$2 OR a.external_id=$3)",
        )
        .bind(organization_id)
        .bind(parsed)
        .bind(identifier)
        .fetch_all(db.pool())
        .await?;
        let mut ids = Vec::new();
        for row in rows {
            let key: Option<Uuid> = row.try_get("key_id")?;
            if scope.allows(key.as_ref().map(|k| k.simple().to_string()).as_deref()) {
                ids.push(row.try_get::<Uuid, _>("operation_id")?);
            }
        }
        if ids.len() > 1 {
            return Err(WalletError::Conflict(
                "identifier matches several operations; use an operation ID".into(),
            ));
        }
        let mut operation = match ids.first() {
            Some(id) => db.operation(organization_id, *id).await?,
            None => None,
        };
        let mut request = operation.as_ref().map(|o| o.operation_id.to_string());
        // Entry lookup handles historical operations whose diagnostic rows have
        // gone away. Scope applies before even reading the description.
        if request.is_none()
            && let Some(id) = parsed
        {
            if let Some(stored) = self.store.get(EntryId::from_uuid(id)).await? {
                if !scope.allows_entry(&stored.entry) {
                    return Ok(None);
                }
                let description: serde_json::Value =
                    serde_json::from_str(stored.entry.description().as_str()).unwrap_or_default();
                request = description
                    .get("request")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
            }
            if request.is_none() {
                request = Some(id.to_string());
            }
        }
        let Some(request) = request else {
            return Ok(None);
        };
        if operation.is_none()
            && let Ok(id) = Uuid::parse_str(&request)
        {
            operation = db.operation(organization_id, id).await?;
        }
        let hold_key = operation
            .as_ref()
            .and_then(|o| o.hold_key.clone())
            .unwrap_or_else(|| format!("req-{request}:hold"));
        let hold = self.store.get(wallet::entry_id_for(&hold_key)).await?;
        let settled = self
            .store
            .get(wallet::entry_id_for(&wallet::settlement_key_for(&hold_key)))
            .await?;
        for stored in [hold.as_ref(), settled.as_ref()].into_iter().flatten() {
            if !scope.allows_entry(&stored.entry) {
                return Ok(None);
            }
        }
        if hold.is_none() && settled.is_none() && operation.is_none() {
            return Ok(None);
        }
        if let Some(o) = &operation
            && !scope.allows(o.key_id.as_ref().map(|k| k.simple().to_string()).as_deref())
        {
            return Ok(None);
        }
        let record = settled
            .as_ref()
            .and_then(|s| SettlementRecord::parse(s.entry.description().as_str()));
        if settled.is_some() && record.is_none() {
            return Err(WalletError::InvalidInput(
                "entry has no supported gateway settlement record".into(),
            ));
        }
        let mut diagnostics = vec![
            "Execution observations do not prove customer receipt or provider usage truth.".into(),
            "Ledger indices include concurrent activity; these are operation effects, not account before/after balances.".into(),
            "Releasing an unused reservation is not a cash refund.".into(),
        ];
        if operation.is_none() {
            diagnostics.push("Historical operation diagnostics are unavailable; financial facts remain in the ledger.".into());
        }
        let mut ledger = Vec::new();
        for stored in [hold.as_ref(), settled.as_ref()].into_iter().flatten() {
            ledger.extend(self.receipt_movements(stored)?);
        }
        let burn = self
            .store
            .get(wallet::entry_id_for(&format!("{hold_key}:promo-burn")))
            .await?;
        if let Some(burn) = &burn {
            ledger.extend(self.receipt_movements(burn)?);
            diagnostics.push("Unused promotional reservation was released and then expired; see the separate promotion debit. It did not become spendable purchased credit.".into());
        }
        let mut corrections = Vec::new();
        let mut corrected_minor = 0_i64;
        if let Some(source) = &settled {
            let rows = sqlx::query("SELECT * FROM oxsum.request_corrections WHERE organization_id=$1 AND source_entry_id=$2 ORDER BY created_at,correction_id LIMIT 1001")
                .bind(organization_id).bind(source.entry.id().as_uuid()).fetch_all(db.pool()).await?;
            if rows.len() > 1000 {
                return Err(WalletError::InvalidInput(
                    "receipt correction detail limit exceeded; use period evidence".into(),
                ));
            }
            for row in rows {
                let id: Uuid = row.try_get("correction_id")?;
                let entry_id: Uuid = row.try_get("entry_id")?;
                let content_hash: String = row.try_get("content_hash")?;
                let amount: i64 = row.try_get("amount_minor")?;
                let credit = self
                    .store
                    .get(EntryId::from_uuid(entry_id))
                    .await?
                    .ok_or_else(|| WalletError::Misconfigured("correction entry missing".into()))?;
                let key = format!("request-correction:{id}");
                if credit.content_hash.to_hex() != content_hash
                    || row.try_get::<String, _>("source_hash")? != source.content_hash.to_hex()
                    || credit.entry.id() != wallet::entry_id_for(&key)
                    || credit.entry.idempotency_key().as_bytes() != key.as_bytes()
                {
                    return Err(WalletError::Misconfigured(
                        "correction source integrity mismatch".into(),
                    ));
                }
                let movement = self.receipt_movements(&credit)?;
                if movement.len() != 1
                    || movement[0].pool != "purchased_credit"
                    || movement[0].settled_effect_minor != amount
                    || amount <= 0
                {
                    return Err(WalletError::Misconfigured(
                        "correction effect mismatch".into(),
                    ));
                }
                corrected_minor = corrected_minor
                    .checked_add(amount)
                    .ok_or_else(|| WalletError::Misconfigured("correction sum overflow".into()))?;
                ledger.extend(movement);
                corrections.push(ReceiptCorrection {
                    id,
                    entry_id,
                    content_hash,
                    amount_minor: amount,
                    reason: row.try_get("reason")?,
                    created_at: row
                        .try_get::<time::OffsetDateTime, _>("created_at")?
                        .to_string(),
                });
            }
        }
        ledger.sort_by_key(|m| m.index);
        let wallet_charged = settled.as_ref().map(|s| {
            -ledger
                .iter()
                .filter(|m| m.entry_id == s.entry.id().to_string())
                .map(|m| m.settled_effect_minor)
                .sum::<i64>()
        });
        let mut attempts = Vec::new();
        if let Some(o) = &operation {
            for a in db
                .operation_attempts(o.operation_id)
                .await?
                .into_iter()
                .take(100)
            {
                attempts.push(ReceiptAttempt {
                    attempt_id: a.attempt_id,
                    external_id: a.external_id,
                    status: a.status.as_str().into(),
                    started_at: a.started_at.to_string(),
                    completed_at: a.completed_at.map(|t| t.to_string()),
                });
            }
            if attempts.len() == 100 {
                diagnostics.push("Attempt timeline is limited to the first 100 records.".into());
            }
        }
        if attempts.is_empty() {
            diagnostics.push("No retained provider-attempt diagnostics; this does not establish zero supplier consumption.".into());
        }
        let sale_digest = record
            .as_ref()
            .and_then(|r| r.sale.as_ref().map(|s| s.digest.clone()));
        let mut sale_version = None;
        if let Some(digest) = &sale_digest {
            if let Some((_, canonical)) = db.sale_snapshot_by_digest(digest).await? {
                if crate::sale_snapshot_hash(&canonical).to_hex() != *digest {
                    return Err(WalletError::Misconfigured(
                        "historical sale digest mismatch".into(),
                    ));
                }
                let sale: serde_json::Value =
                    serde_json::from_str(&canonical).map_err(crate::error::invalid)?;
                sale_version = sale.get("version").and_then(serde_json::Value::as_i64);
            } else {
                diagnostics.push("The committed sale snapshot is unavailable; current prices were not substituted.".into());
            }
        }
        let provenance = if let Some(stored) = &settled {
            sqlx::query("SELECT provider_input_tokens, provider_output_tokens, local_input_tokens, local_output_tokens FROM oxsum.usage_records WHERE tenant_id=$1 AND entry_id=$2")
                .bind(&self.tenant_id).bind(stored.entry.id().as_uuid()).fetch_optional(db.pool()).await?
        } else {
            None
        };
        let usage_basis = if record.as_ref().is_some_and(|r| r.cache.is_some()) {
            "cache_reuse"
        } else if let Some(row) = provenance {
            let provider: Option<i64> = row.try_get("provider_input_tokens")?;
            let local: Option<i64> = row.try_get("local_input_tokens")?;
            if provider.is_some() {
                "provider_reported"
            } else if local.is_some() {
                "local_estimate"
            } else {
                "missing"
            }
        } else if record
            .as_ref()
            .is_some_and(|r| r.kind == crate::SettlementKind::Estimated)
        {
            "local_estimate"
        } else {
            "unknown"
        };
        if usage_basis == "local_estimate" {
            diagnostics.push("Legacy local estimate: reportless completed turns use input only; cancellation can count a bounded forwarded prefix. Hidden provider quantities are not independently established.".into());
        }
        let hold_metadata: Option<serde_json::Value> = hold
            .as_ref()
            .and_then(|h| serde_json::from_str(h.entry.description().as_str()).ok());
        let historical_freeze = hold_metadata
            .as_ref()
            .and_then(|h| h.get("freeze"))
            .and_then(serde_json::Value::as_i64)
            .filter(|n| *n >= 0);
        let historical_model = hold_metadata
            .as_ref()
            .and_then(|h| h.get("model"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let ceiling = record
            .as_ref()
            .map(|r| r.freeze)
            .or_else(|| operation.as_ref().map(|o| o.bound_minor))
            .or(historical_freeze)
            .unwrap_or_else(|| ledger.iter().map(|l| l.reserved_minor).sum());
        let charged = record.as_ref().map(|r| r.charged);
        let bound_terms = if let Ok(id) = Uuid::parse_str(&request) {
            db.operation_billing_terms(organization_id, id).await?
        } else {
            None
        };
        let policy = bound_terms.as_ref().and_then(|t| t.selection.policy.as_ref().map(|p| {
            format!("{}; version {}; digest {}; acceptance {}; applies to this request: {}. {} Application evidence; the existing entry verifier does not independently verify authenticated consent.",p.implementation,p.version,p.digest,t.selection.acceptance.as_ref().map(|a|a.id.to_string()).unwrap_or_default(),t.policy_applies,p.disclosure)
        })).unwrap_or_else(|| crate::billing_policies::LEGACY_DISCLOSURE.into());
        Ok(Some(ChargeReceipt {
            request_id: request,
            operation_id: operation.as_ref().map(|o| o.operation_id),
            execution_state: if operation.is_some() { "Completion and customer delivery are not established by the operation lifecycle" } else { "diagnostics_unavailable" }.into(),
            operation_state: operation.as_ref().map(|o| o.state.as_str().to_owned()),
            settlement_kind: record.as_ref().map(|r| r.kind.as_str().to_owned()),
            financial_state: if settled.is_some() { "settled" } else { "pending" }.into(),
            requested_model: record.as_ref().map(|r| r.requested_model.clone()).or_else(|| operation.as_ref().map(|o| o.model.clone())).or(historical_model).unwrap_or_else(|| "Not recorded".into()),
            served_model: record.as_ref().map(|r| r.served_model.clone()),
            created_at: operation.as_ref().map(|o| o.created_at.to_string()),
            completed_at: operation.as_ref().and_then(|o| o.completed_at.map(|t| t.to_string())),
            entry_id: settled.as_ref().map(|s| s.entry.id().to_string()),
            content_hash: settled.as_ref().map(|s| s.content_hash.to_hex()),
            price_version: record.as_ref().map(|r| r.price_version),
            sale_digest, sale_version,
            policy,
            usage_basis: usage_basis.into(),
            ceiling_minor: ceiling,
            charged_minor: charged,
            corrected_minor,
            net_charged_minor: charged.map(|c| c - corrected_minor),
            corrections,
            wallet_charged_minor: wallet_charged,
            subscription_charged_minor: record.as_ref().map(|r| r.subscription.as_ref().map_or(0, |s| s.charged_minor)),
            released_minor: charged.map(|c| ceiling - c),
            lines: record.as_ref().map(|r| r.lines.iter().map(|l| ReceiptLine { item: l.item.clone(), units: l.units, price_per_million: l.price_per_m }).collect()).unwrap_or_default(),
            discount_percent: record.as_ref().and_then(|r| r.discount_percent),
            rounding: "Sum quantity × historical rate; apply recorded discount to that numerator; divide by 1,000,000 (and 100 for a discount) with one rounding up; cap at original ceiling. The verifier checks the supported recorded schema.".into(),
            ledger, attempts, diagnostics,
        }))
    }

    fn receipt_movements(
        &self,
        stored: &StoredEntry<SCALE>,
    ) -> Result<Vec<ReceiptMovement>, WalletError> {
        let mut rows = Vec::new();
        for (account, pool) in [
            (self.wallet, "purchased_credit"),
            (self.bonus, "bonus"),
            (self.promo, "promotion"),
            (self.credit_line, "credit_line"),
        ] {
            let mut row = ReceiptMovement {
                entry_id: stored.entry.id().to_string(),
                index: stored.require_index().map_err(crate::error::invalid)?.get(),
                booked_on: stored.entry.booking_date().to_string(),
                pool: pool.into(),
                reserved_minor: 0,
                released_minor: 0,
                settled_effect_minor: 0,
            };
            for p in stored
                .entry
                .postings()
                .iter()
                .filter(|p| p.account == account)
            {
                match (p.layer, p.direction) {
                    (Layer::Pending, Direction::Debit) => row.reserved_minor += p.amount.to_minor(),
                    (Layer::Pending, Direction::Credit) => {
                        row.released_minor += p.amount.to_minor()
                    }
                    (Layer::Settled, Direction::Debit) => {
                        row.settled_effect_minor -= p.amount.to_minor()
                    }
                    (Layer::Settled, Direction::Credit) => {
                        row.settled_effect_minor += p.amount.to_minor()
                    }
                }
            }
            if row.reserved_minor != 0 || row.released_minor != 0 || row.settled_effect_minor != 0 {
                rows.push(row);
            }
        }
        Ok(rows)
    }
}

/// A Business-wide exact lookup also identifies the owning customer.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessChargeReceipt {
    /// Customer organization that owns this ledger.
    pub organization_id: Uuid,
    /// Customer display name for the staff journey.
    pub customer_name: String,
    /// The same safe shape returned on the customer's own endpoint.
    pub receipt: ChargeReceipt,
}

impl Db {
    /// Discover the owning customer from exact durable identifiers. A lookup
    /// probes existing customer ledgers by primary key, never scans analytics
    /// or creates a wallet. Reused external identifiers fail as ambiguous.
    pub async fn business_charge_receipt(
        &self,
        token: &str,
        identifier: &str,
    ) -> Result<Option<BusinessChargeReceipt>, WalletError> {
        if identifier.is_empty() || identifier.len() > 256 {
            return Err(WalletError::InvalidInput(
                "identifier must be 1 to 256 bytes".into(),
            ));
        }
        let principal = self
            .business_capability(token, crate::BusinessCapability::CustomersRead)
            .await?;
        let rows = sqlx::query("SELECT o.organization_id, o.name, o.tenant_id FROM oxsum.organizations o JOIN oxsum.customer_profiles c USING (organization_id) WHERE c.business_id=$1 AND EXISTS (SELECT 1 FROM pg_namespace n WHERE n.nspname='ledger_' || o.tenant_id) ORDER BY o.organization_id")
            .bind(principal.business_id).fetch_all(self.pool()).await?;
        let scope = CustomerRecordScope {
            key_ids: None,
            include_unattributed: true,
        };
        let mut found = None;
        for row in rows {
            let id = row.try_get("organization_id")?;
            let tenant: String = row.try_get("tenant_id")?;
            let candidate: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oxsum.request_operations o LEFT JOIN oxsum.request_attempts a USING (operation_id) WHERE o.organization_id=$1 AND (o.operation_id=$2 OR a.external_id=$3))")
                .bind(id).bind(Uuid::parse_str(identifier).ok()).bind(identifier).fetch_one(self.pool()).await?;
            let mut legacy_candidate = false;
            if !candidate && let Ok(entry) = Uuid::parse_str(identifier) {
                super::validate_tenant_id(&tenant)?;
                let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                    .bind(format!("ledger_{tenant}.entries"))
                    .fetch_one(self.pool())
                    .await?;
                if exists {
                    let hold = format!("req-{entry}:hold");
                    let ids = vec![
                        entry,
                        *wallet::entry_id_for(&hold).as_uuid(),
                        *wallet::entry_id_for(&wallet::settlement_key_for(&hold)).as_uuid(),
                    ];
                    legacy_candidate = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM ledger_{tenant}.entries WHERE entry_id=ANY($1))"))
                        .bind(ids).fetch_one(self.pool()).await?;
                }
            }
            if !candidate && !legacy_candidate {
                continue;
            }
            let wallet = Wallet::open_existing(self.pool().clone(), &tenant).await?;
            if let Some(receipt) = wallet.charge_receipt(self, id, identifier, &scope).await? {
                if found.is_some() {
                    return Err(WalletError::Conflict("identifier matches several customers; select a customer or use an operation ID".into()));
                }
                found = Some(BusinessChargeReceipt {
                    organization_id: id,
                    customer_name: row.try_get("name")?,
                    receipt,
                });
            }
        }
        Ok(found)
    }
}
