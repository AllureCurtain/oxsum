//! Organization tiers and pricing discounts (issue #158): the storage contract
//! P7-1's admin endpoints and the gateway's admission and settlement paths
//! stand on. The tests need DATABASE_URL and skip without it, like the other
//! core suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, NewDiscount, NewUser, WalletError};
use time::OffsetDateTime;
use uuid::Uuid;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! db_or_skip {
    () => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// A migrated database and one registered organization.
async fn world(url: &str) -> (Db, Uuid) {
    let pool: sqlx::PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let email = format!(
        "terms_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let registration = db
        .register(NewUser {
            email,
            password: "a long enough password".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    (db, registration.organization.id)
}

fn discount(percent: i32) -> NewDiscount {
    NewDiscount {
        percent,
        organization_id: None,
        model: None,
        label: None,
        valid_from: None,
        valid_until: None,
    }
}

#[tokio::test]
async fn a_tier_writes_and_reads_back() {
    let (db, org) = world(&db_or_skip!()).await;

    let name = format!("t-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let tier = db
        .set_tier(&name, Some(60), Some(vec!["ds-chat".to_owned()]))
        .await
        .unwrap();
    assert_eq!(tier.name, name);
    assert_eq!(tier.requests_per_minute, Some(60));
    assert_eq!(
        tier.model_allowlist.as_deref(),
        Some(&["ds-chat".to_owned()][..])
    );
    assert_eq!(tier.organizations, 0);

    // PUT replaces the whole package: an absent field clears rather than keeps.
    let replaced = db.set_tier(&name, Some(30), None).await.unwrap();
    assert_eq!(replaced.requests_per_minute, Some(30));
    assert_eq!(replaced.model_allowlist, None);

    db.set_organization_tier(org, Some(&name)).await.unwrap();
    let profile = db.tier_of(org).await.unwrap().expect("the org has a tier");
    assert_eq!(profile.name, name);
    assert_eq!(profile.organizations, 1);

    // The list carries the count too.
    let listed = db.tiers().await.unwrap();
    let listed = listed.iter().find(|t| t.name == name).unwrap();
    assert_eq!(listed.organizations, 1);
}

#[tokio::test]
async fn a_tier_refuses_malformed_writes() {
    let (db, _org) = world(&db_or_skip!()).await;

    for bad in ["", "UPPER", "-lead", "trail-", "double--dash", "with space"] {
        assert!(
            matches!(
                db.set_tier(bad, None, None).await,
                Err(WalletError::InvalidInput(_))
            ),
            "{bad:?} is not a tier name"
        );
    }
    assert!(matches!(
        db.set_tier("fine-name", Some(0), None).await,
        Err(WalletError::InvalidInput(_))
    ));
    assert!(matches!(
        db.set_tier("fine-name-2", None, Some(vec![])).await,
        Err(WalletError::InvalidInput(_))
    ));
    assert!(matches!(
        db.set_tier("fine-name-3", None, Some(vec!["  ".to_owned()]))
            .await,
        Err(WalletError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn an_assigned_tier_cannot_be_deleted_and_clears_on_unassign() {
    let (db, org) = world(&db_or_skip!()).await;

    let name = format!("t-{}", &Uuid::new_v4().simple().to_string()[..8]);
    db.set_tier(&name, None, None).await.unwrap();
    db.set_organization_tier(org, Some(&name)).await.unwrap();

    // In use: the RESTRICT refuses the delete.
    assert!(matches!(
        db.delete_tier(&name).await,
        Err(WalletError::Conflict(_))
    ));

    // Cleared: the delete lands, and the organization is unconstrained again.
    db.set_organization_tier(org, None).await.unwrap();
    assert!(db.tier_of(org).await.unwrap().is_none());
    assert_eq!(db.delete_tier(&name).await.unwrap(), Some(()));

    // Unknown names: delete is not-found, assignment is a validation error.
    assert_eq!(db.delete_tier(&name).await.unwrap(), None);
    assert!(matches!(
        db.set_organization_tier(org, Some(&name)).await,
        Err(WalletError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn a_discount_replays_its_idempotency_key() {
    let (db, org) = world(&db_or_skip!()).await;

    let request = NewDiscount {
        organization_id: Some(org),
        model: Some("ds-chat".to_owned()),
        label: Some("pilot".to_owned()),
        ..discount(20)
    };
    let key = format!("terms-{}", Uuid::new_v4());
    let created = db.create_discount(&key, &request).await.unwrap();

    // Same key, same fields: the row it created is the answer.
    let replayed = db.create_discount(&key, &request).await.unwrap();
    assert_eq!(replayed.discount_id, created.discount_id);

    // Same key, different fields: a conflict, not a second row.
    let different = NewDiscount {
        percent: 30,
        ..discount(30)
    };
    assert!(matches!(
        db.create_discount(&key, &different).await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(
        db.discounts()
            .await
            .unwrap()
            .iter()
            .filter(|d| d.discount_id == created.discount_id)
            .count(),
        1
    );
}

#[tokio::test]
async fn the_most_favorable_in_window_discount_applies() {
    let (db, org) = world(&db_or_skip!()).await;
    let model = format!("m-{}", &Uuid::new_v4().simple().to_string()[..8]);

    // Scoped to this org and model.
    db.create_discount(
        &format!("terms-{}", Uuid::new_v4()),
        &NewDiscount {
            organization_id: Some(org),
            model: Some(model.clone()),
            ..discount(10)
        },
    )
    .await
    .unwrap();
    // Platform-wide, more favorable.
    db.create_discount(
        &format!("terms-{}", Uuid::new_v4()),
        &NewDiscount {
            model: Some(model.clone()),
            ..discount(25)
        },
    )
    .await
    .unwrap();
    // Expired: never applies.
    let past = OffsetDateTime::now_utc() - time::Duration::hours(2);
    db.create_discount(
        &format!("terms-{}", Uuid::new_v4()),
        &NewDiscount {
            model: Some(model.clone()),
            valid_from: Some(past),
            valid_until: Some(past + time::Duration::hours(1)),
            ..discount(90)
        },
    )
    .await
    .unwrap();
    // Scoped to another organization: does not leak.
    let other = db
        .register(NewUser {
            email: format!(
                "terms_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "a long enough password".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers")
        .organization
        .id;
    db.create_discount(
        &format!("terms-{}", Uuid::new_v4()),
        &NewDiscount {
            organization_id: Some(other),
            model: Some(model.clone()),
            ..discount(99)
        },
    )
    .await
    .unwrap();

    assert_eq!(
        db.discount_percent(org, &model).await.unwrap(),
        Some(25),
        "the single most favorable applicable row wins — they never stack"
    );
    assert_eq!(
        db.discount_percent(org, "a-model-nobody-discounts")
            .await
            .unwrap(),
        None
    );
    assert_eq!(db.discount_percent(other, &model).await.unwrap(), Some(99));
}

#[tokio::test]
async fn ending_a_discount_stops_it_applying() {
    let (db, org) = world(&db_or_skip!()).await;
    let model = format!("m-{}", &Uuid::new_v4().simple().to_string()[..8]);

    let created = db
        .create_discount(
            &format!("terms-{}", Uuid::new_v4()),
            &NewDiscount {
                organization_id: Some(org),
                model: Some(model.clone()),
                ..discount(40)
            },
        )
        .await
        .unwrap();
    assert_eq!(db.discount_percent(org, &model).await.unwrap(), Some(40));

    let ended = db
        .end_discount(created.discount_id)
        .await
        .unwrap()
        .expect("the row exists");
    assert!(ended.valid_until.is_some());
    assert_eq!(db.discount_percent(org, &model).await.unwrap(), None);

    // A retried end is the same state; an unknown id is not-found.
    let again = db.end_discount(created.discount_id).await.unwrap().unwrap();
    assert_eq!(again.discount_id, created.discount_id);
    assert!(db.end_discount(Uuid::new_v4()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_discount_validates_its_fields() {
    let (db, _org) = world(&db_or_skip!()).await;

    assert!(matches!(
        db.create_discount(&format!("terms-{}", Uuid::new_v4()), &discount(0))
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    assert!(matches!(
        db.create_discount(&format!("terms-{}", Uuid::new_v4()), &discount(101))
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    // A window that ends before it starts.
    let now = OffsetDateTime::now_utc();
    assert!(matches!(
        db.create_discount(
            &format!("terms-{}", Uuid::new_v4()),
            &NewDiscount {
                valid_from: Some(now),
                valid_until: Some(now - time::Duration::hours(1)),
                ..discount(10)
            },
        )
        .await,
        Err(WalletError::InvalidInput(_))
    ));
    // An organization that does not exist.
    assert!(matches!(
        db.create_discount(
            &format!("terms-{}", Uuid::new_v4()),
            &NewDiscount {
                organization_id: Some(Uuid::new_v4()),
                ..discount(10)
            },
        )
        .await,
        Err(WalletError::InvalidInput(_))
    ));
    // A missing or oversized key.
    assert!(matches!(
        db.create_discount("", &discount(10)).await,
        Err(WalletError::InvalidInput(_))
    ));
}
