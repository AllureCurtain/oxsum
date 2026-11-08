//! One transaction owns dispute authority, source effects and the selected outcome.

use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use super::{DisputeResolution, OrderDispute, dispute_from_row};
use crate::{BusinessCapability, Db, Wallet, WalletError, business_staff as authority, error};

const SCOPED_DISPUTE: &str = "SELECT d.*,p.purpose,p.purpose_ref,\
    p.organization_id AS order_organization,p.merchant_id AS order_merchant,\
    p.amount_minor AS order_amount,p.display_amount_minor,p.display_currency,o.tenant_id \
    FROM oxsum.disputes d JOIN oxsum.merchant_accounts m USING(merchant_id) \
    JOIN oxsum.payment_orders p ON p.order_id=d.order_id \
    JOIN oxsum.organizations o ON o.organization_id=d.organization_id \
    WHERE d.dispute_id=$1 AND m.business_id=$2";

fn review() -> WalletError {
    WalletError::Conflict("dispute ledger or audit history requires explicit source review".into())
}

#[derive(Default)]
struct Allocation {
    entry: Option<Uuid>,
    recharged: Option<i64>,
    payable: i64,
    unallocated: i64,
    wallet: i64,
}

/// Read exact sequenced postings, including old effects orphaned before the
/// dispute's terminal commit. No pooled read can overlook this transaction.
async fn retained_effect(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    dispute: &OrderDispute,
    plan: bool,
) -> Result<Allocation, WalletError> {
    let key = format!("dispute-recharge/{}", dispute.dispute_id);
    let id = *crate::entry_id_for(&key).as_uuid();
    let tenant = wallet.tenant_id();
    let rows = sqlx::query(&format!(
        "SELECT entry_id,idempotency_key,reverses,log_index FROM ledger_{tenant}.entries \
         WHERE entry_id=$1 OR idempotency_key=$2"
    ))
    .bind(id)
    .bind(key.as_bytes())
    .fetch_all(&mut **tx)
    .await?;
    if rows.is_empty() {
        return Ok(Allocation::default());
    }
    if rows.len() != 1
        || rows[0].try_get::<Uuid, _>("entry_id")? != id
        || rows[0].try_get::<Vec<u8>, _>("idempotency_key")? != key.as_bytes()
        || rows[0].try_get::<Option<Uuid>, _>("reverses")?.is_some()
        || rows[0].try_get::<Option<i64>, _>("log_index")?.is_none()
    {
        return Err(review());
    }
    if !plan {
        let reversed: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM ledger_{tenant}.entries WHERE reverses=$1)"
        ))
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
        if reversed || wallet.verified_source_hash(id).await?.is_none() {
            return Err(review());
        }
    }
    let postings = sqlx::query(&format!(
        "SELECT a.path,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p \
         JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1"
    ))
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;
    let mut cash = 0i64;
    let mut debt = 0i64;
    let mut payable = 0i64;
    let mut suspense = 0i64;
    let mut wallet_minor = 0i64;
    for row in postings {
        let path: String = row.try_get("path")?;
        let direction: String = row.try_get("direction")?;
        let minor: i64 = row.try_get("amount_minor")?;
        if minor <= 0
            || row.try_get::<String, _>("currency")? != "USD"
            || row.try_get::<String, _>("layer")? != "settled"
        {
            return Err(review());
        }
        let target = match (plan, path.as_str(), direction.as_str()) {
            (_, "Assets:Cash", "C") => &mut cash,
            (true, "Assets:Receivables", "D") => &mut debt,
            (false, "Liabilities:Wallet", "D") => &mut wallet_minor,
            (true, "Liabilities:RefundPayable", "D") => &mut payable,
            (_, "Equity:Adjustments", "D") => &mut suspense,
            _ => return Err(review()),
        };
        *target = target.checked_add(minor).ok_or_else(review)?;
    }
    if cash <= 0
        || cash != dispute.amount_minor
        || debt
            .checked_add(payable)
            .and_then(|v| v.checked_add(suspense))
            .and_then(|v| v.checked_add(wallet_minor))
            != Some(cash)
    {
        return Err(review());
    }
    Ok(Allocation {
        entry: Some(id),
        recharged: Some(debt),
        payable,
        unallocated: suspense,
        wallet: wallet_minor,
    })
}

