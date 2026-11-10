//! Canonical repayment entries and retained immutable source validation.

use doubleentry::{Direction, Layer};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use crate::{PaymentOrder, StatementPaymentTerms, Wallet, WalletError, error};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Canonically verified payment or return allocation, in integer credit minor units.
pub struct Effect {
    /// Immutable effect identity.
    pub id: Uuid,
    /// Original customer payment order.
    pub order: Uuid,
    /// Exact quoted customer statement.
    pub statement: Uuid,
    /// Original receivable journal, absent for excess cash suspense.
    pub journal: Option<Uuid>,
    /// Payment, refund or lost dispute.
    pub kind: String,
    /// Order, refund or dispute identity.
    pub source: Uuid,
    /// Original payment effect consumed by a return.
    pub original: Option<Uuid>,
    /// Full observed incoming or returned cash.
    pub cash: i64,
    /// Receivable paid or restored.
    pub receivable: i64,
    /// Original payable created or consumed.
    pub payable: i64,
    /// Unallocated return cash requiring review.
    pub suspense: i64,
    /// Official ledger entry identity.
    pub entry: Uuid,
    /// Canonical content hash.
    pub hash: String,
    /// Server booking date.
    #[serde(with = "booking_date")]
    pub on: Date,
}

pub(crate) fn key(kind: &str, source: Uuid, journal: Option<Uuid>) -> String {
    format!(
        "statement-{kind}/{source}/{}",
        journal.unwrap_or(Uuid::nil())
    )
}

pub(crate) fn validate(order: &PaymentOrder) -> Result<&StatementPaymentTerms, WalletError> {
    let t = order
        .statement_payment
        .as_ref()
        .ok_or_else(|| error::invalid("statement order has no immutable terms"))?;
    let mut sum = 0i64;
    let mut previous = None;
    for a in &t.allocations {
        if a.amount_minor <= 0 || previous.is_some_and(|p| p >= a.journal_id) {
            return Err(error::invalid(
                "statement allocations are not positive and uniquely sorted",
            ));
        }
        sum = sum
            .checked_add(a.amount_minor)
            .ok_or_else(|| error::invalid("statement allocation overflows"))?;
        previous = Some(a.journal_id);
    }
    let rate = t.rate_mantissa.parse::<i64>().map_err(error::invalid)?;
    if rate <= 0 || t.rate_scale > 8 {
        return Err(error::invalid("statement rate is invalid"));
    }
    let denominator = 10_i128.pow(6 + t.rate_scale);
    let fiat = i128::from(sum)
        .checked_mul(i128::from(rate))
        .and_then(|v| v.checked_mul(100))
        .and_then(|v| v.checked_add(denominator - 1))
        .ok_or_else(|| error::invalid("statement price overflows"))?
        / denominator;
    if order.purpose != crate::PaymentOrderPurpose::StatementRepayment
        || sum != t.amount_minor
        || sum != order.amount_minor
        || !(1..=9_007_199_254_740_991).contains(&sum)
        || order.purpose_ref != Some(t.statement_id)
        || order.organization_id != t.organization_id
        || t.display_amount_minor != order.display_amount_minor
        || t.display_currency != order.display_currency
        || t.rate_mantissa != order.rate_mantissa.to_string()
        || t.rate_scale != order.rate_scale
        || fiat != i128::from(t.display_amount_minor)
        || order.promo_minor != 0
        || order.currency != "USD"
    {
        return Err(WalletError::Conflict(
            "statement order differs from immutable exact terms".into(),
        ));
    }
    Ok(t)
}

/// Bind incoming cash to an exact retained authenticated success, not just status.
pub(crate) async fn payment_receipt_in(
    tx: &mut Transaction<'_, Postgres>,
    order: &PaymentOrder,
) -> Result<Uuid, WalletError> {
    let receipt: Option<Uuid> = sqlx::query_scalar(
        "SELECT e.receipt_id FROM oxsum.merchant_event_inbox e \
         LEFT JOIN oxsum.merchant_payment_refs r ON r.merchant_id=e.merchant_id \
            AND r.provider_payment_id=e.normalized->>'providerPaymentId' \
         LEFT JOIN oxsum.merchant_payment_bindings b ON b.payment_id=r.payment_id \
            AND b.merchant_id=e.merchant_id AND b.version_id=e.version_id \
         WHERE e.merchant_id=$2 AND e.version_id=$3 AND e.state IN ('accepted','applied') \
            AND e.normalized->>'kind'='succeeded' \
            AND COALESCE(e.normalized->>'paymentId',b.payment_id::text)=$1::uuid::text \
            AND COALESCE(e.normalized->>'organizationId',b.organization_id::text)=$4::uuid::text \
            AND e.normalized->>'amountMinor'=$5::text AND e.normalized->>'currency'=$6 \
         ORDER BY e.receipt_id LIMIT 1",
    )
    .bind(order.order_id)
    .bind(order.merchant_id)
    .bind(order.merchant_version_id)
    .bind(order.organization_id)
    .bind(order.display_amount_minor.to_string())
    .bind(&order.display_currency)
    .fetch_optional(&mut **tx)
    .await?;
    receipt.ok_or_else(|| {
        WalletError::Conflict("paid statement order has no exact verified merchant receipt".into())
    })
}

