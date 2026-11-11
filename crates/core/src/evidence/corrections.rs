//! Bounded immutable invoice correction context and official entry references.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{WalletError, error};

/// Include the immediate predecessor and exact original lines in the statement's
/// captured transaction. Already frozen bundles keep their original bytes.
pub(super) async fn capture_in(
    tx: &mut Transaction<'_, Postgres>,
    journal: Uuid,
    bounded: bool,
    entries: &mut Vec<Uuid>,
) -> Result<Option<serde_json::Value>, WalletError> {
    // Bound transfer before constructing nested JSON. The portal also has a
    // finite limit; application consumers retain the existing tighter limits.
    let limit = if bounded { 256i64 } else { 10_000 };
    let mut bytes = 0i64;
    for query in [
        "SELECT octet_length(to_jsonb(c)::text)::bigint FROM oxsum.invoice_corrections c WHERE c.journal_id=$1",
        "SELECT octet_length(to_jsonb(g)::text)::bigint FROM oxsum.invoice_accounting_generations g WHERE g.journal_id=$1",
        "SELECT octet_length(to_jsonb(j)::text)::bigint FROM oxsum.invoice_journals j JOIN oxsum.invoice_corrections c ON c.predecessor_journal_id=j.journal_id WHERE c.journal_id=$1",
        "SELECT octet_length(to_jsonb(s)::text)::bigint FROM oxsum.plan_statements s JOIN oxsum.invoice_corrections c ON c.predecessor_statement_id=s.statement_id WHERE c.journal_id=$1",
        "SELECT octet_length(to_jsonb(l)::text)::bigint FROM oxsum.invoice_correction_lines l JOIN oxsum.invoice_corrections c USING(correction_id) WHERE c.journal_id=$1 LIMIT $2",
        "SELECT octet_length(to_jsonb(l)::text)::bigint FROM oxsum.invoice_journal_lines l JOIN oxsum.invoice_corrections c ON c.predecessor_journal_id=l.journal_id WHERE c.journal_id=$1 LIMIT $2",
    ] {
        let q = sqlx::query_scalar::<_, i64>(query).bind(journal);
        let sizes = if query.contains("$2") {
            q.bind(limit + 1)
        } else {
            q
        }
        .fetch_all(&mut **tx)
        .await?;
        bytes = sizes
            .iter()
            .try_fold(bytes, |total, size| total.checked_add(*size))
            .ok_or_else(|| error::invalid("invoice correction evidence size overflows"))?;
        if sizes.len() as i64 > limit || bytes > 4 * 1024 * 1024 {
            return Err(error::invalid(
                "invoice correction evidence exceeds bounded capture",
            ));
        }
    }
    let context: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('receipt',to_jsonb(c)-'actor_user_id'-'idempotency_key'-'request','generation',to_jsonb(g),'predecessor',to_jsonb(j),'predecessorStatement',to_jsonb(s),'lineSources',(SELECT jsonb_agg(to_jsonb(l) ORDER BY l.line_id) FROM oxsum.invoice_correction_lines l WHERE l.correction_id=c.correction_id),'predecessorLines',(SELECT jsonb_agg(to_jsonb(l) ORDER BY l.created_at,l.line_id) FROM oxsum.invoice_journal_lines l WHERE l.journal_id=c.predecessor_journal_id)) FROM oxsum.invoice_corrections c JOIN oxsum.invoice_accounting_generations g USING(correction_id) JOIN oxsum.invoice_journals j ON j.journal_id=c.predecessor_journal_id JOIN oxsum.plan_statements s ON s.statement_id=c.predecessor_statement_id WHERE c.journal_id=$1")
        .bind(journal).fetch_optional(&mut **tx).await?;
    let Some(mut context) = context else {
        return Ok(None);
    };
    let predecessor = context["predecessor"]["journal_id"]
        .as_str()
        .ok_or_else(|| error::invalid("correction predecessor is missing"))?;
    let void_key = format!("plan-journal/{predecessor}/void");
    let void_id = *crate::entry_id_for(&void_key).as_uuid();
    // A zero net total can still have gross legacy reversal postings. Resolve
    // the actual official entry instead of inferring its existence from the net.
    let tenant: String = sqlx::query_scalar("SELECT o.tenant_id FROM oxsum.invoice_journals j JOIN oxsum.organizations o USING(organization_id) WHERE j.journal_id=$1")
        .bind(journal).fetch_one(&mut **tx).await?;
    crate::wallet::validate_tenant_id(&tenant)?;
    let exists: bool = sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ledger_{tenant}.entries WHERE entry_id=$1 AND log_index IS NOT NULL)"))
        .bind(void_id).fetch_one(&mut **tx).await?;
    if !exists && context["predecessor"]["total_minor"].as_i64().unwrap_or(0) > 0 {
        return Err(error::invalid(
            "correction predecessor void entry is missing",
        ));
    }
    let void_id = exists.then_some(void_id);
    if let Some(id) = void_id {
        entries.push(id);
    }
    if let Some(id) = context["receipt"]["entry_id"].as_str() {
        entries.push(Uuid::parse_str(id).map_err(error::invalid)?);
    }
    context["voidEntryId"] = serde_json::json!(void_id);
    context["scope"] = serde_json::json!(
        "Retained immediate correction sources and referenced official entries. This proves inclusion and source integrity; earlier generations, later payments and independent accounting-policy compliance require their own evidence."
    );
    Ok(Some(context))
}
