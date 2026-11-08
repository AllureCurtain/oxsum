//! Explicit local correction of duplicated historical plan usage; preview by default.

use std::path::PathBuf;

use oxsum_core::{Db, PlanUsageCorrectionRequest, Wallet};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let mut journal = None;
    let mut session_file = None;
    let mut reason = None;
    let mut key = None;
    let mut apply = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--journal" => journal=Some(arguments.next().ok_or("--journal needs a UUID")?.parse::<Uuid>()?),
            "--session-file" => session_file=Some(PathBuf::from(arguments.next().ok_or("--session-file needs a path")?)),
            "--reason" => reason=Some(arguments.next().ok_or("--reason needs text")?),
            "--key" => key=Some(arguments.next().ok_or("--key needs a value")?),
            "--apply" => apply=true,
            _ => return Err("usage: correct_plan_usage --journal UUID --session-file PATH --reason TEXT --key KEY [--apply]".into()),
        }
    }
    let request = PlanUsageCorrectionRequest {
        journal_id: journal.ok_or("--journal is required")?,
        reason: reason.ok_or("--reason is required")?,
        idempotency_key: key.ok_or("--key is required")?,
    };
    let session = std::fs::read_to_string(session_file.ok_or("--session-file is required")?)?;
    // No dotenv discovery, migrations, scans or lazy wallet initialization.
    let db = Db::connect(&std::env::var("DATABASE_URL")?, 4).await?;
    let tenant:Option<String>=sqlx::query_scalar("SELECT o.tenant_id FROM oxsum.invoice_journals j JOIN oxsum.organizations o USING(organization_id) WHERE j.journal_id=$1")
        .bind(request.journal_id).fetch_optional(db.pool()).await?;
    let wallet =
        Wallet::open_existing(db.pool().clone(), &tenant.ok_or("journal not found")?).await?;
    let report = db
        .correct_legacy_plan_usage(
            session.trim(),
            &wallet,
            &request,
            apply,
            time::OffsetDateTime::now_utc().date(),
        )
        .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.status == "refused" {
        std::process::exit(2);
    }
    Ok(())
}
