//! Paid plan fixtures use the same quoted order and verified-event boundary as production.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{
    CollectionCurrency, Db, MerchantCreateRequest, MerchantEventKind, MerchantMode, MerchantRail,
    MerchantStatusRequest, MerchantVersionInput, NewPaymentOrder, PaymentOrder,
    PaymentOrderPurpose, SecretKey, VerifiedMerchantEvent,
};
use uuid::Uuid;

pub async fn merchant_context(db: &Db, owner: &str) -> oxsum_core::MerchantSigningContext {
    db.bootstrap_collection_terms(&CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let key = SecretKey::from_bytes([71; 32]);
    let merchant = db.create_merchant(owner, &MerchantCreateRequest {
        idempotency_key: Uuid::new_v4().to_string(), reason: "local plan payment fixture".into(),
        name: format!("plan-fixture-{}",Uuid::new_v4()), rail: MerchantRail::Hyperswitch,
        mode: MerchantMode::Test, account_id: Uuid::new_v4().to_string(),
        config: serde_json::from_value::<MerchantVersionInput>(serde_json::json!({
            "apiUrl":"https://sandbox.hyperswitch.io", "apiKey":"plan-fixture-api-secret",
            "webhookSecret":"plan-fixture-webhook-secret", "publishableKey":"pk_plan_fixture",
            "sdkUrl":"https://sdk.example"
        })).unwrap(),
    }, &key).await.unwrap();
    db.set_merchant_enabled(
        owner,
        merchant.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "enable fixture".into(),
            enabled: true,
        },
    )
    .await
    .unwrap();
    db.merchant_signing_context(merchant.merchant_id, merchant.version_id, &key)
        .await
        .unwrap()
        .unwrap()
}

/// Create one customer-owned quote-backed order without activating it.
pub async fn created_order(
    db: &Db,
    context: &oxsum_core::MerchantSigningContext,
    organization: Uuid,
    plan: Uuid,
) -> PaymentOrder {
    let quote = db.plan_payment_quote(organization).await.unwrap();
    assert_eq!(quote.terms.customer_plan_id, plan);
    db.create_payment_order(
        context,
        organization,
        &NewPaymentOrder {
            expected_collection_revision: Some(quote.terms.collection_revision),
            expected_plan_quote: Some(quote.fingerprint),
            purpose: PaymentOrderPurpose::PlanPeriod,
            display_amount_minor: quote.terms.display_amount_minor,
            display_currency: quote.terms.display_currency,
            purpose_ref: Some(plan),
            idempotency_key: Uuid::new_v4().to_string(),
            promo_minor: 0,
        },
    )
    .await
    .unwrap()
}

pub async fn mark_paid(
    db: &Db,
    context: &oxsum_core::MerchantSigningContext,
    order: &PaymentOrder,
) {
    let organization = order.organization_id;
    let receipt = db
        .receive_merchant_event(
            context,
            &VerifiedMerchantEvent {
                event_id: Uuid::new_v4().to_string(),
                kind: MerchantEventKind::Succeeded,
                payment_id: Some(order.order_id),
                provider_payment_id: Some(format!("pay_{}", order.order_id)),
                amount_minor: Some(order.display_amount_minor),
                currency: Some(order.display_currency.clone()),
                refund_id: None,
                organization_id: Some(organization),
                provider_account_id: Some(context.account_id.clone()),
                mode: Some(MerchantMode::Test),
            },
            &[71; 32],
        )
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();
}

pub async fn paid_order(db: &Db, owner: &str, organization: Uuid, plan: Uuid) -> PaymentOrder {
    let context = merchant_context(db, owner).await;
    let order = created_order(db, &context, organization, plan).await;
    mark_paid(db, &context, &order).await;
    db.payment_order(organization, order.order_id)
        .await
        .unwrap()
        .unwrap()
}

/// Controlled usage fixture leaves an overage debt after the exact base fee is paid.
pub async fn overage(db: &Db, organization: Uuid, plan: Uuid, amount: i64) {
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=period_start-interval '1 day' WHERE customer_plan_id=$1 AND state='active'")
        .bind(plan).execute(db.pool()).await.unwrap();
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(organization)
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query("INSERT INTO oxsum.usage_records(request_id,tenant_id,model,channel,price_version,kind,entry_id,input_tokens,output_tokens,charged_minor,freeze_minor,settled_at) VALUES($1,$2,'fixture','fixture',1,'usage',$3,1,1,$4,$4,now()-interval '120 seconds')")
        .bind(Uuid::new_v4().to_string()).bind(tenant).bind(Uuid::new_v4()).bind(amount).execute(db.pool()).await.unwrap();
}

