//! Validate retained source legs before proposing any historical correction.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::PlanUsageCorrection;
use crate::{Wallet, WalletError, error};

#[derive(Serialize)]
struct Posting {
    path: String,
    direction: String,
    amount_minor: i64,
    currency: String,
    layer: String,
}

#[derive(Serialize)]
struct Entry {
    entry_id: Uuid,
    content_hash: String,
    description: String,
    #[serde(skip)]
    key: Vec<u8>,
    reverses: Option<Uuid>,
    postings: Vec<Posting>,
}

impl Entry {
    fn matches_pair(&self, debit: &str, credit: &str, amount: i64) -> bool {
        self.reverses.is_none()
            && self.postings.len() == 2
            && self.postings.iter().all(|p| {
                p.currency == "USD"
                    && p.layer == "settled"
                    && p.amount_minor == amount
                    && ((p.path == debit && p.direction == "D")
                        || (p.path == credit && p.direction == "C"))
            })
            && self.postings.iter().any(|p| p.direction == "D")
            && self.postings.iter().any(|p| p.direction == "C")
    }

    fn exact_key(&self, key: &str) -> bool {
        self.key == key.as_bytes() && self.entry_id == *crate::entry_id_for(key).as_uuid()
    }

    fn usage_amount(&self) -> Option<i64> {
        let mut debits = 0i64;
        let mut credits = 0i64;
        for p in self.postings.iter().filter(|p| p.layer == "settled") {
            if p.currency != "USD" || p.amount_minor <= 0 {
                return None;
            }
            match (p.path.as_str(), p.direction.as_str()) {
                ("Income:Usage", "C") => credits = credits.checked_add(p.amount_minor)?,
                (
                    "Liabilities:Wallet"
                    | "Equity:Bonus"
                    | "Equity:Promo"
                    | "Liabilities:CreditLine",
                    "D",
                ) => debits = debits.checked_add(p.amount_minor)?,
                _ => return None,
            }
        }
        (self.reverses.is_none() && debits == credits && credits > 0).then_some(credits)
    }
}

fn refuse(
    mut report: PlanUsageCorrection,
    reason: &str,
) -> Result<PlanUsageCorrection, WalletError> {
    report.status = "refused".into();
    report.refusal = Some(reason.into());
    Ok(report)
}

fn add(a: i64, b: i64) -> Result<i64, WalletError> {
    a.checked_add(b)
        .ok_or_else(|| error::invalid("historical source amounts overflow"))
}

