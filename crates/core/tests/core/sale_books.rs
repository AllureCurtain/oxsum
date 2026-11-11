//! A09: sale authority, immutable terms, exact money and unambiguous scheduling.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{
    BillLine, BusinessRole, BusinessStaffRequest, BusinessStaffRevokeRequest, Db, ItemizedCharge,
    NewUser, Price, ProviderChange, ProviderConnection, ProviderPriceInput, SaleChange,
    SaleChangeRequest, SaleModelMap, SaleMutationResult, SalePreviewRequest, SalePreviewSource,
    SalePreviewUsage, SalePriceSnapshot, SaleTerms, SecretKey, SessionContext,
    SessionContextRequest, UsageRecord, WalletError, price_row,
};
use serde_json::json;
use sqlx::Executor;
use std::collections::BTreeMap;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const PASSWORD: &str = "sale fixture password";
async fn fixture() -> Option<(Db, String, Uuid)> {
    let url = oxsum_testkit::suite_db_url("sale_books")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    Some((db, owner, customer.organization.id))
}
fn terms(rate: i64, discount: i64) -> SaleTerms {
    serde_json::from_value(json!({"price":{"inputPricePerMillion":rate,"outputPricePerMillion":rate,"maxOutputTokens":100},"discountPercent":discount})).unwrap()
}
fn request(change: SaleChange) -> SaleChangeRequest {
    SaleChangeRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "sale fixture change".into(),
        change,
    }
}
async fn change(db: &Db, token: &str, change: SaleChange) -> SaleMutationResult {
    db.change_sale_terms(token, &request(change)).await.unwrap()
}
async fn publish(db: &Db, token: &str, models: SaleModelMap) -> Uuid {
    let book = change(
        db,
        token,
        SaleChange::CreateBook {
            name: Uuid::new_v4().to_string(),
        },
    )
    .await
    .resource_id;
    change(
        db,
        token,
        SaleChange::SaveDraft {
            book_id: book,
            expected_revision: 0,
            models,
        },
    )
    .await;
    change(
        db,
        token,
        SaleChange::PublishDraft {
            book_id: book,
            draft_revision: 1,
        },
    )
    .await;
    book
}
fn models(model: &str, rate: i64, discount: i64) -> SaleModelMap {
    BTreeMap::from([(model.into(), terms(rate, discount))])
}
async fn resolve(db: &Db, org: Uuid, model: &str, at: OffsetDateTime) -> SalePriceSnapshot {
    db.resolve_sale_price(
        db.business_ownership().await.unwrap().business.id,
        org,
        model,
        at,
    )
    .await
    .unwrap()
}
fn assigned(
    org: Uuid,
    book: Uuid,
    start: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
    replaces: Option<Uuid>,
) -> SaleChange {
    SaleChange::AssignBook {
        organization_id: org,
        book_id: book,
        version: 1,
        effective_from: start,
        effective_until: until,
        replaces,
    }
}
fn override_model(
    org: Uuid,
    model: &str,
    rate: i64,
    start: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
    replaces: Option<Uuid>,
) -> SaleChange {
    SaleChange::OverrideModel {
        organization_id: org,
        model: model.into(),
        terms: Box::new(terms(rate, 0)),
        effective_from: start,
        effective_until: until,
        replaces,
    }
}
async fn staff(db: &Db, owner: &str, role: BusinessRole) -> (String, Uuid) {
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.set_business_staff(
        owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "sale fixture role".into(),
            user_id: user.user.id,
            role,
        },
    )
    .await
    .unwrap();
    let token = db
        .login(&user.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token;
    db.switch_session_context(
        &token,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    (token, user.user.id)
}
#[tokio::test]
async fn precedence_pins_one_discount_and_exact_half_open_windows() {
    let _guard = SERIAL.lock().await;
    let Some((db, owner, org)) = fixture().await else {
        return;
    };
    let start = (OffsetDateTime::now_utc() + Duration::hours(1))
        .replace_nanosecond(123_456_789)
        .unwrap();
    let start_micro = start.replace_nanosecond(123_456_000).unwrap();
    let seam = start + Duration::hours(1);
    let until = seam + Duration::hours(1);
    let default = publish(
        &db,
        &owner,
        BTreeMap::from([
            ("common".into(), terms(1_000_000, 10)),
            ("default-only".into(), terms(4_000_000, 50)),
        ]),
    )
    .await;
    let default_schedule = change(
        &db,
        &owner,
        SaleChange::ScheduleDefault {
            book_id: default,
            version: 1,
            effective_from: Some(start),
            effective_until: Some(until),
            replaces: None,
        },
    )
    .await;
    let book = publish(&db, &owner, models("common", 2_000_000, 25)).await;
    let assignment = change(
        &db,
        &owner,
        assigned(org, book, Some(start), Some(until), None),
    )
    .await;
    change(
        &db,
        &owner,
        override_model(org, "common", 0, Some(seam), Some(until), None),
    )
    .await;
    let business = db.business_ownership().await.unwrap().business.id;
    assert!(matches!(
        db.resolve_sale_price(
            business,
            org,
            "common",
            start_micro - Duration::microseconds(1)
        )
        .await,
        Err(WalletError::NotFound(_))
    ));
    let before = resolve(&db, org, "common", seam - Duration::microseconds(1)).await;
    assert_eq!(before.source, "assigned");
    assert_eq!(before.schedule_id, Some(assignment.resource_id));
    assert_eq!(before.effective_from, start_micro);
    assert_eq!(
        before
            .preview(&UsageRecord::tokens(1, 0).unwrap())
            .unwrap()
            .charge_minor,
        2
    );
    assert_eq!(before.terms.discount_percent, 25);
    let fallback = resolve(&db, org, "default-only", start).await;
    assert_eq!(fallback.source, "default");
    assert_eq!(fallback.schedule_id, Some(default_schedule.resource_id));
    assert_eq!(
        fallback
            .preview(&UsageRecord::tokens(1, 0).unwrap())
            .unwrap()
            .charge_minor,
        2
    );
    let at = resolve(&db, org, "common", seam).await;
    assert_eq!(at.source, "override");
    assert_eq!(
        at.preview(&UsageRecord::tokens(10, 10).unwrap())
            .unwrap()
            .charge_minor,
        0
    );
    assert!(matches!(
        db.resolve_sale_price(business, org, "common", until).await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.resolve_sale_price(business, Uuid::new_v4(), "common", start)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.resolve_sale_price(Uuid::new_v4(), org, "common", start)
            .await,
        Err(WalletError::NotFound(_))
    ));
}
#[tokio::test]
async fn draft_edits_publish_and_replacement_never_rewrite_a_pinned_snapshot() {
    let _guard = SERIAL.lock().await;
    let Some((db, owner, org)) = fixture().await else {
        return;
    };
    let book = publish(&db, &owner, models("model", 1_000_000, 0)).await;
    let start = OffsetDateTime::now_utc() + Duration::hours(3);
    let seam = start + Duration::hours(1);
    let first = change(&db, &owner, assigned(org, book, Some(start), None, None)).await;
    let pinned = resolve(&db, org, "model", start).await;
    change(
        &db,
        &owner,
        SaleChange::SaveDraft {
            book_id: book,
            expected_revision: 1,
            models: models("model", 3_000_000, 50),
        },
    )
    .await;
    assert!(matches!(
        db.change_sale_terms(
            &owner,
            &request(SaleChange::SaveDraft {
                book_id: book,
                expected_revision: 1,
                models: models("model", 9, 0)
            })
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    let unpublished = resolve(&db, org, "model", seam).await;
    assert_eq!(unpublished.terms, pinned.terms);
    let draft = db
        .preview_sale_price(
            &owner,
            &SalePreviewRequest {
                organization_id: org,
                model: "model".into(),
                at: Some(start),
                source: SalePreviewSource::Draft {
                    book_id: book,
                    revision: 2,
                },
                usage: SalePreviewUsage::try_new(UsageRecord::tokens(2, 0).unwrap()).unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(draft.snapshot.source, "draft");
    assert_eq!(draft.charge_minor, 3);
    change(
        &db,
        &owner,
        SaleChange::PublishDraft {
            book_id: book,
            draft_revision: 2,
        },
    )
    .await;
    let replacement = SaleChange::AssignBook {
        organization_id: org,
        book_id: book,
        version: 2,
        effective_from: Some(seam),
        effective_until: None,
        replaces: Some(first.resource_id),
    };
    change(&db, &owner, replacement).await;
    assert_eq!(resolve(&db, org, "model", seam).await.version, Some(2));
    assert_eq!(
        resolve(&db, org, "model", seam - Duration::microseconds(1))
            .await
            .version,
        Some(1)
    );
    assert_eq!(
        pinned
            .preview(&UsageRecord::tokens(2, 0).unwrap())
            .unwrap()
            .charge_minor,
        2
    );
    let history = db.sale_book_history(&owner, book).await.unwrap();
    assert_eq!(history.drafts.len(), 2);
    assert_eq!(history.versions.len(), 2);
    let original = history
        .schedules
        .iter()
        .find(|s| s.id == first.resource_id)
        .unwrap();
    assert!(original.scheduled_until.is_none());
    assert!(original.effective_until.is_some());
    for sql in [
        "UPDATE oxsum.sale_drafts SET models='{}' WHERE book_id=$1",
        "UPDATE oxsum.sale_versions SET version=99 WHERE book_id=$1",
        "DELETE FROM oxsum.sale_schedules WHERE book_id=$1",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(book)
                .execute(db.pool())
                .await
                .is_err()
        );
    }
    assert!(
        sqlx::query("DELETE FROM oxsum.sale_schedule_ends WHERE schedule_id=$1")
            .bind(first.resource_id)
            .execute(db.pool())
            .await
            .is_err()
    );
    let encoded = serde_json::to_value(&pinned).unwrap();
    let restored: SalePriceSnapshot = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored, pinned);
}
#[tokio::test]
async fn concurrent_overlaps_cancel_and_replacement_rollback_are_deterministic() {
    let _guard = SERIAL.lock().await;
    let Some((db, owner, org)) = fixture().await else {
        return;
    };
    let book = publish(&db, &owner, models("model", 1, 0)).await;
    let start = OffsetDateTime::now_utc() + Duration::hours(6);
    let seam = start + Duration::hours(1);
    let left = request(assigned(org, book, Some(start), Some(seam), None));
    let right = request(assigned(org, book, Some(start), Some(seam), None));
    let (a, b) = tokio::join!(
        db.change_sale_terms(&owner, &left),
        db.change_sale_terms(&owner, &right)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let first = a.or(b).unwrap();
    let future = change(&db, &owner, assigned(org, book, Some(seam), None, None)).await;
    // Replacement starts inside first, but collides with future: its appended end rolls back too.
    assert!(matches!(
        db.change_sale_terms(
            &owner,
            &request(assigned(
                org,
                book,
                Some(start + Duration::minutes(30)),
                None,
                Some(first.resource_id)
            ))
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    let ends: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.sale_schedule_ends WHERE schedule_id=$1")
            .bind(first.resource_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(ends, 0);
    change(
        &db,
        &owner,
        SaleChange::EndSchedule {
            schedule_id: future.resource_id,
            effective_until: seam,
        },
    )
    .await;
    change(&db, &owner, assigned(org, book, Some(seam), None, None)).await;
    assert!(matches!(
        db.change_sale_terms(
            &owner,
            &request(SaleChange::EndSchedule {
                schedule_id: future.resource_id,
                effective_until: seam
            })
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    // A start a few seconds back is the caller's own "now" inside the skew
    // tolerance (#1053); a clearly backdated window is still refused.
    assert!(matches!(
        db.change_sale_terms(
            &owner,
            &request(assigned(
                org,
                book,
                Some(OffsetDateTime::now_utc() - Duration::minutes(1)),
                None,
                None
            ))
        )
        .await,
        Err(WalletError::InvalidInput(_))
    ));
    assert!(matches!(
        db.change_sale_terms(
            &owner,
            &request(override_model(
                org,
                "model",
                2,
                Some(seam),
                None,
                Some(first.resource_id)
            ))
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    let empty = override_model(
        org,
        "empty",
        0,
        Some(start),
        Some(start + Duration::nanoseconds(1)),
        None,
    );
    assert!(matches!(
        db.change_sale_terms(&owner, &request(empty)).await,
        Err(WalletError::InvalidInput(_))
    ));
}
#[tokio::test]
async fn current_roles_replay_conflicts_and_audit_failures_preserve_authority() {
    let _guard = SERIAL.lock().await;
    let Some((db, owner, org)) = fixture().await else {
        return;
    };
    let (admin, admin_id) = staff(&db, &owner, BusinessRole::Admin).await;
    let (finance, _) = staff(&db, &owner, BusinessRole::Finance).await;
    let book = publish(&db, &admin, models("model", 10, 0)).await;
    let op = request(assigned(org, book, None, None, None));
    assert!(matches!(
        db.change_sale_terms(&admin, &op).await,
        Err(WalletError::Forbidden(_))
    ));
    let result = db.change_sale_terms(&finance, &op).await.unwrap();
    assert_eq!(db.change_sale_terms(&finance, &op).await.unwrap(), result);
    let mut changed = op.clone();
    changed.reason = "changed".into();
    assert!(matches!(
        db.change_sale_terms(&finance, &changed).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.change_sale_terms(
            &finance,
            &request(SaleChange::SaveDraft {
                book_id: book,
                expected_revision: 1,
                models: models("model", 20, 0)
            })
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(
        db.sale_book_history(&finance, book)
            .await
            .unwrap()
            .versions
            .len(),
        1
    );
    assert!(matches!(
        db.change_sale_terms(
            &admin,
            &request(SaleChange::EndSchedule {
                schedule_id: result.resource_id,
                effective_until: OffsetDateTime::now_utc() + Duration::hours(1)
            })
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    for role in [BusinessRole::Support, BusinessRole::Viewer] {
        let (token, _) = staff(&db, &owner, role).await;
        assert!(matches!(
            db.sale_books(&token, None, 100).await,
            Err(WalletError::Forbidden(_))
        ));
        assert!(matches!(
            db.change_sale_terms(&token, &op).await,
            Err(WalletError::Forbidden(_))
        ));
    }
    let replay = request(SaleChange::CreateBook {
        name: Uuid::new_v4().to_string(),
    });
    db.change_sale_terms(&admin, &replay).await.unwrap();
    db.revoke_business_staff(
        &owner,
        &BusinessStaffRevokeRequest {
            user_id: admin_id,
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "revoke sale admin".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.change_sale_terms(&admin, &replay).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.sale_book_history(&owner, Uuid::new_v4()).await,
        Err(WalletError::NotFound(_))
    ));
    db.pool().execute("CREATE FUNCTION oxsum.reject_sale_audit() RETURNS trigger AS $$ BEGIN IF NEW.action LIKE 'business.sale.%' THEN RAISE EXCEPTION 'injected sale audit failure'; END IF; RETURN NEW; END; $$ LANGUAGE plpgsql; CREATE TRIGGER fail_sale_audit BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_sale_audit()").await.unwrap();
    let rejected = request(SaleChange::SaveDraft {
        book_id: book,
        expected_revision: 1,
        models: models("model", 99, 0),
    });
    assert!(db.change_sale_terms(&owner, &rejected).await.is_err());
    assert_eq!(
        db.sale_book_history(&owner, book)
            .await
            .unwrap()
            .drafts
            .len(),
        1
    );
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.business_mutations WHERE idempotency_key=$1",
    )
    .bind(&rejected.idempotency_key)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(receipts, 0);
    db.pool()
        .execute("DROP TRIGGER fail_sale_audit ON oxsum.admin_audit")
        .await
        .unwrap();
    assert_eq!(
        db.change_sale_terms(&owner, &rejected)
            .await
            .unwrap()
            .revision,
        Some(2)
    );
}
#[tokio::test]
async fn explicit_legacy_snapshot_preserves_version_identity_and_removes_all_supplier_costs() {
    let _guard = SERIAL.lock().await;
    let Some((db, owner, org)) = fixture().await else {
        return;
    };
    let channel = Uuid::new_v4().to_string();
    let key = SecretKey::from_bytes([8; 32]);
    db.change_business_provider(
        &owner,
        &Uuid::new_v4().to_string(),
        "legacy fixture",
        &ProviderChange::Connection {
            config: ProviderConnection {
                name: channel.clone(),
                base_url: "https://sale-provider.example/v1".into(),
                protocol: "openai".into(),
                api_keys: Some(vec!["not-a-live-provider-key".into()]),
                destination_policy: None,
            },
        },
        &key,
    )
    .await
    .unwrap();
    let legacy:Price=serde_json::from_value(json!({"inputPricePerMillion":1_000_000,"outputPricePerMillion":0,"maxOutputTokens":100,"upstream":{"inputPricePerMillion":999},"rules":[{"match":{"serviceTier":"priority"},"price":{"inputPricePerMillion":3_000_000,"maxOutputTokens":100,"upstream":{"inputPricePerMillion":777}}}]})).unwrap();
    for rate in [
        legacy.clone(),
        serde_json::from_value(json!({"inputPricePerMillion":7_000_000,"maxOutputTokens":100}))
            .unwrap(),
    ] {
        db.change_business_provider(
            &owner,
            &Uuid::new_v4().to_string(),
            "legacy price",
            &ProviderChange::Price {
                channel: channel.clone(),
                config: Box::new(ProviderPriceInput {
                    model: "model".into(),
                    price: rate,
                    weight: 100,
                }),
            },
            &key,
        )
        .await
        .unwrap();
    }
    let preview = db
        .preview_sale_price(
            &owner,
            &SalePreviewRequest {
                organization_id: org,
                model: "model".into(),
                at: None,
                source: SalePreviewSource::Legacy {
                    channel: channel.clone(),
                    version: 1,
                },
                usage: SalePreviewUsage::try_new(UsageRecord {
                    input_tokens: 1,
                    service_tier: Some("priority".into()),
                    ..Default::default()
                })
                .unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(preview.charge_minor, 3);
    assert_eq!(preview.snapshot.version, Some(1));
    assert_eq!(
        preview.snapshot.legacy_channel.as_deref(),
        Some(channel.as_str())
    );
    assert!(
        !serde_json::to_string(&preview)
            .unwrap()
            .contains("upstream")
    );
    let original = price_row(db.pool(), &channel, "model", Some(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.price, legacy);
    let business = db.business_ownership().await.unwrap().business.id;
    assert!(matches!(
        db.resolve_sale_price(business, org, "model", OffsetDateTime::now_utc())
            .await,
        Err(WalletError::NotFound(_))
    ));
}

fn snapshot(terms: SaleTerms) -> SalePriceSnapshot {
    SalePriceSnapshot {
        business_id: Uuid::nil(),
        organization_id: Uuid::nil(),
        model: "model".into(),
        at: OffsetDateTime::UNIX_EPOCH,
        source: "override".into(),
        schedule_id: None,
        book_id: None,
        version: None,
        draft_revision: None,
        legacy_channel: None,
        effective_from: OffsetDateTime::UNIX_EPOCH,
        effective_until: None,
        terms,
    }
}
#[test]
fn conditional_coverage_rounding_discount_and_units_use_existing_pure_rating() {
    let conditional:SaleTerms=serde_json::from_value(json!({"price":{"inputPricePerMillion":100_000,"outputPricePerMillion":100_000,"maxOutputTokens":100,"cacheReadPricePerMillion":200_000,"cacheWrite5mPricePerMillion":300_000,"cacheWrite1hPricePerMillion":400_000,"reasoningPricePerMillion":500_000,"costPerRequest":2,"rules":[{"match":{"serviceTier":"priority"},"price":{"inputPricePerMillion":1_000_000,"outputPricePerMillion":1_000_000,"maxOutputTokens":100}},{"match":{"serviceTier":"priority","minInputTokens":10,"maxInputTokens":20},"price":{"inputPricePerMillion":2_000_000,"outputPricePerMillion":2_000_000,"maxOutputTokens":100}}]},"discountPercent":50})).unwrap();
    let snap = snapshot(conditional);
    let quote = snap
        .preview(&UsageRecord {
            input_tokens: 4,
            output_tokens: 2,
            cached_tokens: 1,
            cache_write_5m_tokens: 1,
            cache_write_1h_tokens: 1,
            reasoning_tokens: 1,
            ..Default::default()
        })
        .unwrap();
    // Raw sum 3.6 minor -> one 50% discount then ceiling = 2, not line rounding.
    assert_eq!(quote.undiscounted_minor, 4);
    assert_eq!(quote.charge_minor, 2);
    for (input, expected, min) in [
        (9, 5, None),
        (10, 10, Some(10)),
        (20, 20, Some(10)),
        (21, 11, None),
    ] {
        let quote = snap
            .preview(&UsageRecord {
                input_tokens: input,
                service_tier: Some("priority".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(quote.charge_minor, expected);
        assert_eq!(quote.matched_rule.unwrap().min_input_tokens, min);
    }
    let free = snapshot(terms(100_000, 100));
    assert_eq!(
        free.preview(&UsageRecord::tokens(4, 4).unwrap())
            .unwrap()
            .charge_minor,
        0
    );
    let fraction = snapshot(terms(100_000, 0));
    assert_eq!(
        fraction
            .preview(&UsageRecord::tokens(3, 3).unwrap())
            .unwrap()
            .charge_minor,
        1
    );
    let event: SaleTerms = serde_json::from_value(
        json!({"price":{"inputPricePerMillion":0,"mode":"event","costPerRequest":2}}),
    )
    .unwrap();
    assert_eq!(
        snapshot(event)
            .preview(&UsageRecord {
                event_type: Some("task".into()),
                ..Default::default()
            })
            .unwrap()
            .charge_minor,
        2
    );
}
#[test]
fn unknown_dimensions_cost_fields_ambiguous_rules_and_numeric_overflow_are_refused() {
    for usage in [
        json!({"inputTokens":1,"unknownTokens":100}),
        json!({"usageDetails":{"newDimension":100}}),
        json!({"inputTokens":i64::MAX,"cachedTokens":i64::MAX,"cacheWrite5mTokens":1}),
    ] {
        assert!(serde_json::from_value::<SalePreviewUsage>(usage).is_err());
    }
    for price in [
        json!({"inputPricePerMillion":1,"maxOutputTokens":10,"upstream":null}),
        json!({"inputPricePerMillion":1,"maxOutputTokens":10,"rules":[{"match":{"serviceTier":"x"},"price":{"inputPricePerMillion":1,"maxOutputTokens":10,"upstream":{}}}]}),
        json!({"inputPricePerMillion":1,"maxOutputTokens":10,"rules":[{"match":{"minInputTokens":1},"price":{"inputPricePerMillion":1,"maxOutputTokens":10}},{"match":{"maxInputTokens":10},"price":{"inputPricePerMillion":2,"maxOutputTokens":10}}]}),
    ] {
        assert!(serde_json::from_value::<SaleTerms>(json!({"price":price})).is_err());
    }
    let normal = snapshot(terms(10, 0));
    for usage in [
        UsageRecord {
            tool_calls: 1,
            ..Default::default()
        },
        UsageRecord {
            image_input_tokens: 1,
            ..Default::default()
        },
        UsageRecord {
            event_type: Some("unknown".into()),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            normal.preview(&usage),
            Err(WalletError::InvalidInput(_))
        ));
    }
    assert!(
        snapshot(terms(i64::MAX, 1))
            .preview(&UsageRecord::tokens(i64::MAX, i64::MAX).unwrap())
            .is_err()
    );
    let itemized = ItemizedCharge {
        lines: vec![
            BillLine {
                item: "input".into(),
                units: i64::MAX,
                price_per_m: i64::MAX
            };
            3
        ],
        matched_rule: None,
    };
    assert!(itemized.total_minor().is_err());
    assert!(itemized.discounted_minor(1).is_err());
    let large = ItemizedCharge {
        lines: vec![BillLine {
            item: "input".into(),
            units: i64::MAX,
            price_per_m: i64::MAX,
        }],
        matched_rule: None,
    };
    assert!(large.discounted_minor(1).is_err());
    assert!(terms(1, 101).validate().is_err());
    let json = serde_json::to_value(
        SalePreviewUsage::try_new(UsageRecord::tokens(1, 1).unwrap()).unwrap(),
    )
    .unwrap();
    assert!(serde_json::from_value::<SalePreviewUsage>(json).is_ok());
}