async fn verify_history(
    tx: &mut Transaction<'_, Postgres>,
    row: &PgRow,
    business: Uuid,
    dispute: &OrderDispute,
    allocation: &Allocation,
) -> Result<(), WalletError> {
    let lost = dispute.state == "lost";
    if dispute.recharge_entry_id != allocation.entry || lost != allocation.entry.is_some() {
        return Err(review());
    }
    let audit_id: Option<Uuid> = row.try_get("audit_ref")?;
    if dispute.state == "opened" {
        return if audit_id.is_none() {
            Ok(())
        } else {
            Err(review())
        };
    }
    let audit = sqlx::query("SELECT target,actor_user_id,detail FROM oxsum.finance_audit WHERE audit_id=$1 AND business_id=$2 AND action='dispute.resolve'")
        .bind(audit_id).bind(business).fetch_optional(&mut **tx).await?.ok_or_else(review)?;
    let detail: serde_json::Value = audit.try_get("detail")?;
    if audit.try_get::<String, _>("target")? != dispute.dispute_id.to_string()
        || Some(audit.try_get::<Uuid, _>("actor_user_id")?)
            != row.try_get::<Option<Uuid>, _>("resolved_by")?
        || detail["orderId"] != json!(dispute.order_id)
        || detail["resolution"] != dispute.state
        || detail["contestedMinor"] != dispute.amount_minor
        || detail["currency"] != dispute.currency
        || detail["rechargeEntryId"] != json!(allocation.entry)
        || detail["rechargedMinor"] != json!(allocation.recharged)
        || (lost
            && detail.get("payableMinor").is_some()
            && detail["payableMinor"] != allocation.payable)
        || (lost
            && detail.get("unallocatedMinor").is_some()
            && detail["unallocatedMinor"] != allocation.unallocated)
        || (lost
            && row.try_get::<String, _>("purpose")? != "plan_period"
            && (detail["walletTakenMinor"] != allocation.wallet
                || detail["source"]["policy"] != "nonplan-cash-v1"))
    {
        return Err(review());
    }
    Ok(())
}