pub(super) async fn inspect(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    journal: &PgRow,
) -> Result<PlanUsageCorrection, WalletError> {
    let id: Uuid = journal.try_get("journal_id")?;
    let period: Uuid = journal.try_get("plan_period_id")?;
    let plan: Uuid = journal.try_get("customer_plan_id")?;
    let organization: Uuid = journal.try_get("organization_id")?;
    let business: Uuid = journal.try_get("business_id")?;
    let total: i64 = journal.try_get("total_minor")?;
    let settled: i64 = journal.try_get("settled_minor")?;
    let accrual_key = format!("plan-journal/{id}/accrual");
    let correction = *crate::entry_id_for(&format!("plan-usage-correction/{id}")).as_uuid();
    let mut report = PlanUsageCorrection {
        journal_id: id,
        organization_id: organization,
        statement_id: None,
        correction_id: correction,
        status: "proposed".into(),
        refusal: None,
        duplicate_minor: 0,
        receivable_minor: 0,
        payable_minor: 0,
        before_total_minor: total,
        before_settled_minor: settled,
        after_total_minor: total,
        after_settled_minor: settled,
        original_entry_id: *crate::entry_id_for(&accrual_key).as_uuid(),
        entry_id: None,
        already_applied: false,
        evidence: json!({"planPeriodId":period,"customerPlanId":plan}),
        evidence_sha256: String::new(),
    };
    let state: String = journal.try_get("state")?;
    if !matches!(state.as_str(), "posted" | "paid") {
        return refuse(
            report,
            "only original posted or paid journals are supported; draft/void history needs manual review",
        );
    }
    if journal
        .try_get::<Option<Uuid>, _>("replaces_journal_id")?
        .is_some()
        || journal.try_get::<i64, _>("disputed_minor")? != 0
    {
        return refuse(
            report,
            "reissued or disputed journals require combined source review",
        );
    }
    if journal.try_get::<Uuid, _>("period_plan")? != plan
        || journal.try_get::<String, _>("authorization_kind")? != "payment_order"
        || total <= 0
        || settled < 0
        || settled > total
        || (state == "paid" && settled != total)
    {
        return refuse(
            report,
            "journal period, authorization or payment counters are inconsistent",
        );
    }
    let sibling: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.invoice_journals WHERE plan_period_id=$1 AND journal_id<>$2)")
        .bind(period).bind(id).fetch_one(&mut **tx).await?;
    if sibling {
        return refuse(
            report,
            "multiple journals for the original period require combined source review",
        );
    }
    let original_order: Uuid = journal.try_get("authorization_ref")?;
    let original = order(tx, original_order, organization, plan, business).await?;
    let Some(order_amount) = original else {
        return refuse(
            report,
            "original period order is not a matching confirmed paid plan source",
        );
    };
    let lines = sqlx::query("SELECT line_id,kind,amount_minor,source_ref FROM oxsum.invoice_journal_lines WHERE journal_id=$1 ORDER BY created_at,line_id")
        .bind(id).fetch_all(&mut **tx).await?;
    let mut fee = 0;
    let mut duplicated = 0;
    let mut source_lines = Vec::new();
    if lines.len() != 2 {
        return refuse(
            report,
            "only an original base-fee plus positive usage-overage journal is supported; corrections, tax and promo need manual review",
        );
    }
    for line in &lines {
        let kind: String = line.try_get("kind")?;
        let amount: i64 = line.try_get("amount_minor")?;
        if amount <= 0 || line.try_get::<Option<Uuid>, _>("source_ref")? != Some(period) {
            return refuse(
                report,
                "original journal charge has an invalid amount or foreign period source",
            );
        }
        match kind.as_str() {
            "base_fee" if fee == 0 => fee = amount,
            "usage_overage" if duplicated == 0 => duplicated = amount,
            _ => {
                return refuse(
                    report,
                    "journal lines are not the uncorrected historical base-fee and usage pair",
                );
            }
        }
        source_lines.push(json!({"lineId":line.try_get::<Uuid,_>("line_id")?,"kind":kind,"amountMinor":amount,"sourceRef":period}));
    }
    if fee <= 0 || duplicated <= 0 || add(fee, duplicated)? != total {
        return refuse(
            report,
            "original journal total differs from its positive charge lines",
        );
    }
    report.duplicate_minor = duplicated;
    report.evidence["journalLines"] = json!(source_lines);
    let Some(accrual) = entry(tx, wallet, report.original_entry_id).await? else {
        return refuse(report, "original accrual entry is missing or not sequenced");
    };
    if !accrual.exact_key(&accrual_key)
        || !accrual.matches_pair("Assets:Receivables", "Income:PlanFees", total)
    {
        return refuse(
            report,
            "original accrual does not prove the journal's exact receivable and plan revenue",
        );
    }
    report.evidence["accrual"] = json!(accrual);
    let mut source_ids = vec![report.original_entry_id];
    let start: OffsetDateTime = journal.try_get("period_start")?;
    let end: OffsetDateTime = journal.try_get("period_end")?;
    report.evidence["periodStart"] = json!(start.unix_timestamp_nanos().to_string());
    report.evidence["periodEnd"] = json!(end.unix_timestamp_nanos().to_string());
    let usage = sqlx::query("SELECT request_id,entry_id,charged_minor FROM oxsum.usage_records WHERE tenant_id=$1 AND settled_at >= $2 AND settled_at < $3 ORDER BY request_id")
        .bind(wallet.tenant_id()).bind(start).bind(end).fetch_all(&mut **tx).await?;
    let mut usage_total = 0;
    let mut usage_ids = HashSet::new();
    let mut usage_evidence = Vec::new();
    for row in usage {
        let amount: i64 = row.try_get("charged_minor")?;
        if amount == 0 {
            continue;
        }
        let usage_id: Uuid = row.try_get("entry_id")?;
        if amount < 0 || !usage_ids.insert(usage_id) {
            return refuse(
                report,
                "usage rows contain negative charges or repeat one settlement entry",
            );
        }
        let Some(fact) = entry(tx, wallet, usage_id).await? else {
            return refuse(
                report,
                "usage row has no original sequenced settlement in this customer's ledger",
            );
        };
        let request_id: String = row.try_get("request_id")?;
        let record = crate::SettlementRecord::parse(&fact.description);
        if fact.usage_amount() != Some(amount)
            || !record
                .is_some_and(|record| record.request == request_id && record.charged == amount)
        {
            return refuse(
                report,
                "usage row identity or amount differs from its original settled description and Income:Usage legs",
            );
        }
        usage_total = add(usage_total, amount)?;
        source_ids.push(usage_id);
        usage_evidence.push(json!({"requestId":request_id,"chargedMinor":amount,"entry":fact}));
    }
    report.evidence["usage"] = json!(usage_evidence);
    if usage_total != duplicated {
        return refuse(
            report,
            "period's proven original usage does not equal the duplicated usage charge",
        );
    }
    let mut payment_total = 0;
    let mut payment_by_order = BTreeMap::<Uuid, i64>::new();
    let mut payments = Vec::new();
    let prepay_key = format!("plan-journal/{id}/prepay-settle");
    if let Some(prepay) = entry(tx, wallet, *crate::entry_id_for(&prepay_key).as_uuid()).await? {
        let amount = prepay.postings.first().map_or(0, |p| p.amount_minor);
        let cash_key = format!("payment-order/{original_order}/prepay");
        let cash = entry(tx, wallet, *crate::entry_id_for(&cash_key).as_uuid()).await?;
        if amount <= 0
            || amount > order_amount
            || !prepay.exact_key(&prepay_key)
            || !prepay.matches_pair("Liabilities:UnearnedPlanFees", "Assets:Receivables", amount)
            || !cash.as_ref().is_some_and(|fact| {
                fact.exact_key(&cash_key)
                    && fact.matches_pair(
                        "Assets:Cash",
                        "Liabilities:UnearnedPlanFees",
                        order_amount,
                    )
            })
        {
            return refuse(
                report,
                "prepayment allocation is not backed by the exact original order cash; shared or partial legacy prepayment requires manual review",
            );
        }
        source_ids.push(prepay.entry_id);
        source_ids.push(*crate::entry_id_for(&cash_key).as_uuid());
        payment_total = amount;
        payment_by_order.insert(original_order, amount);
        payments.push(json!({"orderId":original_order,"amountMinor":amount,"allocation":prepay,"originalCash":cash}));
    }
    // Legacy pre-0109 repayment entries name the exact journal, even when the
    // order also paid another period. Their own legs are the only allocation.
    let suffix = format!("/journal/{id}");
    let sql = format!(
        "SELECT entry_id FROM ledger_{}.entries WHERE substring(idempotency_key FROM 1 FOR 14)=convert_to('payment-order/','UTF8') AND substring(idempotency_key FROM octet_length(idempotency_key)-$1+1)=convert_to($2,'UTF8') AND log_index IS NOT NULL",
        wallet.tenant_id()
    );
    let payment_ids: Vec<Uuid> = sqlx::query_scalar(&sql)
        .bind(suffix.len() as i32)
        .bind(&suffix)
        .fetch_all(&mut **tx)
        .await?;
    for payment_id in payment_ids {
        let fact = entry(tx, wallet, payment_id)
            .await?
            .ok_or_else(|| error::invalid("payment entry disappeared"))?;
        let source_order = std::str::from_utf8(&fact.key)
            .ok()
            .and_then(|key| key.strip_prefix("payment-order/"))
            .and_then(|key| key.strip_suffix(&suffix))
            .and_then(|id| id.parse::<Uuid>().ok());
        let Some(source_order) = source_order else {
            return refuse(
                report,
                "historical payment key does not contain a valid original order identity",
            );
        };
        let Some(maximum) = order(tx, source_order, organization, plan, business).await? else {
            return refuse(
                report,
                "historical journal payment names an unconfirmed or foreign customer/plan order",
            );
        };
        let amount = fact.postings.first().map_or(0, |p| p.amount_minor);
        if amount <= 0
            || amount > maximum
            || !fact.exact_key(&format!("payment-order/{source_order}/journal/{id}"))
            || !fact.matches_pair("Assets:Cash", "Assets:Receivables", amount)
        {
            return refuse(
                report,
                "historical journal payment does not prove its exact cash-to-receivable allocation",
            );
        }
        let by_order = add(
            payment_by_order.get(&source_order).copied().unwrap_or(0),
            amount,
        )?;
        if by_order > maximum {
            return refuse(
                report,
                "one original order's allocations exceed its confirmed amount",
            );
        }
        payment_by_order.insert(source_order, by_order);
        payment_total = add(payment_total, amount)?;
        source_ids.push(payment_id);
        payments.push(json!({"orderId":source_order,"amountMinor":amount,"allocation":fact}));
    }
    report.evidence["payments"] = json!(payments);
    if payment_total != settled {
        return refuse(
            report,
            "journal settled counter differs from proven original payment allocations",
        );
    }
    let mut orders: Vec<Uuid> = payment_by_order.keys().copied().collect();
    if !orders.contains(&original_order) {
        orders.push(original_order);
    }
    // Verify all original cash allocations of these selected funding orders.
    // Historical payments could split across journals and deferred prepayment;
    // their combined cash cannot exceed or fall short of the confirmed source.
    let mut cash_sources = Vec::new();
    for source_order in &orders {
        let maximum = order(tx, *source_order, organization, plan, business)
            .await?
            .unwrap_or(0);
        let Some(facts) = order_cash_sources(tx, wallet, *source_order, maximum).await? else {
            return refuse(
                report,
                "original order's complete retained cash allocations do not match its confirmed amount",
            );
        };
        source_ids.extend(facts.iter().map(|e| e.entry_id));
        cash_sources.push(json!({"orderId":source_order,"confirmedMinor":maximum,"entries":facts}));
    }
    report.evidence["orderCashSources"] = json!(cash_sources);
    let corrected:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.refunds WHERE order_id=ANY($1)) OR EXISTS(SELECT 1 FROM oxsum.disputes WHERE order_id=ANY($1)) OR EXISTS(SELECT 1 FROM oxsum.credit_notes WHERE order_id=ANY($1))")
        .bind(&orders).fetch_one(&mut **tx).await?;
    if corrected {
        return refuse(
            report,
            "an original funding order has refund, credit-note or dispute history; combined source review is required",
        );
    }
    let reversed:bool = sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ledger_{}.entries WHERE reverses=ANY($1) AND log_index IS NOT NULL)",wallet.tenant_id()))
        .bind(&source_ids).fetch_one(&mut **tx).await?;
    if reversed {
        return refuse(
            report,
            "an original accrual, payment or usage entry has a retained reversal",
        );
    }
    let statements = sqlx::query(
        "SELECT * FROM oxsum.plan_statements WHERE journal_id=$1 ORDER BY created_at FOR UPDATE",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;
    if statements.len() != 1 {
        return refuse(
            report,
            "exactly one original statement is required; missing/reissued/chargeback documents need review",
        );
    }
    let statement = &statements[0];
    let statement_id: Uuid = statement.try_get("statement_id")?;
    report.statement_id = Some(statement_id);
    let statement_state: String = statement.try_get("state")?;
    if !matches!(statement_state.as_str(), "issued" | "past_due" | "paid")
        || statement.try_get::<Option<Uuid>, _>("customer_plan_id")? != Some(plan)
        || statement.try_get::<Uuid, _>("organization_id")? != organization
        || statement.try_get::<Uuid, _>("business_id")? != business
        || statement.try_get::<Option<i32>, _>("period_index")?
            != Some(journal.try_get("period_index")?)
        || statement.try_get::<Option<OffsetDateTime>, _>("period_start")? != Some(start)
        || statement.try_get::<Option<OffsetDateTime>, _>("period_end")? != Some(end)
        || statement.try_get::<String, _>("currency")?
            != journal.try_get::<String, _>("currency")?
        || statement
            .try_get::<Option<Uuid>, _>("replaces_statement_id")?
            .is_some()
        || statement.try_get::<i64, _>("total_minor")? != total
        || statement.try_get::<i64, _>("paid_minor")? != settled
        || (statement_state == "paid" && total != settled)
    {
        return refuse(
            report,
            "original statement identity, state or counters differ from the proven journal",
        );
    }
    let statement_lines = sqlx::query("SELECT line_id,kind,amount_minor,source_ref FROM oxsum.plan_statement_lines WHERE statement_id=$1 ORDER BY created_at,line_id")
        .bind(statement_id).fetch_all(&mut **tx).await?;
    let mut statement_fee = 0;
    let mut statement_usage = 0;
    let mut statement_payments = BTreeMap::<Uuid, i64>::new();
    let mut retained_lines = Vec::new();
    for line in statement_lines {
        let kind: String = line.try_get("kind")?;
        let amount: i64 = line.try_get("amount_minor")?;
        let source: Option<Uuid> = line.try_get("source_ref")?;
        match (kind.as_str(), source) {
            ("plan_fee", Some(source)) if source == period && amount > 0 && statement_fee == 0 => {
                statement_fee = amount
            }
            ("usage", Some(source)) if source == period && amount > 0 && statement_usage == 0 => {
                statement_usage = amount
            }
            ("payment_applied", Some(source))
                if amount < 0 && payment_by_order.contains_key(&source) =>
            {
                let paid = amount
                    .checked_neg()
                    .ok_or_else(|| error::invalid("statement payment overflows"))?;
                statement_payments.insert(
                    source,
                    add(statement_payments.get(&source).copied().unwrap_or(0), paid)?,
                );
            }
            _ => {
                return refuse(
                    report,
                    "statement has unsupported or source-mismatched charge/payment lines",
                );
            }
        }
        retained_lines.push(json!({"lineId":line.try_get::<Uuid,_>("line_id")?,"kind":kind,"amountMinor":amount,"sourceRef":source}));
    }
    report.evidence["statementLines"] = json!(retained_lines);
    if statement_fee != fee
        || statement_usage != duplicated
        || statement_payments != payment_by_order
    {
        return refuse(
            report,
            "statement charge/payment lines differ from the original ledger allocations",
        );
    }
    report.receivable_minor = duplicated.min(total - settled);
    report.payable_minor = duplicated - report.receivable_minor;
    report.after_total_minor = total - duplicated;
    report.after_settled_minor = settled - report.payable_minor;
    Ok(report)
}

