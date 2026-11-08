//! Exact local non-plan refund preview/apply without dotenv, migration or PSP I/O.

use std::path::PathBuf;

use oxsum_core::{Db, NonplanRefundRecoveryRequest, Wallet};
use uuid::Uuid;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("non-plan refund recovery failed: {error}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut refund = None;
    let mut receipt = None;
    let mut session_file = None;
    let mut reason = None;
    let mut key = None;
    let mut expected = None;
    let mut apply = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--refund"=>refund=Some(args.next().ok_or("--refund needs a UUID")?.parse::<Uuid>()?),
            "--receipt"=>receipt=Some(args.next().ok_or("--receipt needs a UUID")?.parse::<Uuid>()?),
            "--session-file"=>session_file=Some(PathBuf::from(args.next().ok_or("--session-file needs a path")?)),
            "--reason"=>reason=Some(args.next().ok_or("--reason needs text")?),
            "--key"=>key=Some(args.next().ok_or("--key needs a value")?),
            "--expected-revision"=>expected=Some(args.next().ok_or("--expected-revision needs the preview SHA256")?),
            "--apply"=>apply=true,
            _=>return Err("usage: recover_nonplan_refund --refund UUID --receipt UUID --session-file PATH --reason TEXT --key KEY [--apply --expected-revision SHA256]".into()),
        }
    }
    if apply && expected.is_none() {
        return Err("--apply requires --expected-revision from an inspected preview".into());
    }
    let request = NonplanRefundRecoveryRequest {
        refund_id: refund.ok_or("--refund is required")?,
        event_receipt_id: receipt.ok_or("--receipt is required")?,
        idempotency_key: key.ok_or("--key is required")?,
        reason: reason.ok_or("--reason is required")?,
        expected_revision: expected,
    };
    let session = std::fs::read_to_string(session_file.ok_or("--session-file is required")?)?;
    // No dotenv discovery, migrations, scans or lazy wallet creation.
    let db = Db::connect(
        &std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL must explicitly name the reviewed installation")?,
        4,
    )
    .await?;
    let tenant:Option<String>=sqlx::query_scalar("SELECT o.tenant_id FROM oxsum.refunds r JOIN oxsum.organizations o USING(organization_id) WHERE r.refund_id=$1")
        .bind(request.refund_id).fetch_optional(db.pool()).await?;
    let wallet =
        Wallet::open_existing(db.pool().clone(), &tenant.ok_or("refund not found")?).await?;
    let report = db
        .recover_nonplan_refund(
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