impl Db {
    /// Resolve a dispute with live business authority, exact wallet ownership,
    /// ledger/documents and audit in one transaction. Matching request keys
    /// replay; changed terms and inconsistent retained effects are conflicts.
    ///
    /// Non-plan losses credit complete Cash and reclaim only proven original
    /// Wallet funding; other amounts remain explicit review suspense. Historical
    /// wrong-direction or interrupted postings require separate source recovery.
    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_order_dispute(
        &self,
        wallet: &Wallet,
        token: &str,
        dispute_id: Uuid,
        resolution: DisputeResolution,
        reason: Option<&str>,
        idempotency_key: &str,
        on: Date,
    ) -> Result<OrderDispute, WalletError> {
        authority::validate(idempotency_key, reason)?;
        let reason = reason.map(str::trim);
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::RefundsWrite,
        )
        .await?;
        let scope = sqlx::query(SCOPED_DISPUTE)
            .bind(dispute_id)
            .bind(business)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| WalletError::NotFound("unknown dispute".into()))?;
        let organization: Uuid = scope.try_get("organization_id")?;
        if scope.try_get::<String, _>("tenant_id")? != wallet.tenant_id()
            || scope.try_get::<Uuid, _>("order_organization")? != organization
            || scope.try_get::<Uuid, _>("order_merchant")?
                != scope.try_get::<Uuid, _>("merchant_id")?
        {
            return Err(WalletError::Conflict(
                "dispute wallet or order ownership differs".into(),
            ));
        }
        let plan = scope.try_get::<String, _>("purpose")? == "plan_period";
        // Immutable coordinates are read before locking. Plan ownership always
        // precedes dispute/journal/ledger/statement locks, even for won/closed.
        // Do not acquire the payment order after its owner: activation owns it first.
        if plan {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text,0))")
                .bind(organization.to_string()).execute(&mut *tx).await?;
            sqlx::query("SELECT subscription_id FROM oxsum.org_subscriptions WHERE organization_id=$1 AND state='active' FOR UPDATE")
                .bind(organization).fetch_optional(&mut *tx).await?;
            sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 AND organization_id=$2 FOR UPDATE")
                .bind(scope.try_get::<Option<Uuid>, _>("purpose_ref")?).bind(organization)
                .fetch_optional(&mut *tx).await?.ok_or_else(review)?;
        }
        if !plan {
            crate::nonplan_returns::lock_source_in(&mut tx, scope.try_get("order_id")?).await?;
        }
        let row = sqlx::query(&format!("{SCOPED_DISPUTE} FOR UPDATE OF d"))
            .bind(dispute_id)
            .bind(business)
            .fetch_one(&mut *tx)
            .await?;
        let dispute = dispute_from_row(&row)?;
        if !plan {
            let event_id: Option<Uuid> = row.try_get("inbox_receipt_id")?;
            let event_id = event_id.ok_or_else(review)?;
            let valid_receipt: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_event_inbox e JOIN oxsum.payment_orders p ON p.order_id=$2 WHERE e.receipt_id=$1 AND e.merchant_id=p.merchant_id AND e.version_id=p.merchant_version_id AND e.state IN ('accepted','applied'))")
                .bind(event_id).bind(dispute.order_id).fetch_one(&mut *tx).await?;
            if !valid_receipt {
                return Err(review());
            }
            let event = self
                .merchant_event_data(event_id)
                .await?
                .ok_or_else(review)?;
            let amount = crate::nonplan_returns::disputed_minor(
                row.try_get("order_amount")?,
                row.try_get("display_amount_minor")?,
                &row.try_get::<String, _>("display_currency")?,
                &event,
            )
            .map_err(|_| review())?;
            if event.kind != crate::MerchantEventKind::Dispute
                || amount != dispute.amount_minor
                || event.currency.as_deref() != Some(dispute.currency.as_str())
                || event.payment_id != Some(dispute.order_id)
                || event.organization_id != Some(organization)
                || event.refund_id.as_deref() != Some(dispute.provider_dispute_id.as_str())
            {
                return Err(review());
            }
            wallet.lock_append_in(&mut tx).await?;
        }
        let retained = retained_effect(&mut tx, wallet, &dispute, plan).await?;
        verify_history(&mut tx, &row, business, &dispute, &retained).await?;
        if let Some(audit) = sqlx::query("SELECT target,actor_user_id,detail FROM oxsum.finance_audit WHERE business_id=$1 AND action='dispute.resolve' AND idempotency_key=$2")
            .bind(business).bind(idempotency_key).fetch_optional(&mut *tx).await? {
            let detail: serde_json::Value = audit.try_get("detail")?;
            if audit.try_get::<String, _>("target")? != dispute_id.to_string()
                || audit.try_get::<Uuid, _>("actor_user_id")? != actor.user_id
                || detail["resolution"] != resolution.as_str()
                || detail.get("reason").and_then(serde_json::Value::as_str).map(str::trim) != reason {
                return Err(WalletError::Conflict("dispute key names different request terms".into()));
            }
            tx.commit().await?;
            return Ok(dispute);
        }
        if matches!(dispute.state.as_str(), "won" | "lost" | "closed") {
            if dispute.state != resolution.as_str() {
                return Err(WalletError::Conflict(
                    "dispute already has a different terminal outcome".into(),
                ));
            }
            tx.commit().await?;
            return Ok(dispute);
        }
        let mut allocation = Allocation::default();
        let mut corrected_journals = Vec::new();
        let mut source = None;
        let mut cancelled_promo_lot = None;
        if resolution == DisputeResolution::Lost {
            if plan {
                self.recharge_journal_dispute_in(
                    &mut tx,
                    wallet,
                    dispute.order_id,
                    dispute_id,
                    dispute.amount_minor,
                    on,
                )
                .await?
                .ok_or_else(|| {
                    error::invalid("a lost dispute has no journal receivable to restore")
                })?;
                allocation = retained_effect(&mut tx, wallet, &dispute, true).await?;
                corrected_journals = crate::plan_usage_corrections::corrected_order_journals_in(
                    &mut tx,
                    dispute.order_id,
                )
                .await?;
            } else {
                let evidence = crate::nonplan_returns::inspect_in(
                    &mut tx,
                    wallet,
                    dispute.order_id,
                    organization,
                    dispute_id,
                    dispute.amount_minor,
                )
                .await?;
                let receipt = wallet
                    .nonplan_dispute_in(
                        &mut tx,
                        dispute_id,
                        dispute.amount_minor,
                        evidence.wallet_minor,
                        on,
                    )
                    .await?;
                allocation.entry = Some(*receipt.entry_id.as_uuid());
                allocation.recharged = Some(0);
                allocation.wallet = evidence.wallet_minor;
                allocation.unallocated = evidence.suspense_minor;
                source = Some(evidence);
                if row.try_get::<String, _>("purpose")? == "bundle" {
                    let lot: Option<Uuid> = sqlx::query_scalar("SELECT lot_id FROM oxsum.promo_lots WHERE organization_id=$1 AND source='bundle_promo' AND source_ref=$2")
                        .bind(organization).bind(dispute.order_id.to_string()).fetch_optional(&mut *tx).await?;
                    if let Some(lot) = lot {
                        self.cancel_promo_lot_in(&mut tx, wallet, lot, on).await?;
                        cancelled_promo_lot = Some(lot);
                    }
                }
            }
        }
        let audit_id = Uuid::new_v4();
        sqlx::query("UPDATE oxsum.disputes SET state=$2,recharge_entry_id=$3,resolved_by=$4,audit_ref=$5,updated_at=now() WHERE dispute_id=$1")
            .bind(dispute_id).bind(resolution.as_str()).bind(allocation.entry).bind(actor.user_id).bind(audit_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES($1,$2,$3,'dispute.resolve',$4,$5,$6)")
            .bind(audit_id).bind(business).bind(actor.user_id).bind(dispute_id.to_string())
            .bind(json!({
                "orderId":dispute.order_id,"resolution":resolution.as_str(),
                "contestedMinor":dispute.amount_minor,"rechargedMinor":allocation.recharged,
                "payableMinor":allocation.payable,"unallocatedMinor":allocation.unallocated,
                "walletTakenMinor":allocation.wallet,"source":source,"cancelledPromoLotId":cancelled_promo_lot,
                "requiresReview":allocation.unallocated>0,"correctedJournals":corrected_journals,
                "reviewReason":if corrected_journals.is_empty(){None}else{Some("historical usage correction requires explicit payable allocation")},
                "currency":dispute.currency,"rechargeEntryId":allocation.entry,"reason":reason,
            })).bind(idempotency_key).execute(&mut *tx).await?;
        let resolved = sqlx::query(SCOPED_DISPUTE)
            .bind(dispute_id)
            .bind(business)
            .fetch_one(&mut *tx)
            .await?;
        let result = dispute_from_row(&resolved)?;
        tx.commit().await?;
        Ok(result)
    }
}