pub(crate) async fn effects_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    order: Uuid,
) -> Result<Vec<Effect>, WalletError> {
    let rows=sqlx::query("SELECT * FROM oxsum.statement_payment_effects WHERE order_id=$1 ORDER BY kind,source_ref,journal_id,effect_id LIMIT 1001").bind(order).fetch_all(&mut **tx).await?;
    if rows.len() > 1000 {
        return Err(error::invalid("statement effect limit exceeded"));
    }
    let original = crate::payment_orders::order_in(tx, order)
        .await?
        .ok_or_else(|| error::invalid("statement source order missing"))?;
    let terms = validate(&original)?;
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(original.organization_id)
            .fetch_one(&mut **tx)
            .await?;
    if tenant != wallet.tenant_id() {
        return Err(error::invalid("statement source wallet differs"));
    }
    let mut effects = Vec::new();
    for r in rows {
        let e = Effect {
            id: r.try_get("effect_id")?,
            order: r.try_get("order_id")?,
            statement: r.try_get("statement_id")?,
            journal: r.try_get("journal_id")?,
            kind: r.try_get("kind")?,
            source: r.try_get("source_ref")?,
            original: r.try_get("original_effect_id")?,
            cash: r.try_get("cash_minor")?,
            receivable: r.try_get("receivable_minor")?,
            payable: r.try_get("payable_minor")?,
            suspense: r.try_get("suspense_minor")?,
            entry: r.try_get("entry_id")?,
            hash: r.try_get("entry_hash")?,
            on: r.try_get("booked_on")?,
        };
        let retained: serde_json::Value = r.try_get("source")?;
        if e.order != order
            || e.statement != terms.statement_id
            || r.try_get::<Uuid, _>("organization_id")? != original.organization_id
            || retained["terms"] != serde_json::json!(terms)
            || retained["originalEffectId"] != serde_json::json!(e.original)
            || e.cash <= 0
            || e.receivable < 0
            || e.payable < 0
            || e.suspense < 0
            || e.receivable
                .checked_add(e.payable)
                .and_then(|v| v.checked_add(e.suspense))
                != Some(e.cash)
            || (e.kind == "payment"
                && (e.source != order
                    || e.original.is_some()
                    || e.suspense != 0
                    || !terms
                        .allocations
                        .iter()
                        .any(|a| Some(a.journal_id) == e.journal && a.amount_minor == e.cash)))
        {
            return Err(error::invalid("retained statement source metadata differs"));
        }
        let k = key(&e.kind, e.source, e.journal);
        if *crate::entry_id_for(&k).as_uuid() != e.entry || e.id != e.entry {
            return Err(error::invalid("statement effect identity differs"));
        }
        let entry = crate::ledger_source::entry_in(tx, wallet.tenant_id(), crate::entry_id_for(&k))
            .await?
            .ok_or_else(|| error::invalid("statement cash entry is missing"))?;
        let reversed: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM ledger_{}.entries WHERE reverses=$1)",
            wallet.tenant_id()
        ))
        .bind(e.entry)
        .fetch_one(&mut **tx)
        .await?;
        if entry.content_hash.to_hex() != e.hash
            || entry.index.is_none()
            || entry.entry.idempotency_key().as_bytes() != k.as_bytes()
            || entry.entry.reverses().is_some()
            || reversed
            || entry.entry.booking_date() != e.on
        {
            return Err(error::invalid("statement entry integrity differs"));
        }
        let inflow = e.kind == "payment";
        let expected = [
            (
                "Assets:Cash",
                if inflow {
                    Direction::Debit
                } else {
                    Direction::Credit
                },
                e.cash,
            ),
            (
                "Assets:Receivables",
                if inflow {
                    Direction::Credit
                } else {
                    Direction::Debit
                },
                e.receivable,
            ),
            (
                "Liabilities:RefundPayable",
                if inflow {
                    Direction::Credit
                } else {
                    Direction::Debit
                },
                e.payable,
            ),
            ("Equity:Adjustments", Direction::Debit, e.suspense),
        ];
        let nonzero = expected.iter().filter(|(_, _, amount)| *amount > 0).count();
        if entry.entry.postings().len() != nonzero {
            return Err(error::invalid("statement cash posting count differs"));
        }
        for (path, direction, amount) in expected {
            let account = wallet.source_account(path)?;
            if amount > 0
                && !entry.entry.postings().iter().any(|p| {
                    p.account == account
                        && p.direction == direction
                        && p.amount.to_minor() == amount
                        && p.layer == Layer::Settled
                        && p.currency == doubleentry::Currency::USD
                })
            {
                return Err(error::invalid("statement cash postings differ"));
            }
        }
        effects.push(e);
    }
    let mut returned = std::collections::BTreeMap::<Uuid, (i64, i64, i64)>::new();
    for effect in effects.iter().filter(|e| e.kind != "payment") {
        if let Some(id) = effect.original {
            let original = effects
                .iter()
                .find(|e| e.id == id && e.kind == "payment")
                .ok_or_else(|| error::invalid("statement return original payment is missing"))?;
            if effect.journal != original.journal {
                return Err(error::invalid(
                    "statement return journal differs from original",
                ));
            }
            let total = returned.entry(id).or_default();
            total.0 = total
                .0
                .checked_add(effect.cash)
                .ok_or_else(|| error::invalid("returned cash overflows"))?;
            total.1 = total
                .1
                .checked_add(effect.receivable)
                .ok_or_else(|| error::invalid("returned debt overflows"))?;
            total.2 = total
                .2
                .checked_add(effect.payable)
                .ok_or_else(|| error::invalid("returned payable overflows"))?;
            if total.0 > original.cash
                || total.1 > original.receivable
                || total.2 > original.cash - total.1
            {
                return Err(error::invalid(
                    "statement returns exceed their retained original source",
                ));
            }
        } else if effect.receivable != 0 || effect.payable != 0 || effect.suspense != effect.cash {
            return Err(error::invalid(
                "unallocated statement return must remain in suspense",
            ));
        }
    }
    Ok(effects)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    order: &PaymentOrder,
    kind: &str,
    source: Uuid,
    journal: Option<Uuid>,
    original: Option<Uuid>,
    receipt: Option<Uuid>,
    cash: i64,
    receivable: i64,
    payable: i64,
    suspense: i64,
    on: Date,
) -> Result<Effect, WalletError> {
    let terms = validate(order)?;
    let k = key(kind, source, journal);
    let entry = wallet
        .statement_cash_in(
            tx,
            &k,
            kind == "payment",
            cash,
            receivable,
            payable,
            suspense,
            on,
        )
        .await?;
    let id = *entry.entry_id.as_uuid();
    let hash = wallet
        .verified_source_hash_in(tx, id)
        .await?
        .ok_or_else(|| error::invalid("statement cash hash missing"))?;
    sqlx::query("INSERT INTO oxsum.statement_payment_effects(effect_id,order_id,organization_id,statement_id,journal_id,kind,source_ref,original_effect_id,event_receipt_id,cash_minor,receivable_minor,payable_minor,suspense_minor,entry_id,entry_hash,booked_on,source) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$1,$14,$15,$16)")
        .bind(id).bind(order.order_id).bind(order.organization_id).bind(terms.statement_id).bind(journal).bind(kind).bind(source).bind(original).bind(receipt).bind(cash).bind(receivable).bind(payable).bind(suspense).bind(&hash).bind(on).bind(serde_json::json!({"terms":terms,"originalEffectId":original})).execute(&mut **tx).await?;
    Ok(Effect {
        id,
        order: order.order_id,
        statement: terms.statement_id,
        journal,
        kind: kind.into(),
        source,
        original,
        cash,
        receivable,
        payable,
        suspense,
        entry: id,
        hash,
        on,
    })
}

