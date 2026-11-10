//! A03/A04: existing invitation links survive the additive consent migration.
use oxsum_core::{Db, NewUser, Role, WalletError};
use sha2::{Digest, Sha256};
use sqlx::Executor;
use uuid::Uuid;
#[tokio::test]
async fn old_live_and_spent_invitations_preserve_identity_after_upgrade() {
    let Some(url) = oxsum_testkit::suite_db_url("invitation_upgrade") else {
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.pool().execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations (version text PRIMARY KEY,name text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0076" {
            continue;
        }
        let mut tx = db.pool().begin().await.unwrap();
        tx.execute("SET LOCAL search_path = ''").await.unwrap();
        tx.execute(std::fs::read_to_string(&file).unwrap().as_str())
            .await
            .unwrap();
        sqlx::query("INSERT INTO oxsum._migrations (version,name) VALUES ($1,$1)")
            .bind(version)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let password = "legacy invitation fixture password";
    let owner = db
        .register(NewUser {
            email: "legacy-owner@example.com".into(),
            password: password.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let secret = "oxi-legacy-valid-fixture";
    let spent = "oxi-legacy-spent-fixture";
    for (token, accepted) in [(secret, false), (spent, true)] {
        sqlx::query("INSERT INTO oxsum.invitations(invitation_id,organization_id,invited_by,token_hash,expires_at,accepted_at,accepted_by) VALUES ($1,$2,$3,$4,now()+interval '1 day',CASE WHEN $5 THEN now() ELSE NULL END,CASE WHEN $5 THEN $3 ELSE NULL END)")
            .bind(Uuid::new_v4()).bind(owner.organization.id).bind(owner.user.id).bind(Sha256::digest(token.as_bytes()).to_vec()).bind(accepted).execute(db.pool()).await.unwrap();
    }
    let before:Vec<(Uuid,Uuid,Vec<u8>)>=sqlx::query_as("SELECT invitation_id,organization_id,token_hash FROM oxsum.invitations ORDER BY invitation_id").fetch_all(db.pool()).await.unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let after:Vec<(Uuid,Uuid,Vec<u8>)>=sqlx::query_as("SELECT invitation_id,organization_id,token_hash FROM oxsum.invitations ORDER BY invitation_id").fetch_all(db.pool()).await.unwrap();
    assert_eq!(before, after);
    let invitee = db
        .redeem_invitation(
            secret,
            NewUser {
                email: "legacy-member@example.com".into(),
                password: password.into(),
                organization_name: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(invitee.organization.id, owner.organization.id);
    assert_eq!(invitee.organization.tenant_id, owner.organization.tenant_id);
    assert_eq!(
        db.members(owner.organization.id).await.unwrap()[1].role,
        Role::Member
    );
    assert!(matches!(
        db.invitation_preview(spent).await,
        Err(WalletError::NotFound(_))
    ));
    assert_eq!(
        db.authenticate(&owner.api_key.secret)
            .await
            .unwrap()
            .unwrap()
            .0
            .id,
        owner.organization.id
    );
}
