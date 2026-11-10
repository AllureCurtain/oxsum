//! Isolated statement read fixtures; synthetic rows test visibility and size only.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
#[path = "integration.rs"]
pub mod setup;
use oxsum_core::{ActingService, Db, IntegrationScope};
use uuid::Uuid;

pub struct World {
    pub db: Db,
    pub owner: String,
    pub app: Uuid,
    pub org: Uuid,
    pub token: String,
    pub actor: ActingService,
}
pub async fn credential(
    db: &Db,
    app: Uuid,
    scopes: Vec<IntegrationScope>,
) -> (String, ActingService) {
    let mut request = setup::request(vec![], scopes);
    request.application_id = Some(app);
    let token = db
        .create_integration_credential(&setup::owner(db).await, &request)
        .await
        .unwrap()
        .secret
        .unwrap();
    let actor = db.authenticate_service(&token).await.unwrap().unwrap();
    (token, actor)
}
pub async fn world(slug: &str) -> Option<World> {
    let db = Db::connect(&oxsum_testkit::suite_db_url(slug)?, 8)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                key: Uuid::new_v4().simple().to_string(),
                name: "Statement reader".into(),
                customer_limit: 10,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let (token, actor) = credential(
        &db,
        app,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::BillingRead,
            IntegrationScope::CustomersLifecycleWrite,
        ],
    )
    .await;
    let org = db
        .provision_application_customer(
            &actor,
            &oxsum_core::ProvisionCustomerRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                external_id: "reader".into(),
                name: "Reader".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    Some(World {
        db,
        owner,
        app,
        org,
        token,
        actor,
    })
}
/// Synthetic retained document; this does not assert ledger/payment correctness.
pub async fn statement(w: &World, state: &str, lines: i32, bytes: i32) -> Uuid {
    let statement = Uuid::new_v4();
    let business = w.db.default_business().await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_statements(statement_id,organization_id,business_id,state,total_minor,currency,issued_at) VALUES($1,$2,$3,$4,$5,'USD',now())")
        .bind(statement).bind(w.org).bind(business).bind(state).bind(i64::from(lines)).execute(w.db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) SELECT gen_random_uuid(),$1,'usage',1,repeat('x',$3),gen_random_uuid() FROM generate_series(1,$2)")
        .bind(statement).bind(lines).bind(bytes).execute(w.db.pool()).await.unwrap();
    statement
}