impl crate::Db {
    /// Inspect one statement repayment's canonical cash effects under live ReportsRead.
    /// Foreign or non-statement orders are hidden before ledger access. No financial effects are created.
    pub async fn statement_payment_evidence(
        &self,
        token: &str,
        order: Uuid,
    ) -> Result<Vec<Effect>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = crate::business_staff::lock_business(&mut tx).await?;
        crate::business_staff::require_capability(
            &mut tx,
            business,
            token,
            crate::BusinessCapability::ReportsRead,
        )
        .await?;
        let tenant:Option<String>=sqlx::query_scalar("SELECT o.tenant_id FROM oxsum.payment_orders p JOIN oxsum.organizations o USING(organization_id) WHERE p.order_id=$1 AND p.business_id=$2 AND p.purpose='statement_repayment'").bind(order).bind(business).fetch_optional(&mut *tx).await?;
        let tenant =
            tenant.ok_or_else(|| WalletError::NotFound("statement payment not found".into()))?;
        // Load only existing account handles outside the effect transaction;
        // one-connection pools need no bootstrap or nested connection.
        tx.commit().await?;
        let wallet = Wallet::open_existing(self.pool().clone(), &tenant).await?;
        let mut tx = self.pool().begin().await?;
        crate::business_staff::require_capability(
            &mut tx,
            business,
            token,
            crate::BusinessCapability::ReportsRead,
        )
        .await?;
        let result = effects_in(&mut tx, &wallet, order).await?;
        tx.commit().await?;
        Ok(result)
    }
}

mod booking_date {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::Date;
    use time::format_description::well_known::Iso8601;

    pub(super) fn serialize<S: Serializer>(date: &Date, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&date.to_string())
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Date, D::Error> {
        let value = String::deserialize(deserializer)?;
        Date::parse(&value, &Iso8601::DEFAULT).map_err(serde::de::Error::custom)
    }
}
