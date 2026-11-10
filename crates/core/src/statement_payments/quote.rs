//! Read-only quote and transaction-owned reservation fences.

use std::collections::BTreeMap;

use doubleentry::LedgerId;
use doubleentry::storage::postgres::PostgresStore;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use super::{StatementPaymentQuote, StatementPaymentSlice, StatementPaymentTerms};
use crate::{CustomerCapability, Db, Principal, WalletError, error};

impl Db {
    /// Quote partial or full debt under the customer's current personal scope.
    /// No locks, provider action, ledger creation or reservation occurs.
    pub async fn statement_payment_quote(
        &self,
        principal: &Principal,
        statement: Uuid,
        amount: Option<i64>,
    ) -> Result<StatementPaymentQuote, WalletError> {
        principal.require_customer(CustomerCapability::Personal)?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let answer = quote_in(&mut tx, principal.organization().id, statement, amount).await?;
        tx.commit().await?;
        Ok(answer)
    }
}

pub(crate) async fn lock_owner_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    pool: &sqlx::PgPool,
) -> Result<(), WalletError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text,0))",
    )
    .bind(organization.to_string())
    .execute(&mut **tx)
    .await?;
    sqlx::query("SELECT subscription_id FROM oxsum.org_subscriptions WHERE organization_id=$1 AND state='active' FOR UPDATE")
        .bind(organization).fetch_optional(&mut **tx).await?;
    // A mixed statement can contain old and new plans. Take every owner in a
    // deterministic order before journals, documents, order or append ownership.
    sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE organization_id=$1 ORDER BY customer_plan_id FOR UPDATE")
        .bind(organization).fetch_all(&mut **tx).await?;
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(organization)
            .fetch_one(&mut **tx)
            .await?;
    crate::wallet::validate_tenant_id(&tenant)?;
    let ledger = LedgerId::new(format!("tenant-{tenant}")).map_err(error::invalid)?;
    // Use the engine's own lock identity; this creates no ledger or account.
    let store = PostgresStore::<{ crate::SCALE }>::new(pool.clone(), ledger);
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(store.append_lock())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(crate) async fn guard_journal_in(
    tx: &mut Transaction<'_, Postgres>,
    journal: Uuid,
) -> Result<(), WalletError> {
    let live: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.statement_payment_reservations r JOIN oxsum.payment_orders o USING(order_id) CROSS JOIN LATERAL jsonb_array_elements(o.statement_payment->'allocations') a WHERE r.state='reserved' AND (a->>'journalId')::uuid=$1)")
        .bind(journal).fetch_one(&mut **tx).await?;
    if live {
        return Err(WalletError::Conflict("journal has a live statement payment; resolve its provider intent before changing the debt".into()));
    }
    Ok(())
}

