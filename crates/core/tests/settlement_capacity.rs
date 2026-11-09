//! Complete settlement evidence and the bounded ledger description, on PostgreSQL.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use doubleentry::entry::MAX_DESCRIPTION_LEN;
use oxsum_core::{
    BillLine, Db, Settlement, SettlementCache, SettlementExperiment, SettlementGuardrail,
    SettlementKind, SettlementMandate, SettlementRecord, SettlementSale, SettlementSubscription,
    UsageRecord, Wallet,
};
use time::macros::date;
use uuid::Uuid;

const DAY: time::Date = date!(2026 - 11 - 09);

async fn wallet() -> Option<Wallet> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url("settlement_capacity")?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    Db::from_pool(pool.clone()).migrate().await.unwrap();
    Some(
        Wallet::open(pool, &Uuid::new_v4().simple().to_string())
            .await
            .unwrap(),
    )
}

#[tokio::test]
async fn complete_v14_and_v15_provenance_survives_storage_and_offline_verification() {
    let Some(wallet) = wallet().await else { return };
    let model = "m".repeat(200);
    let requested = "q".repeat(200);
    let upstream = "u".repeat(200);
    let channel = "c".repeat(40);
    let policy = "p".repeat(64);
    let reason = "an auditable quotation: \"quoted\" and \\ path 🧾 ".repeat(10);
    let hash = "ab".repeat(32);
    let subscription_id = Uuid::new_v4();
    let period_id = Uuid::new_v4();
    let mandate_id = Uuid::new_v4();
    let source = Uuid::new_v4();
    let experiment_id = Uuid::new_v4();
    let request_id = Uuid::new_v4().to_string();
    let usage = UsageRecord {
        input_tokens: 10,
        output_tokens: 2,
        cached_tokens: 10,
        service_tier: Some("t".repeat(64)),
        ..UsageRecord::default()
    };
    let lines = [
        BillLine {
            item: "input".into(),
            units: 0,
            price_per_m: 1_000_000,
        },
        BillLine {
            item: "cache_read".into(),
            units: 10,
            price_per_m: 100_000,
        },
        BillLine {
            item: "output".into(),
            units: 2,
            price_per_m: 1_000_000,
        },
    ];
    let description = Settlement {
        request: &request_id,
        channel: &channel,
        model: &model,
        requested_model: &requested,
        served_model: &model,
        upstream_model: Some(&upstream),
        price_version: 1,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged: 3,
        freeze: 12,
        upstream_attempts: 0,
        mandate: Some(SettlementMandate {
            id: &mandate_id,
            hash: &hash,
        }),
        subscription: Some(SettlementSubscription {
            subscription_id: &subscription_id,
            period_id: &period_id,
            drawn_minor: 12,
            charged_minor: 3,
        }),
        cache: Some(SettlementCache {
            source_entry_id: &source,
            similarity: 1.0,
        }),
        guardrail: vec![SettlementGuardrail {
            policy: &policy,
            verdict: "flagged",
            reason: &reason,
            phase: "post_call",
            streamed: false,
        }],
        reason: Some(&reason),
        experiment: Some(SettlementExperiment {
            id: &experiment_id,
            variant: &model,
        }),
        sale: Some(SettlementSale { digest: &hash }),
    }
    .description()
    .unwrap();
    assert!(description.chars().count() > 512);
    assert!(description.chars().count() < MAX_DESCRIPTION_LEN);
    for version in [14, 15] {
        let description = if version == 14 {
            // Retain the preceding writer's full-text wire shape as well as
            // testing the current writer's commitment and explicit phase.
            let mut legacy: serde_json::Value = serde_json::from_str(&description).unwrap();
            legacy["v"] = 14.into();
            legacy["reason"] = reason.clone().into();
            legacy.as_object_mut().unwrap().remove("reasonCommitment");
            let note = legacy["guardrail"][0].as_object_mut().unwrap();
            note.remove("phase");
            note.remove("reasonCommitment");
            note.insert("reason".into(), reason.clone().into());
            legacy.to_string()
        } else {
            description.clone()
        };
        let receipt = wallet
            .usage_receipt(
                &format!("composed/{version}"),
                &description,
                None,
                None,
                DAY,
            )
            .await
            .unwrap();
        let proof = wallet
            .receipt_proof(receipt.entry_id, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(proof.entry.description().as_str(), description);
        let record = SettlementRecord::parse(proof.entry.description().as_str()).unwrap();
        assert_eq!(record.requested_model, requested);
        assert_eq!(record.upstream_model.as_deref(), Some(upstream.as_str()));
        assert_eq!(record.subscription.unwrap().charged_minor, 3);
        assert_eq!(record.cache.unwrap().source_entry_id, source);
        assert_eq!(record.mandate.unwrap().hash, hash);
        if version == 14 {
            assert_eq!(record.guardrail[0].reason.as_deref(), Some(reason.as_str()));
            assert!(record.guardrail[0].reason_commitment.is_none());
            assert!(record.guardrail[0].phase.is_none());
            assert_eq!(record.reason.as_deref(), Some(reason.as_str()));
        } else {
            assert!(record.guardrail[0].reason.is_none());
            assert_eq!(record.guardrail[0].phase.as_deref(), Some("post_call"));
            assert!(oxsum_verify::verify_reason_commitment(
                record.guardrail[0].reason_commitment.as_ref().unwrap(),
                &reason,
            ));
            assert!(record.reason.is_none());
            assert!(oxsum_verify::verify_reason_commitment(
                record.reason_commitment.as_ref().unwrap(),
                &reason,
            ));
        }
        assert_eq!(record.experiment.unwrap().variant, model);
        assert_eq!(record.sale.unwrap().digest, hash);
        let json = serde_json::to_string(&proof).unwrap();
        assert!(oxsum_core::verify_bundle(&json, &receipt.content_hash).unwrap());
        assert_eq!(
            oxsum_verify::verify_charge(&description),
            oxsum_verify::ChargeCheck::Recomputed
        );
        let mut changed: serde_json::Value = serde_json::from_str(&json).unwrap();
        changed["entry"]["description"] =
            serde_json::Value::String(description.replace("flagged", "blocked"));
        assert!(!oxsum_core::verify_bundle(&changed.to_string(), &receipt.content_hash).unwrap());
    }
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(wallet.reserved().await.unwrap(), 0);
}

#[tokio::test]
async fn maximum_unicode_receipt_reads_back_and_oversize_leaves_no_entry() {
    let Some(wallet) = wallet().await else { return };
    let description = "🧾".repeat(MAX_DESCRIPTION_LEN);
    assert_eq!(description.len(), 65_536);
    let receipt = wallet
        .usage_receipt("unicode-bound", &description, None, None, DAY)
        .await
        .unwrap();
    let proof = wallet
        .receipt_proof(receipt.entry_id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(proof.entry.description().as_str(), description);
    assert!(
        oxsum_core::verify_bundle(
            &serde_json::to_string(&proof).unwrap(),
            &receipt.content_hash
        )
        .unwrap()
    );
    let size = wallet.log_size().await.unwrap();
    assert!(
        wallet
            .usage_receipt("oversize", &(description + "x"), None, None, DAY)
            .await
            .is_err()
    );
    assert_eq!(wallet.log_size().await.unwrap(), size);
    assert_eq!(wallet.available().await.unwrap(), 0);
}