/// Fulfill a paid fixture, including its durable receipt/outbox.
pub async fn fulfill(db: &Db, order: &PaymentOrder) {
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(order.organization_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant)
        .await
        .unwrap();
    db.fulfill_payment_order(
        order.order_id,
        &wallet,
        time::OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
}

/// Seed an old binary's partially paid journal before running today's rollover.
///
/// This deliberately reproduces historical duplicated overage for recovery,
/// refund and dunning tests. New production closes must never create it. The
/// real authorizing order has already prepaid its exact fee; all fixture ledger
/// writes use the engine's public transaction API and normal immutable guards.
pub async fn legacy_close(db: &Db, organization: Uuid, plan: Uuid, overage_minor: i64) -> Uuid {
    use doubleentry::storage::postgres::PostgresStore;
    use doubleentry::{
        AccountRegistry, Amount, Currency, Draft, Entry, EntryBatch, IdempotencyKey, LedgerId,
        LedgerPolicy, LedgerStore, SealContext,
    };
    use sqlx::Row;

    assert!(overage_minor > 0);
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=LEAST(period_start,period_end-interval '1 day') WHERE customer_plan_id=$1 AND state='active'")
        .bind(plan).execute(db.pool()).await.unwrap();
    let row = sqlx::query("SELECT p.plan_period_id,p.period_index,p.period_start,p.period_end,p.authorization_ref,s.price_minor,s.currency,s.business_id,o.tenant_id,c.net_terms_days FROM oxsum.plan_periods p JOIN oxsum.customer_plans c USING(customer_plan_id) JOIN oxsum.sold_plans s USING(plan_version_id) JOIN oxsum.organizations o ON o.organization_id=c.organization_id JOIN oxsum.payment_orders pay ON pay.order_id=p.authorization_ref WHERE c.customer_plan_id=$1 AND c.organization_id=$2 AND p.state='active' AND p.authorization_kind='payment_order' AND pay.status IN ('paid','fulfilled') AND pay.amount_minor=s.price_minor")
        .bind(plan).bind(organization).fetch_one(db.pool()).await.unwrap();
    let tenant: String = row.get("tenant_id");
    let period: Uuid = row.get("plan_period_id");
    let index: i32 = row.get("period_index");
    let start: time::OffsetDateTime = row.get("period_start");
    let end: time::OffsetDateTime = row.get("period_end");
    let order: Uuid = row.get("authorization_ref");
    let fee: i64 = row.get("price_minor");
    let currency: String = row.get("currency");
    let business: Uuid = row.get("business_id");
    let net_terms: i32 = row.get("net_terms_days");
    assert!(fee > 0);
    let total = fee.checked_add(overage_minor).unwrap();
    let journal = Uuid::new_v4();
    let statement = Uuid::new_v4();
    let store = PostgresStore::<6>::new(
        db.pool().clone(),
        LedgerId::new(format!("tenant-{tenant}")).unwrap(),
    )
    .in_schema(&format!("ledger_{tenant}"));
    let accounts = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
    let calendar = store.calendar().await.unwrap();
    let find = |path: &str| {
        accounts
            .records()
            .iter()
            .find(|r| r.account.path.to_string() == path)
            .unwrap_or_else(|| panic!("account {path} exists"))
            .id
    };
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("INSERT INTO oxsum.invoice_journals(journal_id,customer_plan_id,organization_id,business_id,plan_period_id,period_index,state,total_minor,currency) VALUES($1,$2,$3,$4,$5,$6,'draft',$7,$8)")
        .bind(journal).bind(plan).bind(organization).bind(business).bind(period).bind(index).bind(total).bind(&currency).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_statements(statement_id,customer_plan_id,organization_id,business_id,period_index,period_start,period_end,journal_id,state,total_minor,currency) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'open',$9,$10)")
        .bind(statement).bind(plan).bind(organization).bind(business).bind(index).bind(start).bind(end).bind(journal).bind(total).bind(&currency).execute(&mut *tx).await.unwrap();
    for (journal_kind, statement_kind, amount) in [
        ("base_fee", "plan_fee", fee),
        ("usage_overage", "usage", overage_minor),
    ] {
        sqlx::query("INSERT INTO oxsum.invoice_journal_lines(line_id,journal_id,kind,amount_minor,description,source_ref) VALUES($1,$2,$3,$4,'Historical close fixture',$5)")
            .bind(Uuid::new_v4()).bind(journal).bind(journal_kind).bind(amount).bind(period).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,$3,$4,'Historical close fixture',$5)")
            .bind(Uuid::new_v4()).bind(statement).bind(statement_kind).bind(amount).bind(period).execute(&mut *tx).await.unwrap();
    }
    for (key, debit, credit, minor) in [
        (
            format!("plan-journal/{journal}/accrual"),
            "Assets:Receivables",
            "Income:PlanFees",
            total,
        ),
        (
            format!("plan-journal/{journal}/prepay-settle"),
            "Liabilities:UnearnedPlanFees",
            "Assets:Receivables",
            fee,
        ),
    ] {
        let amount = Amount::<6>::from_minor(minor);
        let sealed = Entry::<Draft, 6>::new(
            oxsum_core::entry_id_for(&key),
            IdempotencyKey::new(key.as_bytes().to_vec()).unwrap(),
            time::OffsetDateTime::now_utc().date(),
        )
        .debit(find(debit), amount, Currency::USD)
        .credit(find(credit), amount, Currency::USD)
        .seal(&SealContext {
            accounts: &accounts,
            calendar: &calendar,
            policy: &LedgerPolicy::default(),
        })
        .unwrap();
        store
            .append_in_tx(&mut tx, &EntryBatch::single(sealed))
            .await
            .unwrap();
    }
    sqlx::query("UPDATE oxsum.invoice_journals SET state='posted',settled_minor=$2,posted_at=now() WHERE journal_id=$1")
        .bind(journal).bind(fee).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'payment_applied',$3,'Historical prepayment fixture',$4)")
        .bind(Uuid::new_v4()).bind(statement).bind(-fee).bind(order).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_statements SET state='issued',paid_minor=$2,issued_at=now(),due_at=now()+($3 * interval '1 day') WHERE statement_id=$1")
        .bind(statement).bind(fee).bind(net_terms).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    journal
}