pub(crate) async fn quote_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    statement: Uuid,
    amount: Option<i64>,
) -> Result<StatementPaymentQuote, WalletError> {
    if amount.is_some_and(|n| !(1..=9_007_199_254_740_991).contains(&n)) {
        return Err(error::invalid(
            "statement amount is outside the exact supported range",
        ));
    }
    let row = sqlx::query("SELECT s.business_id,o.tenant_id FROM oxsum.plan_statements s JOIN oxsum.organizations o USING(organization_id) WHERE s.statement_id=$1 AND s.organization_id=$2 AND s.state<>'open'")
        .bind(statement).bind(organization).fetch_optional(&mut **tx).await?
        .ok_or_else(|| WalletError::NotFound("customer statement not found".into()))?;
    let business: Uuid = row.try_get("business_id")?;
    let tenant: String = row.try_get("tenant_id")?;
    let source = crate::financial_reconciliation::debt_sources::inspect_in(
        tx,
        organization,
        &tenant,
        statement,
    )
    .await?;
    let reservations = sqlx::query("SELECT o.*,r.amount_minor AS reserved_amount,r.organization_id AS reserved_organization FROM oxsum.statement_payment_reservations r JOIN oxsum.payment_orders o USING(order_id) WHERE r.statement_id=$1 AND r.state='reserved' ORDER BY r.order_id LIMIT 1001")
        .bind(statement).fetch_all(&mut **tx).await?;
    if reservations.len() > 1000 {
        return Err(error::invalid("statement reservation limit exceeded"));
    }
    let mut reserved = 0i64;
    let mut reserved_by_journal = BTreeMap::<Uuid, i64>::new();
    let mut pending = Vec::new();
    for row in &reservations {
        let order = crate::payment_orders::order(row)?;
        let terms = super::evidence::validate(&order)?;
        if terms.organization_id != organization
            || terms.statement_id != statement
            || terms.amount_minor != row.try_get::<i64, _>("reserved_amount")?
            || organization != row.try_get::<Uuid, _>("reserved_organization")?
        {
            return Err(WalletError::Conflict(
                "reservation source differs from order".into(),
            ));
        }
        reserved = reserved
            .checked_add(terms.amount_minor)
            .ok_or_else(|| error::invalid("reservation amount overflows"))?;
        pending.push(row.try_get("order_id")?);
        for slice in &terms.allocations {
            let value = reserved_by_journal.entry(slice.journal_id).or_default();
            *value = value
                .checked_add(slice.amount_minor)
                .ok_or_else(|| error::invalid("reservation slice overflows"))?;
        }
    }
    let available = source
        .outstanding_minor
        .checked_sub(reserved)
        .ok_or_else(|| error::invalid("available debt overflows"))?;
    let mut answer = StatementPaymentQuote {
        statement_id: statement,
        outstanding_minor: source.outstanding_minor,
        reserved_minor: reserved,
        available_minor: available.max(0),
        pending_order_ids: pending,
        reason: source.reason.map(str::to_owned),
        terms: None,
        fingerprint: None,
    };
    if !source.verified {
        return Ok(answer);
    }
    if available <= 0 {
        answer.reason = Some("noAvailableDebt".into());
        return Ok(answer);
    }
    let amount = amount.unwrap_or(available);
    if !(1..=9_007_199_254_740_991).contains(&amount) {
        return Err(error::invalid(
            "select a partial amount within the exact supported range",
        ));
    }
    if amount > available {
        return Err(error::invalid("statement amount exceeds unreserved debt"));
    }
    let mut left = amount;
    let mut allocations = Vec::new();
    for slice in source.slices {
        let available =
            slice.remaining_minor - reserved_by_journal.remove(&slice.journal_id).unwrap_or(0);
        if available < 0 {
            return Err(WalletError::Conflict(
                "reservation exceeds original journal debt".into(),
            ));
        }
        let take = available.min(left);
        if take > 0 {
            allocations.push(StatementPaymentSlice {
                journal_id: slice.journal_id,
                customer_plan_id: slice.customer_plan_id,
                amount_minor: take,
            });
            left -= take;
        }
    }
    if left != 0 || !reserved_by_journal.is_empty() {
        return Err(WalletError::Conflict(
            "statement reservation sources need review".into(),
        ));
    }
    let collection = crate::collection_terms::terms_in(tx, business).await?;
    let Some(currency) = collection.currency.filter(|_| collection.ready) else {
        answer.reason = Some("collectionNotConfigured".into());
        return Ok(answer);
    };
    let display = currency.display()?;
    let bytes=serde_json::to_vec(&serde_json::json!({"source":source.revision,"reservations":answer.pending_order_ids,"reservedMinor":reserved})).map_err(error::invalid)?;
    let revision = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let terms = StatementPaymentTerms {
        business_id: business,
        organization_id: organization,
        statement_id: statement,
        source_revision: revision,
        amount_minor: amount,
        allocations,
        collection_revision: collection.revision,
        display_currency: currency.code,
        display_amount_minor: crate::plan_payments::fiat_minor(amount, &display)?,
        rate_mantissa: display.rate_mantissa().to_string(),
        rate_scale: display.rate_scale(),
    };
    answer.fingerprint = Some(terms.fingerprint()?);
    answer.terms = Some(terms);
    Ok(answer)
}