async fn order(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    organization: Uuid,
    plan: Uuid,
    business: Uuid,
) -> Result<Option<i64>, WalletError> {
    Ok(sqlx::query_scalar("SELECT o.amount_minor FROM oxsum.payment_orders o JOIN oxsum.merchant_accounts m USING(merchant_id) WHERE o.order_id=$1 AND o.organization_id=$2 AND o.purpose='plan_period' AND o.purpose_ref=$3 AND m.business_id=$4 AND o.status IN ('paid','fulfilled') AND o.paid_at IS NOT NULL AND o.promo_minor=0 AND o.currency='USD'")
        .bind(id).bind(organization).bind(plan).bind(business).fetch_optional(&mut **tx).await?)
}

async fn order_cash_sources(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    order: Uuid,
    maximum: i64,
) -> Result<Option<Vec<Entry>>, WalletError> {
    let prefix = format!("payment-order/{order}/");
    let ids:Vec<Uuid>=sqlx::query_scalar(&format!("SELECT entry_id FROM ledger_{}.entries WHERE substring(idempotency_key FROM 1 FOR $1)=convert_to($2,'UTF8') AND log_index IS NOT NULL ORDER BY entry_id",wallet.tenant_id()))
        .bind(prefix.len() as i32).bind(&prefix).fetch_all(&mut **tx).await?;
    let mut facts = Vec::new();
    let mut cash = 0;
    for id in ids {
        let fact = entry(tx, wallet, id)
            .await?
            .ok_or_else(|| error::invalid("cash source disappeared"))?;
        let Some(key) = std::str::from_utf8(&fact.key).ok() else {
            return Ok(None);
        };
        let suffix = key.strip_prefix(&prefix).unwrap_or("");
        let credit = if suffix == "prepay" {
            "Liabilities:UnearnedPlanFees"
        } else if suffix
            .strip_prefix("journal/")
            .is_some_and(|id| id.parse::<Uuid>().is_ok())
        {
            "Assets:Receivables"
        } else {
            return Ok(None);
        };
        let amount = fact.postings.first().map_or(0, |p| p.amount_minor);
        if amount <= 0 || !fact.exact_key(key) || !fact.matches_pair("Assets:Cash", credit, amount)
        {
            return Ok(None);
        }
        cash = add(cash, amount)?;
        facts.push(fact);
    }
    Ok((cash == maximum && maximum > 0).then_some(facts))
}

async fn entry(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    id: Uuid,
) -> Result<Option<Entry>, WalletError> {
    let tenant = wallet.tenant_id();
    let row=sqlx::query(&format!("SELECT entry_id,encode(content_hash,'hex') AS content_hash,description,idempotency_key,reverses FROM ledger_{tenant}.entries WHERE entry_id=$1 AND log_index IS NOT NULL"))
        .bind(id).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let rows=sqlx::query(&format!("SELECT a.path,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1 ORDER BY posting_index"))
        .bind(id).fetch_all(&mut **tx).await?;
    let postings = rows
        .iter()
        .map(|p| {
            Ok(Posting {
                path: p.try_get("path")?,
                direction: p.try_get("direction")?,
                amount_minor: p.try_get("amount_minor")?,
                currency: p.try_get("currency")?,
                layer: p.try_get("layer")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Some(Entry {
        entry_id: id,
        content_hash: row.try_get("content_hash")?,
        description: row.try_get("description")?,
        key: row.try_get("idempotency_key")?,
        reverses: row.try_get("reverses")?,
        postings,
    }))
}
