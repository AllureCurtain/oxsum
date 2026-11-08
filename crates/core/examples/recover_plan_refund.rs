//! Explicit local plan-refund recovery; previews by default and never calls a PSP.
use std::path::PathBuf;

use oxsum_core::{Db, Wallet};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let mut refund = None;
    let mut session_file = None;
    let mut apply = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--refund" => {
                refund = Some(
                    arguments
                        .next()
                        .ok_or("--refund needs a UUID")?
                        .parse::<Uuid>()?,
                )
            }
            "--session-file" => {
                session_file = Some(PathBuf::from(
                    arguments.next().ok_or("--session-file needs a path")?,
                ))
            }
            "--apply" => apply = true,
            _ => {
                return Err(
                    "usage: recover_plan_refund --refund UUID --session-file PATH [--apply]".into(),
                );
            }
        }
    }
    let refund = refund.ok_or("--refund is required")?;
    let session = std::fs::read_to_string(session_file.ok_or("--session-file is required")?)?;
    // No dotenv discovery, migration or schema reset. The operator names the DB.
    let db = Db::connect(&std::env::var("DATABASE_URL")?, 4).await?;
    let tenant:Option<String>=sqlx::query_scalar("SELECT o.tenant_id FROM oxsum.refunds r JOIN oxsum.organizations o USING(organization_id) WHERE r.refund_id=$1")
        .bind(refund).fetch_optional(db.pool()).await?;
    let wallet =
        Wallet::open_existing(db.pool().clone(), &tenant.ok_or("refund not found")?).await?;
    let report = db
        .recover_plan_refund(
            session.trim(),
            &wallet,
            refund,
            apply,
            time::OffsetDateTime::now_utc().date(),
        )
        .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
